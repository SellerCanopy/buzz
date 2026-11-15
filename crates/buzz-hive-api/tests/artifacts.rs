//! Artifacts and the activity an agent logs: the six tools and the routes the Files tab reads.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use buzz_hive_api::App;
use buzz_hive_core::access::{ActorKind, Level, RelayRole};
use buzz_hive_core::testing::{FakeDirectory, MemStore};
use buzz_hive_core::workflow::source::{MemSource, Source, Sources};
use common::{app, call, community, AGENT, HIVE, MEMBER, OWNER};
use serde_json::{json, Value};

const BUILDER: &str = "b1";
const CHANNEL: &str = "00000000-0000-0000-0000-00000000004d";
const SHA: &str = "abababababababababababababababababababababababababababababababab";

fn docs() -> Arc<Sources> {
    let stage = |id: &str, extra: &str| {
        format!("  - id: {id}\n    kind: prompt\n    agent: builder\n    prompt: Do {id}\n{extra}")
    };
    let stages = [
        stage(
            "draft",
            "    outputs:\n      - { artifact: plan }\n      - { artifact: logo, optional: true }\n    repeat:\n      until: plan_complete\n      plan: todo\n      max_rounds: 3\n",
        ),
        stage("review", "    inputs:\n      - { name: plan, required: true }\n"),
    ]
    .concat();
    let files = MemSource::new()
        .with("docs/kiln-package.yaml", "name: docs\nversion: 1.0.0\n")
        .with(
            "docs/workflow.yaml",
            format!("version: 2\nstages:\n{stages}"),
        );
    Arc::new(Sources(vec![Source::new("test", false, files)]))
}

/// A card whose run is at `draft`, worked by `builder`.
async fn running() -> (App, String) {
    let channel = uuid::Uuid::parse_str(CHANNEL).unwrap();
    let directory = FakeDirectory::new()
        .with_role(OWNER, RelayRole::Owner)
        .with_role(MEMBER, RelayRole::Member)
        .with_agent(AGENT, OWNER)
        .with_named_agent("builder", BUILDER, OWNER)
        .with_channel(channel, &[HIVE, BUILDER]);
    let base = app(MemStore::new()).await;
    let app = App {
        directory: Arc::new(directory),
        catalog: docs(),
        ..base
    };
    let mut tx = app.store.begin(community()).await.unwrap();
    tx.access()
        .set_level(BUILDER, Level::Run, ActorKind::Agent, OWNER)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    call(
        &app,
        "SetSetting",
        OWNER,
        json!({"scope": "user", "key": "update_channel", "valueJson": format!("\"{CHANNEL}\"")}),
    )
    .await;
    let (_, card) = call(
        &app,
        "CreateCard",
        OWNER,
        json!({"title": "Write the docs", "workflow": "docs"}),
    )
    .await;
    let card = card["id"].as_str().unwrap().to_owned();
    let (status, run) = call(&app, "StartStage", OWNER, json!({"cardId": card})).await;
    assert_eq!(status, StatusCode::OK, "{run}");
    (app, card)
}

async fn ok(app: &App, method: &str, who: &str, body: Value) -> Value {
    let (status, got) = call(app, method, who, body).await;
    assert_eq!(status, StatusCode::OK, "{method}: {got}");
    got
}

#[tokio::test]
async fn the_agent_saves_documents_and_the_card_lists_and_reads_them() {
    let (app, card) = running().await;
    let saved = ok(
        &app,
        "SaveArtifact",
        BUILDER,
        json!({"cardId": card, "name": "plan", "content": "- [ ] one\n- [ ] two"}),
    )
    .await;
    assert_eq!(
        (saved["name"].as_str(), saved["version"].as_i64()),
        (Some("plan"), Some(1))
    );
    assert_eq!(saved["kind"], "text");
    assert_eq!(saved["stageId"], "draft");
    assert_eq!(saved["sha256"].as_str().unwrap().len(), 64);
    // A save answers without the body it was given.
    assert!(saved.get("content").is_none() || saved["content"] == "");
    ok(
        &app,
        "SaveArtifact",
        BUILDER,
        json!({"cardId": card, "name": "plan", "content": "- [x] one\n- [ ] two"}),
    )
    .await;

    // Anyone who can read the board lists and reads.
    let listed = ok(&app, "ListArtifacts", MEMBER, json!({"cardId": card})).await;
    assert_eq!(
        listed["artifacts"],
        json!([{"name": "plan", "latest": 2, "handoff": true, "kind": "text"}])
    );
    let versions = ok(
        &app,
        "ListArtifactVersions",
        MEMBER,
        json!({"cardId": card, "name": "plan"}),
    )
    .await;
    let versions = versions["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 2);
    assert!(versions
        .iter()
        .all(|v| v.get("content").is_none() || v["content"] == ""));
    let latest = ok(
        &app,
        "ReadArtifact",
        MEMBER,
        json!({"cardId": card, "name": "plan"}),
    )
    .await;
    assert_eq!(latest["content"], "- [x] one\n- [ ] two");
    assert_eq!(latest["version"], 2);
    let first = ok(
        &app,
        "ReadArtifact",
        MEMBER,
        json!({"cardId": card, "name": "plan", "version": 1}),
    )
    .await;
    assert_eq!(first["content"], "- [ ] one\n- [ ] two");

    let (status, body) = call(
        &app,
        "ReadArtifact",
        MEMBER,
        json!({"cardId": card, "name": "plan", "version": 9}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn a_file_is_saved_as_a_pointer_and_listed_with_the_card_files() {
    let (app, card) = running().await;
    let url = "https://media.example.org/logo.png";
    let saved = ok(
        &app,
        "SaveArtifact",
        BUILDER,
        json!({
            "cardId": card, "name": "logo", "kind": "file",
            "sha256": SHA, "size": 2048, "mediaType": "image/png", "mediaRef": url,
        }),
    )
    .await;
    assert_eq!(saved["kind"], "file");
    assert_eq!(saved["mediaRef"], url);
    assert_eq!(saved["size"], "2048");
    let files = ok(&app, "ListCardFiles", MEMBER, json!({"cardId": card})).await;
    let files = files["files"].as_array().unwrap();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(files[0]["name"], "logo");
    assert_eq!(files[0]["url"], url);
    assert_eq!(files[0]["state"], "saved");
    assert_eq!(files[0]["mediaType"], "image/png");
    // A document is not a file.
    ok(
        &app,
        "SaveArtifact",
        BUILDER,
        json!({"cardId": card, "name": "plan", "content": "x"}),
    )
    .await;
    let files = ok(&app, "ListCardFiles", MEMBER, json!({"cardId": card})).await;
    assert_eq!(files["files"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_save_that_breaks_a_rule_is_invalid_and_one_by_someone_else_is_refused() {
    let (app, card) = running().await;
    for (body, who, status) in [
        // A name the stage does not declare.
        (
            json!({"cardId": card, "name": "other", "content": "x"}),
            BUILDER,
            StatusCode::BAD_REQUEST,
        ),
        // An unknown kind.
        (
            json!({"cardId": card, "name": "plan", "kind": "pdf"}),
            BUILDER,
            StatusCode::BAD_REQUEST,
        ),
        // A file with text.
        (
            json!({"cardId": card, "name": "logo", "kind": "file", "content": "x", "sha256": SHA, "size": 1, "mediaType": "image/png", "mediaRef": "r"}),
            BUILDER,
            StatusCode::BAD_REQUEST,
        ),
        // A file with a bad hash.
        (
            json!({"cardId": card, "name": "logo", "kind": "file", "sha256": "xx", "size": 1, "mediaType": "image/png", "mediaRef": "r"}),
            BUILDER,
            StatusCode::BAD_REQUEST,
        ),
        // No card.
        (
            json!({"name": "plan", "content": "x"}),
            BUILDER,
            StatusCode::BAD_REQUEST,
        ),
        // Not the stage's agent.
        (
            json!({"cardId": card, "name": "plan", "content": "x"}),
            AGENT,
            StatusCode::FORBIDDEN,
        ),
        // A reader may not save.
        (
            json!({"cardId": card, "name": "plan", "content": "x"}),
            MEMBER,
            StatusCode::FORBIDDEN,
        ),
    ] {
        let (got, reply) = call(&app, "SaveArtifact", who, body.clone()).await;
        assert_eq!(got, status, "{who} {body}: {reply}");
    }
    let listed = ok(&app, "ListArtifacts", MEMBER, json!({"cardId": card})).await;
    // An empty list is left out of the JSON.
    assert!(listed.get("artifacts").is_none(), "{listed}");
}

#[tokio::test]
async fn a_person_sets_handoff_and_an_agent_may_not() {
    let (app, card) = running().await;
    ok(
        &app,
        "SaveArtifact",
        BUILDER,
        json!({"cardId": card, "name": "plan", "content": "x"}),
    )
    .await;
    let info = ok(
        &app,
        "SetHandoff",
        OWNER,
        json!({"cardId": card, "name": "plan", "handoff": false}),
    )
    .await;
    // false is the default, so the wire leaves it out.
    assert!(!info["handoff"].as_bool().unwrap_or(false));
    let listed = ok(&app, "ListArtifacts", MEMBER, json!({"cardId": card})).await;
    assert!(!listed["artifacts"][0]["handoff"].as_bool().unwrap_or(false));
    let (status, _) = call(
        &app,
        "SetHandoff",
        MEMBER,
        json!({"cardId": card, "name": "plan", "handoff": true}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &app,
        "SetHandoff",
        OWNER,
        json!({"cardId": card, "name": "nope", "handoff": true}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The board hears about both.
    let (_, polled) = call(&app, "PollEvents", MEMBER, json!({})).await;
    let kinds: Vec<&str> = polled["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["type"].as_str())
        .filter(|k| k.starts_with("artifact."))
        .collect();
    assert_eq!(kinds, ["artifact.saved", "artifact.handoff"]);
}

#[tokio::test]
async fn update_plan_ticks_an_item_and_says_how_many_are_done() {
    let (app, card) = running().await;
    ok(
        &app,
        "SaveArtifact",
        BUILDER,
        json!({"cardId": card, "name": "todo", "content": "- [ ] one\n- [ ] two"}),
    )
    .await;
    let got = ok(
        &app,
        "UpdatePlan",
        BUILDER,
        json!({"cardId": card, "item": "two", "done": true}),
    )
    .await;
    assert_eq!(got, json!({"plan": "todo", "checked": 1, "total": 2}));
    let latest = ok(
        &app,
        "ReadArtifact",
        MEMBER,
        json!({"cardId": card, "name": "todo"}),
    )
    .await;
    assert_eq!(latest["content"], "- [ ] one\n- [x] two");
    let (status, body) = call(
        &app,
        "UpdatePlan",
        BUILDER,
        json!({"cardId": card, "item": "three", "done": true}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn logged_work_and_decisions_are_read_back_and_make_the_brief() {
    let (app, card) = running().await;
    let work = ok(
        &app,
        "LogWork",
        BUILDER,
        json!({"cardId": card, "summary": "Wrote the outline.", "detail": "Three parts."}),
    )
    .await;
    assert_eq!(work["entry"]["type"], "progress");
    assert_eq!(work["entry"]["stageId"], "draft");
    assert_eq!(work["entry"]["author"]["id"], BUILDER);
    let decision = ok(
        &app,
        "RecordDecision",
        BUILDER,
        json!({
            "cardId": card, "summary": "Kept it short.", "why": "People skim.",
            "rejected": [{"option": "Long form", "reason": "nobody reads it"}],
        }),
    )
    .await;
    assert_eq!(decision["entry"]["type"], "decision");
    let fields: Value =
        serde_json::from_str(decision["entry"]["fieldsJson"].as_str().unwrap()).unwrap();
    assert_eq!(fields["why"], "People skim.");
    assert_eq!(fields["rejected"][0]["option"], "Long form");

    let history = ok(&app, "ReadHistory", MEMBER, json!({"cardId": card})).await;
    assert_eq!(history["entries"].as_array().unwrap().len(), 2);
    let after = work["entry"]["seq"].clone();
    let rest = ok(
        &app,
        "ReadHistory",
        MEMBER,
        json!({"cardId": card, "afterSeq": after, "limit": 5}),
    )
    .await;
    assert_eq!(rest["entries"].as_array().unwrap().len(), 1);
    let (status, _) = call(
        &app,
        "ReadHistory",
        MEMBER,
        json!({"cardId": card, "limit": 501}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let brief = ok(&app, "GetBrief", MEMBER, json!({"cardId": card})).await;
    let text = brief["text"].as_str().unwrap();
    assert!(text.contains("Kept it short. Why: People skim."), "{text}");
    assert!(text.contains("- Wrote the outline."), "{text}");

    // Only the stage's agent logs.
    let (status, _) = call(
        &app,
        "LogWork",
        AGENT,
        json!({"cardId": card, "summary": "x"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
