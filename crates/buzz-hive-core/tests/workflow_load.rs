//! Loading packages from sources, and the packages every build ships.

use buzz_hive_core::domain::workflow::Severity;
use buzz_hive_core::workflow::builtin::{builtin_source, builtin_templates};
use buzz_hive_core::workflow::check::{check, has_errors, CheckEnv};
use buzz_hive_core::workflow::load::{clean_path, load, valid_path, Package};
use buzz_hive_core::workflow::source::{BrokenSource, MemSource, Source};
use buzz_hive_core::workflow::templates::load_stage_templates;

fn demo_source() -> MemSource {
    MemSource::new()
        .with("demo/kiln-package.yaml", "name: demo\nversion: 1.0.0\n")
        .with(
            "demo/workflow.yaml",
            r#"version: 2
stages:
  - id: research
    name: Research
    kind: prompt
    agent: builder
    prompt_file: prompts/research.md
    results: [blocked]
    on: { passed: build, blocked: ask }
  - id: build
    kind: prompt
    agent: builder
    prompt: "Build {{issue.title}}"
"#,
        )
        .with("demo/prompts/research.md", "Research {{issue.title}}")
        .with("demo/.DS_Store", "junk")
        .with("demo/.claude-plugin/plugin.json", r#"{"name":"demo"}"#)
        .with("demo/skills/a/.git/config", "junk")
        .with("not-a-package/readme.md", "ignored")
        .with("broken/kiln-package.yaml", "name: broken\n")
        .with("nameless/kiln-package.yaml", "version: 1.0.0\n")
}

#[test]
fn load_reads_packages() {
    let catalog = load(&[Source::new("test", false, demo_source())]);
    assert_eq!(catalog.names(), ["broken", "demo"]);
    assert_eq!(catalog.findings.len(), 1);
    assert_eq!(catalog.findings[0].path, "test/nameless");
    assert_eq!(catalog.findings[0].code, "package_unreadable");

    let broken = catalog.get("broken").expect("broken loads");
    assert_eq!(broken.findings.len(), 1);
    assert_eq!(broken.findings[0].code, "workflow_missing");
    assert_eq!(broken.findings[0].severity, Severity::Error);

    let demo = catalog.get("demo").expect("demo loads");
    assert!(!demo.files.contains_key(".DS_Store"));
    assert_eq!(demo.files.len(), 4, "{:?}", demo.files.keys());
    assert!(demo.files.contains_key(".claude-plugin/plugin.json"));

    let (research, index) = demo.stage("research").expect("stage research");
    assert_eq!(index, 0);
    assert_eq!(research.agent.as_deref(), Some("builder"));
    assert_eq!(research.on["passed"].to, "build");
    assert_eq!(research.results.len(), 1);
    assert_eq!(
        demo.prompt_text(research).unwrap(),
        "Research {{issue.title}}"
    );

    let (build, _) = demo.stage("build").expect("stage build");
    assert_eq!(demo.prompt_text(build).unwrap(), "Build {{issue.title}}");
    assert!(demo.stage("missing").is_none());

    let mut stage = build.clone();
    stage.prompt = String::new();
    stage.prompt_file = "prompts/none.md".into();
    assert!(demo.prompt_text(&stage).is_err());
}

#[test]
fn load_skips_dot_folders_and_marks_built_in_sources() {
    let source = MemSource::new()
        .with(".copy-1/kiln-package.yaml", "name: half\nversion: 1.0.0\n")
        .with("whole/kiln-package.yaml", "name: whole\nversion: 1.0.0\n");
    let catalog = load(&[Source::new("/mine", true, source)]);
    assert_eq!(
        catalog.names(),
        ["whole"],
        "a dot folder is a copy still being written"
    );
    assert!(catalog.get("whole").unwrap().builtin);
}

#[test]
fn a_source_that_cannot_be_listed_is_a_catalog_finding() {
    let catalog = load(&[Source::new(
        "/gone",
        false,
        BrokenSource("permission denied".into()),
    )]);
    assert!(catalog.names().is_empty());
    assert_eq!(catalog.findings.len(), 1);
    assert_eq!(catalog.findings[0].code, "source_unreadable");
    assert_eq!(catalog.findings[0].path, "/gone");
}

#[test]
fn a_file_that_is_not_text_makes_its_package_unreadable() {
    let source = MemSource::new()
        .with("bin/kiln-package.yaml", "name: bin\nversion: 1.0.0\n")
        .with("bin/blob", [0xff_u8, 0xfe, 0x00]);
    let catalog = load(&[Source::new("test", false, source)]);
    assert!(catalog.names().is_empty());
    assert_eq!(catalog.findings[0].code, "package_unreadable");
    assert!(catalog.findings[0].message.contains("blob"));
}

#[test]
fn a_prompt_for_a_command_names_the_plugin() {
    let catalog = load(&[Source::new("test", false, demo_source())]);
    let mut p: Package = catalog.get("demo").unwrap().clone();
    let mut stage = p.workflow.stages[0].clone();
    stage.prompt = String::new();
    stage.prompt_file = String::new();
    stage.command = "review".into();
    stage.args = "{{issue.number}} now".into();
    assert_eq!(
        p.prompt_text(&stage).unwrap(),
        "/demo:review {{issue.number}} now"
    );

    p.files.insert(
        ".claude-plugin/plugin.json".into(),
        r#"{"name":"manifest"}"#.into(),
    );
    stage.args = String::new();
    assert_eq!(p.prompt_text(&stage).unwrap(), "/manifest:review");

    p.files
        .insert(".claude-plugin/plugin.json".into(), "{".into());
    assert_eq!(p.command_prefix(), "demo");
    p.files.insert(
        ".claude-plugin/plugin.json".into(),
        r#"{"version":"1"}"#.into(),
    );
    assert_eq!(p.command_prefix(), "demo");
}

#[test]
fn paths_are_cleaned_and_validated_as_go_does() {
    assert_eq!(clean_path("a//b/./c/../d"), "a/b/d");
    assert_eq!(clean_path("../x.md"), "../x.md");
    assert_eq!(clean_path("/a/../.."), "/");
    assert_eq!(clean_path(""), ".");
    assert!(valid_path("prompts/a.md"));
    assert!(valid_path("."));
    assert!(!valid_path("../a"));
    assert!(!valid_path("/a"));
    assert!(!valid_path("a//b"));
    assert!(!valid_path(""));
}

#[test]
fn every_built_in_package_loads_and_checks_clean() {
    let catalog = load(&[builtin_source()]);
    assert_eq!(
        catalog.names(),
        ["demo", "engineering", "openspec", "qrspi", "rpi", "task"]
    );
    assert_eq!(catalog.findings, vec![]);
    let env = CheckEnv {
        catalog: Some(&catalog),
    };
    for name in catalog.names() {
        let package = catalog.get(name).unwrap();
        assert!(package.builtin, "{name} is marked built-in");
        assert!(!package.workflow.stages.is_empty(), "{name} has stages");
        let findings = check(package, &env);
        assert!(!has_errors(&findings), "{name}: {findings:#?}");
        assert_eq!(findings, vec![], "{name} has warnings");
        for stage in &package.workflow.stages {
            if stage.kind == "prompt" {
                assert!(
                    stage.agent.as_deref().is_some_and(|a| !a.is_empty()),
                    "{name}/{} names an agent",
                    stage.id
                );
            }
        }
    }
}

#[test]
fn every_built_in_stage_template_decodes() {
    let files = builtin_templates();
    assert!(!files.is_empty());
    let templates = load_stage_templates(&files).expect("the shipped templates decode");
    let ids: Vec<&str> = templates.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "implement",
            "plan_review",
            "pull_request",
            "research",
            "wait_for_merge"
        ]
    );
    for template in &templates {
        let value: serde_json::Value = serde_json::from_str(&template.json).expect("JSON");
        assert_eq!(value["id"], template.id.as_str());
    }
}

#[test]
fn built_in_packages_hash_the_same_each_time() {
    let one = load(&[builtin_source()]);
    let two = load(&[builtin_source()]);
    for name in one.names() {
        assert_eq!(one.get(name).unwrap().hash(), two.get(name).unwrap().hash());
    }
}
