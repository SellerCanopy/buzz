//! The workflow procedures over `MemStore` and the shipped packages.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use base64::Engine as _;
use buzz_hive_core::testing::MemStore;
use buzz_hive_core::workflow::builtin::builtin_source;
use buzz_hive_core::workflow::options::WorkflowOptions;
use buzz_hive_core::workflow::source::{MemSource, Source, Sources};
use common::{app, call, community, MEMBER, OWNER, STRANGER};
use serde_json::json;

const BUILTINS: [&str; 6] = ["demo", "engineering", "openspec", "qrspi", "rpi", "task"];

fn broken_source() -> MemSource {
    MemSource::new()
        .with("broken/kiln-package.yaml", "name: broken\nversion: 1.0.0\n")
        .with(
            "broken/workflow.yaml",
            "version: 2\nstages:\n  - { id: a, kind: prompt, agent: builder, prompt: x, on: { passed: nowhere } }\n",
        )
        .with("nameless/kiln-package.yaml", "version: 1.0.0\n")
}

async fn app_with_broken() -> buzz_hive_api::App {
    let mut app = app(MemStore::new()).await;
    app.catalog = Arc::new(Sources(vec![
        builtin_source(),
        Source::new("/mine", false, broken_source()),
    ]));
    app
}

#[tokio::test]
async fn list_workflows_names_every_built_in_with_its_stages() {
    let app = app(MemStore::new()).await;
    let (status, body) = call(&app, "ListWorkflows", MEMBER, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let workflows = body["workflows"].as_array().unwrap();
    let names: Vec<&str> = workflows
        .iter()
        .map(|w| w["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, BUILTINS);
    for workflow in workflows {
        assert_eq!(workflow["ok"], true, "{}", workflow["name"]);
        assert_eq!(workflow["builtin"], true);
        assert!(
            workflow.get("own").is_none(),
            "own is false, so it is left out"
        );
        let stages = workflow["stages"].as_array().unwrap();
        assert!(!stages.is_empty(), "{} has stages", workflow["name"]);
        for stage in stages {
            assert!(!stage["id"].as_str().unwrap().is_empty());
            assert!(!stage["name"].as_str().unwrap().is_empty());
            assert!(
                ["prompt", "approval", "wait", "action"].contains(&stage["kind"].as_str().unwrap())
            );
            if stage["kind"] == "prompt" {
                assert!(!stage["agent"].as_str().unwrap().is_empty());
            }
        }
    }
}

#[tokio::test]
async fn list_workflows_marks_a_package_with_errors_not_ok_and_keeps_listing() {
    let app = app_with_broken().await;
    let (status, body) = call(&app, "ListWorkflows", MEMBER, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let workflows = body["workflows"].as_array().unwrap();
    let broken = workflows.iter().find(|w| w["name"] == "broken").unwrap();
    assert!(broken.get("ok").is_none(), "false is left out: {broken}");
    assert!(broken.get("builtin").is_none());
    assert_eq!(broken["paths"], json!(["/mine/broken"]));
    assert_eq!(broken["stages"][0]["id"], "a");
    assert_eq!(workflows.iter().filter(|w| w["ok"] == true).count(), 6);
}

#[tokio::test]
async fn list_workflows_reads_the_options_a_person_saved() {
    let store = MemStore::new();
    let mut tx = buzz_hive_core::ports::store::Store::begin(&store, community())
        .await
        .unwrap();
    tx.workflows()
        .set_options(
            "task",
            &WorkflowOptions {
                paused_stages: Some(vec![]),
                next_workflow: "rpi".into(),
                labels: vec!["chore".into()],
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let app = app(store).await;
    let (_, body) = call(&app, "ListWorkflows", MEMBER, json!({})).await;
    let task = body["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == "task")
        .unwrap();
    assert_eq!(task["nextWorkflow"], "rpi");
    assert_eq!(task["labels"], json!(["chore"]));
    assert!(
        task.get("pausedStages").is_none(),
        "an empty list pauses nothing"
    );
}

#[tokio::test]
async fn the_workflow_procedures_need_read() {
    let app = app(MemStore::new()).await;
    for method in [
        "ListWorkflows",
        "CheckWorkflow",
        "ListTemplateVariables",
        "ListStageTemplates",
    ] {
        let (status, _) = call(&app, method, STRANGER, json!({})).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method}");
        let (status, body) = call(&app, method, OWNER, json!({})).await;
        assert_eq!(status, StatusCode::OK, "{method}: {body}");
    }
}

#[tokio::test]
async fn check_workflow_with_no_name_reports_every_package_and_every_folder_that_failed() {
    let app = app_with_broken().await;
    let (status, body) = call(&app, "CheckWorkflow", MEMBER, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.get("ok").is_none(), "an error finding makes ok false");
    let packages = body["packages"].as_array().unwrap();
    assert_eq!(
        packages.len(),
        8,
        "six built-ins, broken and the nameless folder"
    );
    let broken = packages.iter().find(|p| p["name"] == "broken").unwrap();
    assert_eq!(broken["path"], "/mine/broken");
    assert_eq!(broken["findings"][0]["code"], "route_target_unknown");
    assert_eq!(broken["findings"][0]["severity"], "error");
    assert_eq!(
        broken["findings"][0]["path"],
        "workflow.yaml:stages[0].on.passed"
    );
    let nameless = packages.iter().find(|p| p.get("name").is_none()).unwrap();
    assert_eq!(nameless["path"], "/mine/nameless");
    assert_eq!(nameless["findings"][0]["code"], "package_unreadable");
    assert!(nameless["findings"][0].get("path").is_none());
}

#[tokio::test]
async fn check_workflow_by_name_and_the_built_ins_are_ok() {
    let app = app_with_broken().await;
    let (_, body) = call(&app, "CheckWorkflow", MEMBER, json!({"name": "qrspi"})).await;
    assert_eq!(body["ok"], true, "{body}");
    assert_eq!(body["packages"].as_array().unwrap().len(), 1);
    assert!(body["packages"][0].get("findings").is_none());

    let (status, body) = call(&app, "CheckWorkflow", MEMBER, json!({"name": "ghost"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

fn b64(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text)
}

#[tokio::test]
async fn check_workflow_checks_a_draft_in_place_of_a_package() {
    let app = app(MemStore::new()).await;
    let draft = json!({
        "name": "mine",
        "files": {
            "kiln-package.yaml": b64("name: mine\nversion: 1.0.0\n"),
            "workflow.yaml": b64("{\"version\": 2, \"stages\": [{\"id\": \"a\", \"kind\": \"prompt\", \"agent\": \"builder\", \"prompt\": \"x\", \"on\": {\"passed\": \"nowhere\"}}]}"),
        }
    });
    let (status, body) = call(&app, "CheckWorkflow", MEMBER, json!({"draft": draft})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["packages"][0]["name"], "mine");
    assert_eq!(
        body["packages"][0]["findings"][0]["code"],
        "route_target_unknown"
    );
    assert!(body.get("ok").is_none());
}

#[tokio::test]
async fn a_draft_with_a_bad_name_or_file_is_refused() {
    let app = app(MemStore::new()).await;
    let package = b64("name: mine\nversion: 1.0.0\n");
    let cases = [
        json!({"name": "other", "files": {"kiln-package.yaml": package}}),
        json!({"name": "mine", "files": {"kiln-package.yaml": package, "../x": b64("x")}}),
        json!({"name": "mine", "files": {"kiln-package.yaml": package, "a/.git/config": b64("x")}}),
        json!({"name": "mine", "files": {"kiln-package.yaml": package, "blob": base64::engine::general_purpose::STANDARD.encode([0xff_u8, 0xfe])}}),
        json!({"name": "mine", "files": {"workflow.yaml": b64("version: 2\n")}}),
    ];
    for draft in cases {
        let (status, body) = call(
            &app,
            "CheckWorkflow",
            MEMBER,
            json!({"draft": draft.clone()}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{draft}: {body}");
    }
}

#[tokio::test]
async fn template_variables_come_from_the_draft_or_the_fixed_list() {
    let app = app(MemStore::new()).await;
    let (_, fixed) = call(&app, "ListTemplateVariables", MEMBER, json!({})).await;
    let names: Vec<&str> = fixed["variables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.first(), Some(&"issue.title"));
    assert!(!names.iter().any(|n| n.contains('<')));

    let draft = json!({
        "name": "mine",
        "files": {
            "kiln-package.yaml": b64("name: mine\nversion: 1.0.0\nparameters:\n  - { name: depth, type: text }\n"),
            "workflow.yaml": b64("version: 2\nstages: [{id: a, kind: prompt, agent: builder, prompt: x, outputs: [{artifact: plan}]}]\n"),
        }
    });
    let (status, body) = call(
        &app,
        "ListTemplateVariables",
        MEMBER,
        json!({"draft": draft}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let names: Vec<&str> = body["variables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"params.depth"));
    assert!(names.contains(&"artifacts.plan.path"));
}

#[tokio::test]
async fn stage_templates_are_the_shipped_ones_sorted_by_id() {
    let app = app(MemStore::new()).await;
    let (status, body) = call(&app, "ListStageTemplates", MEMBER, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let templates = body["templates"].as_array().unwrap();
    let ids: Vec<&str> = templates
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "implement",
            "plan_review",
            "pull_request",
            "research",
            "wait_for_merge",
            "worktree"
        ]
    );
    for template in templates {
        let stage: serde_json::Value =
            serde_json::from_str(template["stageJson"].as_str().unwrap()).unwrap();
        assert_eq!(stage["id"], template["id"]);
    }
}
