//! Starting a run, reading runs and settings, and the refusals a person can fix.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use buzz_hive_api::App;
use buzz_hive_core::access::RelayRole;
use buzz_hive_core::testing::{FakeDirectory, MemStore};
use buzz_hive_core::workflow::source::{MemSource, Source, Sources};
use common::{app, call, community, AGENT, HIVE, MEMBER, OWNER};
use serde_json::{json, Value};

const BUILDER: &str = "b1";
const CHANNEL: &str = "00000000-0000-0000-0000-00000000004d";

fn channel() -> uuid::Uuid {
    uuid::Uuid::parse_str(CHANNEL).unwrap()
}

/// A package `demo` whose first stage names the agent `builder`.
fn demo() -> Arc<Sources> {
    let files = MemSource::new()
        .with(
            "demo/kiln-package.yaml",
            "name: demo\nversion: 1.0.0\nparameters:\n  - name: depth\n    type: choice\n    choices: [normal, deep]\n    default: normal\n",
        )
        .with(
            "demo/workflow.yaml",
            "version: 2\nstages:\n  - id: research\n    name: Research\n    kind: prompt\n    agent: builder\n    prompt: \"Research {{issue.title}} at depth {{params.depth}}\"\n",
        );
    Arc::new(Sources(vec![Source::new("test", false, files)]))
}

async fn with_directory(directory: FakeDirectory) -> App {
    let base = app(MemStore::new()).await;
    App {
        directory: Arc::new(directory),
        catalog: demo(),
        ..base
    }
}

fn people() -> FakeDirectory {
    FakeDirectory::new()
        .with_role(OWNER, RelayRole::Owner)
        .with_role(MEMBER, RelayRole::Member)
        .with_agent(AGENT, OWNER)
}

async fn make_card(app: &App) -> String {
    let (status, card) = call(
        app,
        "CreateCard",
        OWNER,
        json!({"title": "Fix the build", "description": "It is red", "workflow": "demo"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{card}");
    card["id"].as_str().unwrap().to_owned()
}

async fn set_channel(app: &App) -> (StatusCode, Value) {
    call(
        app,
        "SetSetting",
        OWNER,
        json!({"scope": "user", "key": "update_channel", "valueJson": format!("\"{CHANNEL}\"")}),
    )
    .await
}

/// The layer that holds `update_channel`: `defaults` when no one has set it.
async fn update_channel_layer(app: &App) -> String {
    let (_, settings) = call(app, "GetSettings", MEMBER, json!({"scope": "user"})).await;
    let entry = settings["settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["key"] == "update_channel")
        .unwrap_or_else(|| panic!("update_channel is listed: {settings}"));
    entry["source"]["layer"].as_str().unwrap().to_owned()
}

fn ready() -> FakeDirectory {
    people()
        .with_named_agent("builder", BUILDER, OWNER)
        .with_channel(channel(), &[HIVE, BUILDER])
}

#[tokio::test]
async fn starting_with_no_update_channel_names_the_setting() {
    let app = with_directory(ready()).await;
    let card = make_card(&app).await;
    let (status, body) = call(&app, "StartStage", OWNER, json!({"cardId": card})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "failed_precondition");
    assert!(
        body["message"].as_str().unwrap().contains("update_channel"),
        "{body}"
    );
}

#[tokio::test]
async fn an_unknown_agent_is_named_in_the_refusal() {
    // The channel is fine, but no agent is called builder.
    let directory = people().with_channel(channel(), &[HIVE]);
    let app = with_directory(directory).await;
    let card = make_card(&app).await;
    let (status, body) = set_channel(&app).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(&app, "StartStage", OWNER, json!({"cardId": card})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "failed_precondition");
    assert!(
        body["message"].as_str().unwrap().contains("builder"),
        "{body}"
    );
}

#[tokio::test]
async fn a_channel_the_beehive_account_has_not_joined_returns_the_join_message() {
    let directory = people().with_channel(channel(), &[BUILDER]);
    let app = with_directory(directory).await;
    let (status, body) = set_channel(&app).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "failed_precondition");
    assert_eq!(
        body["message"],
        "add the Beehive account to this channel, then save again"
    );
    assert_eq!(
        update_channel_layer(&app).await,
        "defaults",
        "nothing was saved"
    );
}

#[tokio::test]
async fn a_started_run_is_readable_and_queues_the_status_and_the_task() {
    let app = with_directory(ready()).await;
    let card = make_card(&app).await;
    let (status, saved) = set_channel(&app).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["key"], "update_channel");
    assert_eq!(saved["source"]["layer"], "user");

    let (status, run) = call(&app, "StartStage", AGENT, json!({"cardId": card})).await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(run["state"], "running");
    assert_eq!(run["stageId"], "research");
    assert_eq!(run["cardId"], card);
    assert_eq!(run["visit"], 1);
    let id = run["id"].as_str().unwrap();

    let (_, got) = call(&app, "GetRun", MEMBER, json!({"runId": id})).await;
    assert_eq!(got["id"], id);
    let (_, listed) = call(&app, "ListRuns", MEMBER, json!({"cardId": card})).await;
    assert_eq!(listed["runs"].as_array().unwrap().len(), 1);
    let (_, other) = call(
        &app,
        "ListRuns",
        MEMBER,
        json!({"cardId": "00000000-0000-0000-0000-000000000009"}),
    )
    .await;
    assert!(other
        .get("runs")
        .is_none_or(|r| r.as_array().unwrap().is_empty()));

    let (_, card_now) = call(&app, "GetCard", MEMBER, json!({"cardId": card})).await;
    assert_eq!(card_now["state"], "running");

    // One status line and one stage task wait for the relay, in that order.
    let mut tx = app.store.begin(community()).await.unwrap();
    let rows = tx.outbox().all().await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows[0].key.ends_with(":status"));
    assert!(rows[1].key.ends_with(":research:1"));

    let (_, polled) = call(&app, "PollEvents", MEMBER, json!({})).await;
    let kinds: Vec<_> = polled["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["type"].as_str().unwrap().to_owned())
        .collect();
    assert!(kinds.contains(&"run.started".to_owned()), "{kinds:?}");
    assert!(kinds.contains(&"setting.changed".to_owned()), "{kinds:?}");

    // The card holds the run, so a second start is refused.
    let (status, body) = call(&app, "StartStage", OWNER, json!({"cardId": card})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body["message"]
        .as_str()
        .unwrap()
        .contains("already has a run"));
}

#[tokio::test]
async fn the_update_channel_needs_admin_and_a_run_needs_run() {
    let app = with_directory(ready()).await;
    let card = make_card(&app).await;
    // The agent holds Run, which is not enough for the community's own settings.
    let (status, body) = call(
        &app,
        "SetSetting",
        AGENT,
        json!({"scope": "user", "key": "update_channel", "valueJson": format!("\"{CHANNEL}\"")}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // A Read member can neither change a setting nor start a run.
    for (method, request) in [
        ("StartStage", json!({"cardId": card})),
        (
            "SetSetting",
            json!({"scope": "card", "scopeId": card, "key": "params.depth", "valueJson": "\"deep\""}),
        ),
    ] {
        let (status, body) = call(&app, method, MEMBER, request).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method}: {body}");
    }
    // Run is enough for a card's own setting.
    let (status, body) = call(
        &app,
        "SetSetting",
        AGENT,
        json!({"scope": "card", "scopeId": card, "key": "params.depth", "valueJson": "\"deep\""}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["source"]["layer"], "card");
    // An undeclared value is refused before it is stored.
    let (status, body) = call(
        &app,
        "SetSetting",
        AGENT,
        json!({"scope": "card", "scopeId": card, "key": "params.depth", "valueJson": "\"bogus\""}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn an_empty_value_removes_a_setting() {
    let app = with_directory(ready()).await;
    set_channel(&app).await;
    assert_eq!(update_channel_layer(&app).await, "user");
    let (status, body) = call(
        &app,
        "SetSetting",
        OWNER,
        json!({"scope": "user", "key": "update_channel", "valueJson": ""}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(update_channel_layer(&app).await, "defaults");
}

#[tokio::test]
async fn a_stage_that_is_not_there_and_a_parameter_that_is_not_declared_are_refused() {
    let app = with_directory(ready()).await;
    let card = make_card(&app).await;
    let (status, body) = set_channel(&app).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for request in [
        json!({"cardId": card, "stageId": "nowhere"}),
        json!({"cardId": card, "parameters": {"unknown": "x"}}),
        json!({"cardId": card, "parameters": {"depth": "bogus"}}),
        json!({"cardId": "not-an-id"}),
        json!({}),
    ] {
        let (status, body) = call(&app, "StartStage", OWNER, request.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{request}: {body}");
    }
    // Each refusal left the card idle, so a good start still works.
    let (_, card_now) = call(&app, "GetCard", MEMBER, json!({"cardId": card})).await;
    assert_eq!(card_now["state"], "idle");
}

#[tokio::test]
async fn a_run_starts_at_the_named_stage_with_its_parameters() {
    let app = with_directory(ready()).await;
    let card = make_card(&app).await;
    let (status, body) = set_channel(&app).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, run) = call(
        &app,
        "StartStage",
        OWNER,
        json!({"cardId": card, "stageId": "research", "parameters": {"depth": "deep"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(run["stageId"], "research");
    assert_eq!(run["steps"][0]["stage"], "research");
    assert_eq!(run["steps"][0]["visit"], 1);
}

#[tokio::test]
async fn agents_list_by_name_with_their_owner() {
    let app = with_directory(ready()).await;
    let (status, body) = call(&app, "ListAgents", MEMBER, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["agents"][0]["name"], "builder");
    assert_eq!(body["agents"][0]["pubkey"], BUILDER);
    assert_eq!(body["agents"][0]["ownerPubkey"], OWNER);
}

#[tokio::test]
async fn a_service_with_no_relay_account_says_so() {
    let base = with_directory(ready()).await;
    let app = App {
        hive_pubkey: String::new(),
        ..base
    };
    let card = make_card(&app).await;
    for (method, request) in [
        ("StartStage", json!({"cardId": card})),
        (
            "SetSetting",
            json!({"scope": "user", "key": "update_channel", "valueJson": format!("\"{CHANNEL}\"")}),
        ),
    ] {
        let (status, body) = call(&app, method, OWNER, request).await;
        assert_eq!(status, StatusCode::CONFLICT, "{method}: {body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("BUZZ_HIVE_NOSTR_KEY"),
            "{body}"
        );
    }
}

#[tokio::test]
async fn get_status_counts_what_is_running_and_the_posts_still_waiting() {
    let app = with_directory(ready()).await;
    let (_, before) = call(&app, "GetStatus", MEMBER, json!({})).await;
    assert_eq!(before["running"], serde_json::Value::Null, "{before}");
    assert_eq!(before["outboxPending"], serde_json::Value::Null, "{before}");

    let card = make_card(&app).await;
    let (status, body) = set_channel(&app).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(&app, "StartStage", OWNER, json!({"cardId": card})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Nothing drains the outbox in this test, so the posts the start queued are all waiting.
    let (status, after) = call(&app, "GetStatus", MEMBER, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_eq!(after["running"], "1", "{after}");
    assert!(
        after["outboxPending"]
            .as_str()
            .unwrap()
            .parse::<i64>()
            .unwrap()
            > 0
    );
    assert!(after["headSeq"].as_str().unwrap().parse::<i64>().unwrap() > 0);
    assert_eq!(after["outboxFailed"], serde_json::Value::Null, "{after}");
}
