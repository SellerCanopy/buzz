//! An agent ends a stage: `StageDone` over HTTP, as the tool `stage_done` calls it.

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

fn solo() -> Arc<Sources> {
    let files = MemSource::new()
        .with("solo/kiln-package.yaml", "name: solo\nversion: 1.0.0\n")
        .with(
            "solo/workflow.yaml",
            "version: 2\nstages:\n  - id: only\n    kind: prompt\n    agent: builder\n    prompt: \"Do {{issue.title}}\"\n",
        );
    Arc::new(Sources(vec![Source::new("test", false, files)]))
}

/// A running run of the one-stage workflow `solo`, whose stage is `builder`'s. `builder` may
/// run, and so may the other agent.
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
        catalog: solo(),
        ..base
    };
    let mut tx = app.store.begin(community()).await.unwrap();
    tx.access()
        .set_level(BUILDER, Level::Run, ActorKind::Agent, OWNER)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let (status, saved) = call(
        &app,
        "SetSetting",
        OWNER,
        json!({"scope": "user", "key": "update_channel", "valueJson": format!("\"{CHANNEL}\"")}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let (_, card) = call(
        &app,
        "CreateCard",
        OWNER,
        json!({"title": "Fix the build", "workflow": "solo"}),
    )
    .await;
    let card = card["id"].as_str().unwrap().to_owned();
    let (status, run) = call(&app, "StartStage", OWNER, json!({"cardId": card})).await;
    assert_eq!(status, StatusCode::OK, "{run}");
    (app, card)
}

async fn card_state(app: &App, card: &str) -> Value {
    let (_, got) = call(app, "GetCard", MEMBER, json!({"cardId": card})).await;
    got["state"].clone()
}

#[tokio::test]
async fn the_agent_of_the_stage_ends_the_run_and_the_card_is_done() {
    let (app, card) = running().await;
    let (status, run) = call(
        &app,
        "StageDone",
        BUILDER,
        json!({"cardId": card, "stage": "only", "visit": 1, "result": "passed", "summary": "all green"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(run["state"], "ended");
    assert_eq!(card_state(&app, &card).await, "done");

    // The people watching the board see the three events, from the agent.
    let (_, polled) = call(&app, "PollEvents", MEMBER, json!({})).await;
    let events = polled["events"].as_array().unwrap();
    let ours: Vec<&Value> = events
        .iter()
        .filter(|e| {
            ["result.reported", "run.ended"].contains(&e["type"].as_str().unwrap_or_default())
        })
        .collect();
    assert_eq!(ours.len(), 2, "{events:?}");
    assert!(ours
        .iter()
        .all(|e| e["actorPubkey"] == BUILDER && e["actorKind"] == "agent"));
}

#[tokio::test]
async fn another_agent_with_run_is_refused_with_permission_denied() {
    let (app, card) = running().await;
    for who in [AGENT, OWNER] {
        let (status, body) = call(
            &app,
            "StageDone",
            who,
            json!({"cardId": card, "stage": "only", "visit": 1, "result": "passed"}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{who}: {body}");
        assert_eq!(body["code"], "permission_denied");
        assert!(
            body["message"].as_str().unwrap().contains("agent builder"),
            "{body}"
        );
    }
    assert_eq!(card_state(&app, &card).await, "running");
}

#[tokio::test]
async fn a_result_the_stage_does_not_have_names_the_ones_it_does() {
    let (app, card) = running().await;
    let (status, body) = call(
        &app,
        "StageDone",
        BUILDER,
        json!({"cardId": card, "stage": "only", "visit": 1, "result": "great"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_argument");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("passed, failed, needs_input"),
        "{body}"
    );
    for bad in [
        json!({"cardId": card, "stage": "only", "visit": 1}),
        json!({"cardId": card, "stage": "only", "visit": 1, "result": ""}),
        json!({"cardId": "nope", "stage": "only", "visit": 1, "result": "passed"}),
        json!({"stage": "only", "visit": 1, "result": "passed"}),
        // The stage and the visit are required.
        json!({"cardId": card, "visit": 1, "result": "passed"}),
        json!({"cardId": card, "stage": "", "visit": 1, "result": "passed"}),
        json!({"cardId": card, "stage": "only", "result": "passed"}),
        json!({"cardId": card, "stage": "only", "visit": 0, "result": "passed"}),
        json!({"cardId": card, "stage": "only", "visit": -1, "result": "passed"}),
    ] {
        let (status, body) = call(&app, "StageDone", BUILDER, bad.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }
}

#[tokio::test]
async fn a_report_for_another_stage_or_visit_is_refused_and_the_card_keeps_running() {
    let (app, card) = running().await;
    for (stage, visit) in [("elsewhere", 1), ("only", 2)] {
        let (status, body) = call(
            &app,
            "StageDone",
            BUILDER,
            json!({"cardId": card, "stage": stage, "visit": visit, "result": "passed"}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{stage} {visit}: {body}");
        assert_eq!(body["code"], "failed_precondition");
    }
    assert_eq!(card_state(&app, &card).await, "running");
}

#[tokio::test]
async fn a_second_report_after_the_run_ended_is_refused() {
    let (app, card) = running().await;
    let body = json!({"cardId": card, "stage": "only", "visit": 1, "result": "passed"});
    let (status, _) = call(&app, "StageDone", BUILDER, body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let (status, refused) = call(&app, "StageDone", BUILDER, body).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["code"], "failed_precondition");
    assert_eq!(card_state(&app, &card).await, "done");
}

#[tokio::test]
async fn a_read_member_cannot_report() {
    let (app, card) = running().await;
    let (status, body) = call(
        &app,
        "StageDone",
        MEMBER,
        json!({"cardId": card, "stage": "only", "visit": 1, "result": "passed"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}
