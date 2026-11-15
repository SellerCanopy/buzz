//! Stopping, retrying, pausing and resetting over HTTP, as the tools and the board call them.

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

/// A running run of `solo`, and its card and run ids. `builder` and the other agent may run.
async fn running() -> (App, String, String) {
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
    let run = run["id"].as_str().unwrap().to_owned();
    (app, card, run)
}

async fn card(app: &App, card: &str) -> Value {
    call(app, "GetCard", MEMBER, json!({"cardId": card}))
        .await
        .1
}

#[tokio::test]
async fn a_person_stops_a_run_and_a_second_stop_has_nothing_to_stop() {
    let (app, card_id, run) = running().await;
    // Read is not enough.
    let (status, body) = call(&app, "StopRun", MEMBER, json!({"runId": run})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, stopped) = call(&app, "StopRun", OWNER, json!({"runId": run})).await;
    assert_eq!(status, StatusCode::OK, "{stopped}");
    assert_eq!(stopped["state"], "ended");
    assert_eq!(card(&app, &card_id).await["state"], "stopped");

    let (status, body) = call(&app, "StopRun", OWNER, json!({"runId": run})).await;
    assert_ne!(status, StatusCode::OK, "{body}");
    assert_eq!(body["code"], "failed_precondition", "{body}");
    let (status, body) = call(&app, "StopRun", OWNER, json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_argument");
}

#[tokio::test]
async fn an_agent_with_run_may_stop_a_run_through_cancel_run() {
    let (app, card_id, run) = running().await;
    let (status, stopped) = call(&app, "StopRun", BUILDER, json!({"runId": run})).await;
    assert_eq!(status, StatusCode::OK, "{stopped}");
    assert_eq!(card(&app, &card_id).await["state"], "stopped");
}

#[tokio::test]
async fn a_stopped_card_retries_as_a_new_run_and_only_when_it_failed_or_stopped() {
    let (app, card_id, run) = running().await;
    // The run is still going, so there is nothing to retry.
    let (status, body) = call(&app, "RetryRun", OWNER, json!({"runId": run})).await;
    assert_ne!(status, StatusCode::OK, "{body}");
    assert_eq!(body["code"], "failed_precondition", "{body}");

    call(&app, "StopRun", OWNER, json!({"runId": run})).await;
    let (status, body) = call(&app, "RetryRun", MEMBER, json!({"runId": run})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = call(
        &app,
        "RetryRun",
        OWNER,
        json!({"runId": run, "overridesJson": {"agent.model": "\"x\""}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, again) = call(&app, "RetryRun", BUILDER, json!({"runId": run})).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_ne!(again["id"], json!(run));
    assert_eq!(again["state"], "running");
    assert_eq!(again["stageId"], "only");
    assert_eq!(again["visit"], 2);
    assert_eq!(again["attempt"], 1);
    assert_eq!(card(&app, &card_id).await["state"], "running");
}

#[tokio::test]
async fn pausing_every_run_is_an_admin_persons_call() {
    let (app, card_id, _run) = running().await;
    // An agent with Run is below Admin, and a Read member is too.
    for who in [BUILDER, AGENT, MEMBER] {
        let (status, body) = call(&app, "PauseAll", who, json!({})).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{who}: {body}");
    }
    let (status, body) = call(&app, "PauseAll", OWNER, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["paused"], 1);
    assert_eq!(card(&app, &card_id).await["state"], "waiting");
    let (_, body) = call(&app, "PauseAll", OWNER, json!({})).await;
    assert!(body["paused"].is_null() || body["paused"] == 0, "{body}");
}

#[tokio::test]
async fn resetting_a_card_stops_its_run_and_leaves_it_idle_for_a_person_only() {
    let (app, card_id, run) = running().await;
    let (status, body) = call(&app, "ResetCard", BUILDER, json!({"cardId": card_id})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = call(&app, "ResetCard", MEMBER, json!({"cardId": card_id})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, reset) = call(&app, "ResetCard", OWNER, json!({"cardId": card_id})).await;
    assert_eq!(status, StatusCode::OK, "{reset}");
    assert_eq!(reset["state"], "idle");
    let got = card(&app, &card_id).await;
    assert_eq!(got["state"], "idle");
    // Idle starts again at the first stage, so no stage is current.
    assert!(got["stageId"].is_null() || got["stageId"] == "", "{got}");
    let (_, runs) = call(&app, "GetRun", MEMBER, json!({"runId": run})).await;
    assert_eq!(runs["state"], "ended");

    // The card starts again from idle.
    let (status, started) = call(&app, "StartStage", OWNER, json!({"cardId": card_id})).await;
    assert_eq!(status, StatusCode::OK, "{started}");
}

#[tokio::test]
async fn a_run_reports_the_attempt_its_stage_is_on() {
    let (app, _card, run) = running().await;
    let (_, got) = call(&app, "GetRun", MEMBER, json!({"runId": run})).await;
    assert_eq!(got["attempt"], 1);
    assert!(got["steps"][0]["failure"].is_null() || got["steps"][0]["failure"] == "");
}
