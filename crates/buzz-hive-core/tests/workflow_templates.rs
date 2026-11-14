//! Stage templates and template variables.

use std::collections::BTreeMap;

use buzz_hive_core::workflow::decode::decode_stage;
use buzz_hive_core::workflow::load::parse;
use buzz_hive_core::workflow::names::{agent_name, check_param_value};
use buzz_hive_core::workflow::template::{
    artifact_names, draft_variables, known_variable, known_variables_text, param_variable,
    value_variable, variables,
};
use buzz_hive_core::workflow::templates::load_stage_templates;

const RESEARCH: &str = "id: research\nname: Research\nkind: prompt\nagent: builder\nprompt: Read {{issue.title}}\noutputs: [{ artifact: research }]\n";

fn files(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn stage_templates_sort_by_id() {
    let got = load_stage_templates(&files(&[
        (
            "wait.yaml",
            "id: wait\nkind: wait\nwait: { pull_request: merged }\n",
        ),
        ("research.yaml", RESEARCH),
        ("notes.txt", "ignored"),
    ]))
    .expect("loads");
    let ids: Vec<&str> = got.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["research", "wait"]);
}

#[test]
fn stage_templates_are_refused_with_the_file_named() {
    let cases = [
        ("unknown key", "id: a\nkind: prompt\nbogus: 1\n", "a.yaml"),
        (
            "id differs",
            "id: b\nkind: prompt\n",
            "its id is \"b\", want \"a\"",
        ),
        ("no id", "kind: prompt\n", "its id is \"\""),
        ("not yaml", "id: [\n", "a.yaml"),
    ];
    for (name, text, want) in cases {
        let err = load_stage_templates(&files(&[("a.yaml", text)])).expect_err(name);
        assert!(
            err.contains("stage template a.yaml") && err.contains(want),
            "{name}: {err}"
        );
    }
}

#[test]
fn a_stage_template_keeps_its_stage_as_json() {
    let templates = load_stage_templates(&files(&[("research.yaml", RESEARCH)])).unwrap();
    let json: serde_json::Value = serde_json::from_str(&templates[0].json).unwrap();
    assert_eq!(json["kind"], "prompt");
    assert_eq!(json["id"], "research");
    let (_, none, findings) = decode_stage("empty.yaml", "");
    assert!(none.is_none() || !findings.is_empty());
}

#[test]
fn param_variable_names() {
    for (input, want) in [
        ("params.depth", Some("depth")),
        ("params.max_files2", Some("max_files2")),
        ("params.", None),
        ("params.Depth", None),
        ("params.a.b", None),
        ("params.1a", None),
        ("params.a-b", None),
        ("param.depth", None),
        ("depth", None),
        ("stage.a.params.depth", None),
    ] {
        assert_eq!(param_variable(input), want, "{input}");
    }
}

#[test]
fn known_variable_does_not_accept_params() {
    assert!(!known_variable("params.depth"));
    assert!(!value_variable("params.depth"));
}

#[test]
fn the_known_variables_text_is_the_list_in_order() {
    assert_eq!(
        known_variables_text(),
        "issue.title, issue.body, issue.url, issue.number, issue.repo, branch, scratch, \
         artifacts.<name>.path, artifacts.<name>.file, inputs.<name>.file, inputs.<name>.path, \
         params.<name>, sendback.comments, stage.results, comments, repos or files"
    );
}

#[test]
fn value_and_known_variables() {
    for name in [
        "issue.number",
        "issue.url",
        "issue.repo",
        "branch",
        "artifacts.plan.path",
        "inputs.plan.path",
    ] {
        assert!(value_variable(name), "{name}");
    }
    for name in [
        "issue.title",
        "issue.body",
        "scratch",
        "sendback.comments",
        "stage.results",
        "comments",
        "params.x",
        "artifacts.plan.file",
        "inputs.plan.file",
    ] {
        assert!(!value_variable(name), "{name}");
    }
    assert!(!known_variable("params.x"));
    assert!(!known_variable("artifacts.<name>.path"));
    assert!(!known_variable("artifacts.<name>.file"));
    assert!(known_variable("comments"));
    assert!(known_variable("inputs.plan.file") && known_variable("inputs.plan.path"));
    assert!(!known_variable("inputs.<name>.file"));
    assert!(!known_variable("inputs.Bad Name.file"));
    assert!(!known_variable("inputs.plan.files"));
    assert!(known_variable("artifacts.questions.file"));
    assert!(!known_variable("artifacts.Bad Name.file"));
    assert!(!known_variable("artifacts.q.files"));
    assert!(
        !value_variable("artifacts.questions.file"),
        "an agent writes the file"
    );
}

#[test]
fn the_variables_a_draft_may_use() {
    let mut package_files = BTreeMap::new();
    package_files.insert(
        "kiln-package.yaml".to_owned(),
        "name: demo\nversion: 1\nparameters:\n  - { name: target, type: text, description: Where to deploy. }\n  - { name: n, type: text }\n".to_owned(),
    );
    package_files.insert(
        "workflow.yaml".to_owned(),
        r#"version: 2
stages:
  - { id: a, kind: prompt, agent: x, prompt: x, outputs: [{ artifact: plan }] }
  - { id: b, kind: prompt, agent: x, prompt: x, outputs: [{ artifact: plan }, { artifact: review }, { artifact: Bad Name }], inputs: [{ name: plan }, { name: Bad Name }] }
  - { id: c, kind: prompt, agent: x, prompt: x, inputs: [{ name: plan, required: true }, { name: notes }] }
"#
        .to_owned(),
    );
    let package = parse("test/demo", package_files).unwrap();
    let got = draft_variables(&package);
    let names: Vec<&str> = got.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "issue.title",
            "issue.body",
            "issue.url",
            "issue.number",
            "issue.repo",
            "branch",
            "scratch",
            "sendback.comments",
            "stage.results",
            "comments",
            "repos",
            "files",
            "params.target",
            "params.n",
            "artifacts.plan.path",
            "artifacts.plan.file",
            "artifacts.review.path",
            "artifacts.review.file",
            "inputs.plan.path",
            "inputs.plan.file",
            "inputs.notes.path",
            "inputs.notes.file",
        ]
    );
    let by_name: BTreeMap<&str, _> = got.iter().map(|v| (v.name.as_str(), v)).collect();
    for (name, description) in [
        ("params.target", "Where to deploy."),
        ("params.n", "A package parameter."),
        (
            "artifacts.plan.path",
            "Where the plan artifact is copied for this stage.",
        ),
        (
            "artifacts.review.path",
            "Where the review artifact is copied for this stage.",
        ),
        (
            "artifacts.plan.file",
            "The repo file the skill wrote for the plan artifact.",
        ),
        (
            "inputs.plan.path",
            "Where the plan input is copied for this stage.",
        ),
        ("inputs.plan.file", "The repo file of the plan input."),
    ] {
        assert_eq!(by_name[name].description, description, "{name}");
    }
    // An artifact path is an engine value; a parameter's text is a person's.
    for (name, value) in [
        ("artifacts.plan.path", true),
        ("artifacts.review.path", true),
        ("artifacts.plan.file", false),
        ("params.target", false),
        ("params.n", false),
        ("issue.title", false),
        ("issue.number", true),
        ("inputs.plan.path", true),
        ("inputs.plan.file", false),
    ] {
        assert_eq!(by_name[name].value, value, "{name}");
    }
    assert!(
        got.iter().all(|v| !v.pattern),
        "a draft never offers a pattern row"
    );
}

#[test]
fn a_draft_without_stages_or_parameters_offers_only_the_fixed_variables() {
    let mut package_files = BTreeMap::new();
    package_files.insert(
        "kiln-package.yaml".to_owned(),
        "name: demo\nversion: 1\n".to_owned(),
    );
    package_files.insert(
        "workflow.yaml".to_owned(),
        "version: 2\nstages: []\n".to_owned(),
    );
    let package = parse("test/demo", package_files).unwrap();
    let got = draft_variables(&package);
    let fixed = variables().iter().filter(|v| !v.pattern).count();
    assert_eq!(got.len(), fixed);
    assert!(got.iter().all(|v| !["params.", "artifacts.", "inputs."]
        .iter()
        .any(|p| v.name.starts_with(p))));
}

#[test]
fn artifact_names_come_from_the_prompt_text() {
    let mut package_files = BTreeMap::new();
    package_files.insert(
        "kiln-package.yaml".to_owned(),
        "name: demo\nversion: 1\n".to_owned(),
    );
    package_files.insert(
        "workflow.yaml".to_owned(),
        "version: 2\nstages: []\n".to_owned(),
    );
    package_files.insert(
        "prompts/a.md".to_owned(),
        "see {{artifacts.plan.file}} and {{artifacts.plan.path}}".to_owned(),
    );
    let package = parse("test/demo", package_files).unwrap();
    let stage = |text: &str| {
        decode_stage(
            "s.yaml",
            &format!("id: s\nkind: prompt\nagent: x\n{text}\n"),
        )
        .0
    };
    let cases: [(&str, &str, &[&str]); 5] = [
        (
            "prompt",
            "prompt: \"{{artifacts.b.file}} {{artifacts.a.file}} {{artifacts.b.file}}\"",
            &["b", "a"],
        ),
        (
            "command args",
            "command: x\nargs: \"{{ artifacts.q.file }}\"",
            &["q"],
        ),
        ("prompt file", "prompt_file: prompts/a.md", &["plan"]),
        (
            "path is not a file",
            "prompt: \"{{artifacts.a.path}} {{issue.title}}\"",
            &[],
        ),
        ("missing prompt file", "prompt_file: prompts/none.md", &[]),
    ];
    for (name, text, want) in cases {
        assert_eq!(artifact_names(&stage(text), Some(&package)), want, "{name}");
    }
    assert_eq!(
        artifact_names(&stage("prompt: \"{{artifacts.a.file}}\""), None),
        ["a"]
    );
}

#[test]
fn agent_names_and_parameter_values() {
    assert!(agent_name("Builder"));
    assert!(agent_name("a b"));
    assert!(!agent_name(""));
    assert!(!agent_name(" a"));
    assert!(!agent_name("a\n"));
    assert!(!agent_name(&"a".repeat(65)));
    assert!(agent_name(&"a".repeat(64)));
    let parameter = buzz_hive_core::domain::workflow::Parameter {
        name: "n".into(),
        kind: Some(buzz_hive_core::domain::workflow::ParamType::Number),
        ..Default::default()
    };
    assert!(check_param_value(&parameter, "3.5").is_ok());
    assert!(check_param_value(&parameter, "").is_ok());
    assert!(check_param_value(&parameter, "lots").is_err());
}
