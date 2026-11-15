//! The package checks. Ported from the Go engine's `check_test.go`, with the cases that
//! depended on agent profiles, permission levels and user settings replaced by the ones for
//! a stage that names one Buzz agent.

use std::collections::BTreeMap;

use buzz_hive_core::domain::workflow::{Finding, Severity};
use buzz_hive_core::workflow::check::{check, has_errors, CheckEnv};
use buzz_hive_core::workflow::load::{load, parse, Package};
use buzz_hive_core::workflow::source::{MemSource, Source};

/// A package from its `workflow.yaml` and extra files.
fn pkg(workflow: &str, extra: &[(&str, &str)]) -> Package {
    let mut files = BTreeMap::new();
    files.insert(
        "kiln-package.yaml".to_owned(),
        "name: demo\nversion: 1.0.0\n".to_owned(),
    );
    files.insert("workflow.yaml".to_owned(), workflow.to_owned());
    for (path, text) in extra {
        files.insert((*path).to_owned(), (*text).to_owned());
    }
    parse("test/demo", files).expect("the package parses")
}

fn with_params(package_extra: &str, workflow: &str) -> Package {
    let mut files = BTreeMap::new();
    files.insert(
        "kiln-package.yaml".to_owned(),
        format!("name: demo\nversion: 1.0.0\n{package_extra}"),
    );
    files.insert("workflow.yaml".to_owned(), workflow.to_owned());
    parse("test/demo", files).expect("the package parses")
}

fn codes(findings: &[Finding]) -> Vec<String> {
    findings.iter().map(|f| f.code.clone()).collect()
}

fn run(package: &Package) -> Vec<Finding> {
    check(package, &CheckEnv::default())
}

const VALID: &str = r#"version: 2
stages:
  - id: research
    name: Research
    kind: prompt
    agent: builder
    prompt_file: prompts/research.md
    outputs: [{ artifact: research, handoff: true }]
    results: [blocked]
    on: { passed: plan, needs_work: research, failed: stop, needs_input: ask, blocked: ask }
    loop_limit: 3
    execution: { timeout: 30m, retries: { max: 2, backoff: 1m } }
  - id: plan
    kind: prompt
    agent: builder
    prompt: "/plan {{issue.url}} {{artifacts.research.path}}"
"#;

#[test]
fn a_valid_package_has_no_findings() {
    let p = pkg(
        VALID,
        &[(
            "prompts/research.md",
            "Research {{issue.title}}\n\n{{issue.body}}",
        )],
    );
    assert_eq!(run(&p), vec![]);
    let (research, index) = p.stage("research").expect("the stage loads");
    assert_eq!(index, 0);
    assert_eq!(research.agent.as_deref(), Some("builder"));
}

#[test]
fn a_package_returns_every_finding_it_has() {
    let p = pkg(
        r#"version: 2
enabled: true
stages:
  - id: a
    kind: prompt
    agent: builder
    prompt: x
    on: { passed: nowhere }
  - id: b
    kind: prompt
    agent: builder
    prompt: x
    execution: { timeout: soon }
"#,
        &[],
    );
    let findings = run(&p);
    assert_eq!(
        codes(&findings),
        ["unknown_field", "invalid_value", "route_target_unknown"]
    );
    let paths: Vec<&str> = findings.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "workflow.yaml:enabled",
            "workflow.yaml:stages[1].execution.timeout",
            "workflow.yaml:stages[0].on.passed"
        ]
    );
    assert!(findings
        .iter()
        .all(|f| f.severity == Severity::Error && !f.message.is_empty()));
    assert!(has_errors(&findings));
}

struct Case {
    name: &'static str,
    yaml: &'static str,
    extra: &'static [(&'static str, &'static str)],
    want: &'static [&'static str],
}

const fn case(
    name: &'static str,
    yaml: &'static str,
    extra: &'static [(&'static str, &'static str)],
    want: &'static [&'static str],
) -> Case {
    Case {
        name,
        yaml,
        extra,
        want,
    }
}

const REVIEW_CMD: &[(&str, &str)] = &[("commands/review.md", "Review.")];

#[test]
fn rules_about_the_file_stage_ids_and_kinds() {
    run_cases(&[
        case(
            "version",
            "version: 1\nstages: [{id: a, kind: prompt, agent: fake, prompt: x}]\n",
            &[],
            &["version_unsupported"],
        ),
        case(
            "no stages",
            "version: 2\nstages: []\n",
            &[],
            &["stages_missing"],
        ),
        case(
            "stage ids",
            r#"version: 2
stages:
  - { kind: prompt, agent: fake, prompt: x }
  - { id: a, kind: prompt, agent: fake, prompt: x }
  - { id: a, kind: prompt, agent: fake, prompt: x }
  - { id: stop, kind: prompt, agent: fake, prompt: x }
"#,
            &[],
            &[
                "stage_id_missing",
                "stage_id_duplicate",
                "stage_id_reserved",
            ],
        ),
        case(
            "kinds",
            r#"version: 2
stages:
  - { id: a }
  - { id: b, kind: promt }
  - { id: c, kind: action }
  - { id: d, kind: prompt, agent: fake }
  - { id: e, kind: approval }
  - { id: f, kind: wait }
"#,
            &[],
            &[
                "kind_missing",
                "kind_unknown",
                "action_missing",
                "prompt_missing",
                "approval_artifacts_missing",
                "wait_labels_missing",
            ],
        ),
    ]);
}

#[test]
fn rules_about_routes_calls_and_conditions() {
    run_cases(&[
        case(
            "routes",
            r#"version: 2
stages:
  - id: a
    kind: prompt
    agent: fake
    prompt: x
    results: [blocked, parked]
    on: { blocked: ask, finished: a, passed: nowhere }
"#,
            &[],
            &[
                "result_unrouted",
                "route_result_unknown",
                "route_target_unknown",
                "route_cycle_unbounded",
            ],
        ),
        case(
            "call target unknown",
            r#"version: 2
stages:
  - id: a
    kind: prompt
    agent: fake
    prompt: x
    on: { needs_work: { to: missing, give: { plan: review } } }
  - { id: b, kind: prompt, agent: fake, prompt: x }
"#,
            &[],
            &["route_target_unknown"],
        ),
        case(
            "call then unknown",
            r#"version: 2
stages:
  - id: a
    kind: prompt
    agent: fake
    prompt: x
    on: { passed: { to: b, then: missing } }
  - { id: b, kind: prompt, agent: fake, prompt: x }
"#,
            &[],
            &["route_target_unknown"],
        ),
        case(
            "groups, conditions and loops",
            r#"version: 2
stages:
  - id: a
    kind: prompt
    agent: fake
    prompt: x
    approval: {artifacts: [plan]}
    wait: {labels: [ready]}
    conditions: {labels: [], else: nowhere}
  - id: b
    kind: prompt
    agent: fake
    prompt: x
    conditions: {labels: [go]}
    on: {needs_work: a}
  - id: c
    kind: prompt
    agent: fake
    prompt: x
    loop_limit: 2
    conditions: {labels: [go], else: stop}
    on: {needs_work: a, failed: c}
"#,
            &[],
            &[
                "approval_not_allowed",
                "wait_not_allowed",
                "conditions_labels_missing",
                "route_target_unknown",
                "conditions_else_missing",
                "route_cycle_unbounded",
            ],
        ),
    ]);
}

#[test]
fn rules_about_prompts_and_commands() {
    run_cases(&[
        case(
            "prompts",
            r#"version: 2
stages:
  - { id: a, kind: prompt, agent: fake, prompt: x, prompt_file: prompts/a.md }
  - { id: b, kind: prompt, agent: fake, prompt_file: prompts/none.md }
  - { id: c, kind: prompt, agent: fake, prompt_file: ../outside.md }
  - { id: d, kind: prompt, agent: fake, prompt: "{{issue.titel}} {{issue.titel}} {{artifacts.Bad Name.path}} {{repos}}" }
  - { id: e, kind: prompt, agent: fake, prompt_file: prompts/e.md }
"#,
            &[("prompts/a.md", "x"), ("prompts/e.md", "{{nope}}")],
            &[
                "prompt_conflict",
                "prompt_file_missing",
                "prompt_file_outside",
                "template_unknown_variable",
                "template_unknown_variable",
            ],
        ),
        case(
            "command and prompt",
            "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, command: review, prompt: x }\n",
            REVIEW_CMD,
            &["prompt_conflict"],
        ),
        case(
            "args without command",
            "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, prompt: x, args: y }\n",
            &[],
            &["args_without_command"],
        ),
        case(
            "bad command name",
            "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, command: 'a b' }\n",
            &[],
            &["command_name_invalid"],
        ),
        case(
            "missing command",
            "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, command: nope }\n",
            &[],
            &["command_missing"],
        ),
        case(
            "known command",
            "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, command: review, args: '{{issue.number}}' }\n",
            REVIEW_CMD,
            &[],
        ),
        case(
            "command from a skill",
            "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, command: plan }\n",
            &[("skills/plan/SKILL.md", "Plan.")],
            &[],
        ),
        case(
            "unknown variable in args",
            "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, command: review, args: '{{issue.titel}}' }\n",
            REVIEW_CMD,
            &["template_unknown_variable"],
        ),
    ]);
}

#[test]
fn rules_about_numbers_agents_and_removed_fields() {
    run_cases(&[
        case(
            "numbers",
            r#"version: 2
stages:
  - id: a
    kind: prompt
    agent: fake
    prompt: x
    loop_limit: -1
    execution: { timeout: -5m, retries: { max: -2 } }
"#,
            &[],
            &["count_negative", "count_negative", "duration_negative"],
        ),
        case(
            "a prompt stage with no agent",
            "version: 2\nstages:\n  - { id: a, kind: prompt, prompt: x }\n",
            &[],
            &["agent_unknown"],
        ),
        case(
            "an agent name with a space at the end",
            "version: 2\nstages:\n  - { id: a, kind: prompt, agent: 'fake ', prompt: x }\n",
            &[],
            &["agent_name_invalid"],
        ),
        case(
            "an agent name that is too long",
            "version: 2\nstages:\n  - id: a\n    kind: prompt\n    agent: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n    prompt: x\n",
            &[],
            &["agent_name_invalid"],
        ),
        case(
            "the old agent mapping",
            "version: 2\nstages:\n  - id: a\n    kind: prompt\n    agent: { profile: claude-code, permission: ask }\n    prompt: x\n",
            &[],
            &["agent_object_removed"],
        ),
        case(
            "conversation",
            r#"version: 2
stages:
  - id: a
    kind: prompt
    agent: fake
    prompt: "Read these: {{comments}}"
    conversation: {mode: deliver, from: [teammate]}
"#,
            &[],
            &["conversation_removed"],
        ),
        case(
            "idle timeout",
            "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, prompt: x, execution: { idle_timeout: 10m } }\n",
            &[],
            &["idle_timeout_removed"],
        ),
    ]);
}

#[test]
fn rules_about_outputs_and_repeat() {
    run_cases(&[
        case(
            "outputs and repeat",
            r#"version: 2
stages:
  - id: a
    kind: prompt
    agent: fake
    prompt: x
    outputs: [{artifact: Bad Name}, {artifact: plan}, {artifact: plan, handoff: false}]
  - { id: b, kind: approval, repeat: {until: plan_complete, plan: plan} }
"#,
            &[],
            &["artifact_name_invalid", "output_duplicate", "approval_artifacts_missing", "repeat_not_allowed"],
        ),
        case(
            "output file ok",
            "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, prompt: x, outputs: [{artifact: r, file: '.claude/tasks/GH-{{issue.number}}-*/r.md'}] }\n",
            &[],
            &[],
        ),
        case(
            "output file absolute",
            "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, prompt: x, outputs: [{artifact: r, file: /etc/passwd}] }\n",
            &[],
            &["output_file_invalid"],
        ),
        case(
            "output file parent",
            "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, prompt: x, outputs: [{artifact: r, file: '../x.md'}] }\n",
            &[],
            &["output_file_invalid"],
        ),
        case(
            "output file backslash",
            "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, prompt: x, outputs: [{artifact: r, file: 'a\\\\b.md'}] }\n",
            &[],
            &["output_file_invalid"],
        ),
        case(
            "output file bad glob",
            "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, prompt: x, outputs: [{artifact: r, file: 'a[.md'}] }\n",
            &[],
            &["output_file_invalid"],
        ),
        case(
            "output file unknown variable",
            "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, prompt: x, outputs: [{artifact: r, file: '{{issue.titel}}.md'}] }\n",
            &[],
            &["template_unknown_variable"],
        ),
    ]);
}

#[test]
fn rules_about_optional_outputs() {
    run_cases(&[
        case(
            "nothing needs it",
            "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, prompt: x, outputs: [{artifact: review, optional: true}] }\n",
            &[],
            &[],
        ),
        case(
            "a required input reads it through the passed call",
            r#"version: 2
stages:
  - id: a
    kind: prompt
    agent: fake
    prompt: x
    outputs: [{artifact: review, optional: true}]
    on: { passed: { to: b, give: { plan: review } } }
  - { id: b, kind: prompt, agent: fake, prompt: x, inputs: [{name: plan, required: true}] }
"#,
            &[],
            &["output_optional_needed"],
        ),
        case(
            "the next stage needs it by its own name",
            r#"version: 2
stages:
  - id: a
    kind: prompt
    agent: fake
    prompt: x
    outputs: [{artifact: review, optional: true}]
  - { id: b, kind: prompt, agent: fake, prompt: x, inputs: [{name: review, required: true}] }
"#,
            &[],
            &["output_optional_needed"],
        ),
        case(
            "the next stage reads it without requiring it",
            r#"version: 2
stages:
  - id: a
    kind: prompt
    agent: fake
    prompt: x
    outputs: [{artifact: review, optional: true}]
  - { id: b, kind: prompt, agent: fake, prompt: x, inputs: [{name: review}] }
"#,
            &[],
            &[],
        ),
        case(
            "another stage always saves it",
            r#"version: 2
stages:
  - id: a
    kind: prompt
    agent: fake
    prompt: x
    outputs: [{artifact: review}]
  - id: b
    kind: prompt
    agent: fake
    prompt: x
    outputs: [{artifact: review, optional: true}]
  - { id: c, kind: prompt, agent: fake, prompt: x, inputs: [{name: review, required: true}] }
"#,
            &[],
            &[],
        ),
        case(
            "read through a needs_work call only",
            r#"version: 2
stages:
  - id: a
    kind: prompt
    agent: fake
    prompt: x
    results: [needs_work]
    outputs: [{artifact: review, optional: true}]
    on: { passed: c, needs_work: { to: b, give: { plan: review } } }
  - { id: b, kind: prompt, agent: fake, prompt: x, inputs: [{name: plan, required: true}] }
  - { id: c, kind: prompt, agent: fake, prompt: x }
"#,
            &[],
            &[],
        ),
    ]);
}

#[test]
fn rules_about_inputs_and_calls() {
    run_cases(&[
        case(
            "input name invalid",
            "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, prompt: x, inputs: [{name: Bad Name}] }\n",
            &[],
            &["input_name_invalid"],
        ),
        case(
            "input duplicate",
            "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, prompt: x, inputs: [{name: plan}, {name: plan, required: true}] }\n",
            &[],
            &["input_duplicate"],
        ),
        case(
            "inputs on another kind",
            "version: 2\nstages:\n  - { id: a, kind: approval, inputs: [{name: plan}] }\n",
            &[],
            &["approval_artifacts_missing", "inputs_not_allowed"],
        ),
        case(
            "input template unknown",
            "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, prompt: 'Read {{inputs.plan.file}} and {{inputs.notes.path}}', inputs: [{name: notes}] }\n",
            &[],
            &["input_template_unknown"],
        ),
        case(
            "input template in an output file unknown",
            "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, prompt: x, outputs: [{artifact: r, file: '{{inputs.plan.file}}.out'}] }\n",
            &[],
            &["input_template_unknown"],
        ),
        case(
            "input template declared",
            "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, prompt: 'Read {{inputs.plan.file}} and {{inputs.plan.path}}', inputs: [{name: plan, required: true}] }\n",
            &[],
            &[],
        ),
        case(
            "a call gives an input the stage does not declare",
            r#"version: 2
stages:
  - id: a
    kind: prompt
    agent: fake
    prompt: x
    on: { needs_work: { to: b, give: { plan: review } } }
  - { id: b, kind: prompt, agent: fake, prompt: x, inputs: [{name: notes}] }
"#,
            &[],
            &["input_unknown"],
        ),
        case(
            "a call gives to a stage with no inputs",
            r#"version: 2
stages:
  - id: a
    kind: prompt
    agent: fake
    prompt: x
    on: { needs_work: { to: b, give: { plan: review } } }
  - { id: b, kind: prompt, agent: fake, prompt: x }
"#,
            &[],
            &["input_unknown"],
        ),
        case(
            "a call leaves out a required input no stage saves",
            r#"version: 2
stages:
  - id: a
    kind: prompt
    agent: fake
    prompt: x
    on: { needs_work: { to: b, give: { notes: n } } }
  - { id: b, kind: prompt, agent: fake, prompt: x, inputs: [{name: plan, required: true}, {name: notes}] }
"#,
            &[],
            &["input_missing"],
        ),
        case(
            "a call leaves out a required input a stage saves",
            r#"version: 2
stages:
  - id: a
    kind: prompt
    agent: fake
    prompt: x
    outputs: [{artifact: plan}]
    on: { needs_work: { to: b, give: { notes: n } } }
  - { id: b, kind: prompt, agent: fake, prompt: x, inputs: [{name: plan, required: true}, {name: notes}] }
"#,
            &[],
            &[],
        ),
        case(
            "a call gives every required input",
            r#"version: 2
stages:
  - id: a
    kind: prompt
    agent: fake
    prompt: x
    on: { needs_work: { to: b, give: { plan: review } } }
  - { id: b, kind: prompt, agent: fake, prompt: x, inputs: [{name: plan, required: true}] }
"#,
            &[],
            &[],
        ),
        case(
            "a call gives a document with a bad name",
            r#"version: 2
stages:
  - id: a
    kind: prompt
    agent: fake
    prompt: x
    on: { needs_work: { to: b, give: { plan: Bad Name } } }
  - { id: b, kind: prompt, agent: fake, prompt: x, inputs: [{name: plan}] }
"#,
            &[],
            &["give_artifact_invalid"],
        ),
        case(
            "a plain route into a stage with a required input",
            r#"version: 2
stages:
  - id: a
    kind: prompt
    agent: fake
    prompt: x
    on: { needs_work: b }
  - { id: b, kind: prompt, agent: fake, prompt: x, inputs: [{name: plan, required: true}] }
"#,
            &[],
            &[],
        ),
        case(
            "a stage called with a then calls another stage",
            r#"version: 2
stages:
  - id: a
    kind: prompt
    agent: fake
    prompt: x
    on: { needs_work: { to: b, then: c } }
  - id: b
    kind: prompt
    agent: fake
    prompt: x
    on: { needs_work: { to: c, give: { plan: review } } }
  - { id: c, kind: prompt, agent: fake, prompt: x, inputs: [{name: plan}] }
"#,
            &[],
            &["call_then_replaced"],
        ),
        case(
            "a stage called with a then routes plainly",
            r#"version: 2
stages:
  - id: a
    kind: prompt
    agent: fake
    prompt: x
    on: { needs_work: { to: b, then: c } }
  - id: b
    kind: prompt
    agent: fake
    prompt: x
    on: { needs_work: c }
  - { id: c, kind: prompt, agent: fake, prompt: x }
"#,
            &[],
            &[],
        ),
        case(
            "a stage called plainly calls another stage",
            r#"version: 2
stages:
  - id: a
    kind: prompt
    agent: fake
    prompt: x
    on: { needs_work: b }
  - id: b
    kind: prompt
    agent: fake
    prompt: x
    on: { needs_work: { to: c, give: { plan: review } } }
  - { id: c, kind: prompt, agent: fake, prompt: x, inputs: [{name: plan}] }
"#,
            &[],
            &[],
        ),
    ]);
}

#[test]
fn rules_about_action_fields_and_workspaces() {
    run_cases(&[
        case(
            "action fields on another kind",
            r#"version: 2
stages:
  - id: a
    kind: prompt
    agent: fake
    prompt: x
    action: merge
    with: {method: squash}
"#,
            &[],
            &["action_not_allowed", "with_not_allowed"],
        ),
        case(
            "action arguments",
            r#"version: 2
stages:
  - id: a
    kind: prompt
    agent: fake
    prompt: x
    with: {argv: [npm, [nested]], method: {a: b}}
"#,
            &[],
            &["invalid_value", "invalid_value", "with_not_allowed"],
        ),
        case(
            "workspace values",
            "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, prompt: x, workspace: somewhere }\n",
            &[],
            &["invalid_value"],
        ),
        case(
            "workspace on an approval",
            "version: 2\nstages:\n  - { id: a, kind: approval, approval: {artifacts: [plan]}, workspace: branch }\n",
            &[],
            &["workspace_not_allowed"],
        ),
        case(
            "workspace on a wait",
            "version: 2\nstages:\n  - { id: a, kind: wait, wait: {labels: [ready]}, workspace: none }\n",
            &[],
            &["workspace_not_allowed"],
        ),
        case(
            "prompt with no workspace",
            "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, prompt: x, workspace: none }\n",
            &[],
            &["workspace_not_allowed"],
        ),
        case(
            "prompt on the branch",
            "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, prompt: x, workspace: branch }\n",
            &[],
            &[],
        ),
        case(
            "wrong types",
            r#"version: 2
stages:
  - id: a
    kind: prompt
    agent: fake
    prompt: x
    loop_limit: many
    results: blocked
"#,
            &[],
            &["invalid_value", "invalid_value"],
        ),
    ]);
}

fn run_cases(cases: &[Case]) {
    let mut failures = Vec::new();
    for c in cases {
        let p = pkg(c.yaml, c.extra);
        let findings = run(&p);
        let got = codes(&findings);
        if got != c.want {
            failures.push(format!(
                "{}: codes {got:?}, want {:?}; findings {findings:#?}",
                c.name, c.want
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn action_arguments_decode_as_values_and_lists() {
    use buzz_hive_core::domain::workflow::ActionArg;
    let p = pkg(
        r#"version: 2
stages:
  - id: a
    kind: action
    action: run_command
    with: {argv: [npm, test, "--issue={{issue.number}}"], note: "{{issue.title}}"}
"#,
        &[],
    );
    assert_eq!(p.findings, vec![]);
    let with = p.workflow.stages[0]
        .with
        .as_ref()
        .expect("the stage has arguments");
    assert_eq!(
        with["argv"],
        ActionArg::List(vec![
            "npm".into(),
            "test".into(),
            "--issue={{issue.number}}".into()
        ])
    );
    assert_eq!(with["note"], ActionArg::Text("{{issue.title}}".into()));
}

#[test]
fn an_unknown_package_field_is_a_finding() {
    let mut files = BTreeMap::new();
    files.insert(
        "kiln-package.yaml".to_owned(),
        "name: demo\nversion: 1.0.0\nenabled: true\n".to_owned(),
    );
    files.insert(
        "workflow.yaml".to_owned(),
        "version: 2\nstages: [{id: a, kind: prompt, agent: fake, prompt: x}]\n".to_owned(),
    );
    let p = parse("test/demo", files).expect("parses");
    let findings = run(&p);
    assert_eq!(codes(&findings), ["unknown_field"]);
    assert_eq!(findings[0].path, "kiln-package.yaml:enabled");
}

#[test]
fn a_name_clash_is_a_finding_on_both_packages() {
    let files = |desc: &str| {
        MemSource::new()
            .with(
                "one/kiln-package.yaml",
                format!("name: same\nversion: 1.0.0\ndescription: {desc}\n"),
            )
            .with(
                "one/workflow.yaml",
                "version: 2\nstages: [{id: a, kind: prompt, agent: fake, prompt: x}]\n",
            )
    };
    let catalog = load(&[
        Source::new("/mine", false, files("mine")),
        Source::new("/installed", false, files("installed")),
    ]);
    let both = catalog.all("same");
    assert_eq!(both.len(), 2);
    assert_eq!(both[0].meta.description, "mine");
    for p in both {
        let findings = check(
            p,
            &CheckEnv {
                catalog: Some(&catalog),
            },
        );
        assert_eq!(codes(&findings), ["name_clash"], "{}", p.path);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(
            findings[0].message.contains("/mine/one")
                && findings[0].message.contains("/installed/one")
        );
    }
}

#[test]
fn the_hash_follows_the_files() {
    let a = pkg(VALID, &[("prompts/research.md", "Research")]);
    let mut b = pkg(VALID, &[("prompts/research.md", "Research")]);
    assert_eq!(a.hash(), b.hash());
    assert_eq!(a.hash().len(), 64);
    b.files
        .insert("prompts/research.md".into(), "Research more".into());
    assert_ne!(a.hash(), b.hash());
    // Bytes moved between a file name and its content still change it.
    let hash = |name: &str, text: &str| {
        buzz_hive_core::workflow::hash::hash_files(&BTreeMap::from([(
            name.to_owned(),
            text.to_owned(),
        )]))
    };
    assert_ne!(hash("ab", "c"), hash("a", "bc"));
}

#[test]
fn an_optional_output_and_a_route_survive_a_round_trip_through_json() {
    use buzz_hive_core::domain::workflow::WorkflowFile;
    let p = pkg(
        r#"version: 2
stages:
  - id: a
    name: A
    kind: prompt
    agent: builder
    prompt: "x"
    results: [needs_work]
    loop_limit: 2
    outputs: [{ artifact: review, file: r.md, optional: true }, { artifact: plan }]
    on:
      passed: b
      needs_work: { to: b, give: { plan: review }, then: c }
  - { id: b, name: B, kind: prompt, agent: builder, prompt: "x" }
  - { id: c, name: C, kind: prompt, agent: builder, prompt: "x" }
"#,
        &[],
    );
    assert_eq!(p.findings, vec![]);
    let a = &p.workflow.stages[0];
    assert!(a.outputs[0].optional && !a.outputs[1].optional);
    assert!(!a.on["passed"].is_call() && a.on["passed"].to == "b");
    let call = &a.on["needs_work"];
    assert!(call.is_call());
    assert_eq!(call.to, "b");
    assert_eq!(call.give["plan"], "review");
    assert_eq!(call.then, "c");
    // A plain route is a string in JSON and a call is an object, and both read back.
    let json = serde_json::to_value(&p.workflow).expect("serializes");
    assert_eq!(json["stages"][0]["on"]["passed"], "b");
    assert_eq!(json["stages"][0]["on"]["needs_work"]["to"], "b");
    let back: WorkflowFile = serde_json::from_value(json).expect("reads back");
    assert_eq!(back, p.workflow);
}

#[test]
fn an_output_that_never_sets_optional_is_not_optional() {
    let p = pkg(
        "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, prompt: x, outputs: [{artifact: plan, handoff: true, file: p.md}] }\n",
        &[],
    );
    let output = &p.workflow.stages[0].outputs[0];
    assert!(!output.optional);
    assert_eq!(output.handoff, Some(true));
    assert_eq!(output.file, "p.md");
}

#[test]
fn a_call_route_with_an_unknown_key_is_a_finding() {
    let p = pkg(
        "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, prompt: x, on: { needs_work: { to: a, bogus: 1 } } }\n",
        &[],
    );
    assert!(
        codes(&p.findings).contains(&"unknown_field".to_owned()),
        "{:?}",
        p.findings
    );
}

#[test]
fn an_unknown_role_is_named_at_its_position() {
    let p = pkg(
        "version: 2\nstages:\n  - { id: a, kind: prompt, agent: fake, prompt: x, roles: [merges, deploys, closes_issue] }\n",
        &[],
    );
    let findings = run(&p);
    assert_eq!(codes(&findings), ["unknown_role"]);
    assert_eq!(findings[0].path, "workflow.yaml:stages[0].roles[1]");
    assert!(findings[0].message.contains("deploys"));
    assert_eq!(
        p.workflow.stages[0].roles,
        ["merges", "deploys", "closes_issue"]
    );
}

const OK_WORKFLOW: &str = "version: 2\nstages: [{id: a, kind: prompt, agent: fake, prompt: x}]\n";

#[test]
fn parameter_rules() {
    let cases: &[(&str, &str, &[&str], &str)] = &[
        (
            "valid",
            "parameters:\n  - { name: depth, type: choice, default: normal, choices: [normal, deep] }\n  - { name: retries, type: number, default: \"3\" }\n  - { name: verbose, type: bool, default: \"false\" }\n  - { name: base, type: branch, default: main }\n  - { name: note, type: text }\n",
            &[],
            "",
        ),
        ("bad name", "parameters:\n  - { name: Depth, type: text }\n", &["parameter_name"], "kiln-package.yaml:parameters[0].name"),
        ("dotted name", "parameters:\n  - { name: a.b, type: text }\n", &["parameter_name"], "kiln-package.yaml:parameters[0].name"),
        (
            "duplicate",
            "parameters:\n  - { name: a, type: text }\n  - { name: a, type: text }\n",
            &["parameter_duplicate"],
            "kiln-package.yaml:parameters[1].name",
        ),
        ("choice with no choices", "parameters:\n  - { name: a, type: choice }\n", &["parameter_choices"], "kiln-package.yaml:parameters[0].choices"),
        ("choices on a text", "parameters:\n  - { name: a, type: text, choices: [x] }\n", &["parameter_choices"], "kiln-package.yaml:parameters[0].choices"),
        ("number default", "parameters:\n  - { name: a, type: number, default: lots }\n", &["parameter_default"], "kiln-package.yaml:parameters[0].default"),
        (
            "choice default",
            "parameters:\n  - { name: a, type: choice, choices: [x, y], default: z }\n",
            &["parameter_default"],
            "kiln-package.yaml:parameters[0].default",
        ),
        ("unknown type", "parameters:\n  - { name: a, type: date }\n", &["invalid_value"], "kiln-package.yaml:parameters[0].type"),
    ];
    for (name, params, want, want_path) in cases {
        let findings = run(&with_params(params, OK_WORKFLOW));
        assert_eq!(codes(&findings), *want, "{name}: {findings:#?}");
        if !want_path.is_empty() {
            assert_eq!(findings[0].path, *want_path, "{name}");
        }
    }
}

#[test]
fn parameters_are_checked_without_a_workflow_file() {
    let mut files = BTreeMap::new();
    files.insert(
        "kiln-package.yaml".to_owned(),
        "name: demo\nversion: 1.0.0\nparameters:\n  - { name: Bad, type: text }\n".to_owned(),
    );
    let p = parse("test/demo", files).expect("parses");
    let got: Vec<_> = run(&p)
        .into_iter()
        .filter(|f| f.code == "parameter_name")
        .collect();
    assert_eq!(got.len(), 1);
}

#[test]
fn template_parameter_rules() {
    const DECLARE: &str = "parameters:\n  - { name: depth, type: text }\n";
    type Row = (
        &'static str,
        &'static str,
        &'static str,
        &'static [(&'static str, &'static str)],
        &'static [&'static str],
        &'static str,
    );
    let rows: &[Row] = &[
        (
            "declared in a prompt",
            DECLARE,
            "version: 2\nstages: [{id: a, kind: prompt, agent: fake, prompt: \"go {{params.depth}}\"}]\n",
            &[],
            &[],
            "",
        ),
        (
            "undeclared in a prompt",
            DECLARE,
            "version: 2\nstages: [{id: a, kind: prompt, agent: fake, prompt: \"go {{params.nope}}\"}]\n",
            &[],
            &["template_unknown_parameter"],
            "workflow.yaml:stages[0].prompt",
        ),
        (
            "undeclared with no parameters",
            "",
            "version: 2\nstages: [{id: a, kind: prompt, agent: fake, prompt: \"{{params.depth}}\"}]\n",
            &[],
            &["template_unknown_parameter"],
            "workflow.yaml:stages[0].prompt",
        ),
        (
            "undeclared reported once",
            DECLARE,
            "version: 2\nstages: [{id: a, kind: prompt, agent: fake, prompt: \"{{params.x}} {{ params.x }}\"}]\n",
            &[],
            &["template_unknown_parameter"],
            "",
        ),
        (
            "undeclared in a prompt file",
            DECLARE,
            "version: 2\nstages: [{id: a, kind: prompt, agent: fake, prompt_file: p.md}]\n",
            &[("p.md", "{{params.nope}}")],
            &["template_unknown_parameter"],
            "workflow.yaml:stages[0].prompt_file",
        ),
        (
            "an invalid name is an unknown variable",
            DECLARE,
            "version: 2\nstages: [{id: a, kind: prompt, agent: fake, prompt: \"{{params.A}}\"}]\n",
            &[],
            &["template_unknown_variable"],
            "",
        ),
    ];
    for (name, params, workflow, extra, want, want_path) in rows {
        let mut p = with_params(params, workflow);
        for (path, text) in *extra {
            p.files.insert((*path).to_owned(), (*text).to_owned());
        }
        let findings = run(&p);
        assert_eq!(codes(&findings), *want, "{name}: {findings:#?}");
        if !want_path.is_empty() {
            assert_eq!(findings[0].path, *want_path, "{name}");
        }
    }
}

#[test]
fn action_parameters_are_checked_by_argument() {
    const DECLARE: &str = "parameters:\n  - { name: depth, type: text }\n";
    let ok = with_params(
        DECLARE,
        r#"version: 2
stages:
  - id: a
    kind: action
    action: run_command
    with: {argv: [npm, "--depth={{params.depth}}"]}
"#,
    );
    assert!(!codes(&run(&ok)).contains(&"template_unknown_parameter".to_owned()));

    let gone = with_params(
        "",
        r#"version: 2
stages:
  - id: a
    kind: action
    action: run_command
    with: {argv: [npm, "{{params.gone}}"]}
"#,
    );
    let paths: Vec<String> = run(&gone)
        .into_iter()
        .filter(|f| f.code == "template_unknown_parameter")
        .map(|f| f.path)
        .collect();
    assert_eq!(paths, ["workflow.yaml:stages[0].with.argv"]);
}

#[test]
fn every_old_agent_key_fails_with_agent_object_removed() {
    for key in [
        "profile: claude-code",
        "permission: ask",
        "model_tier: strongest",
        "effort: high",
        "tools: { allow: [], deny: [] }",
        "setting_sources: [user]",
        "options: {}",
        "model: x",
    ] {
        let p = pkg(
            &format!(
                "version: 2\nstages:\n  - {{ id: a, kind: prompt, agent: {{ {key} }}, prompt: x }}\n"
            ),
            &[],
        );
        let findings = run(&p);
        assert_eq!(
            codes(&findings),
            ["agent_object_removed"],
            "{key}: {findings:#?}"
        );
        assert_eq!(findings[0].path, "workflow.yaml:stages[0].agent");
        assert!(
            findings[0].message.contains("profile, model and effort"),
            "{key}"
        );
    }
}
