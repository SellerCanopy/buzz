//! Needs you over HTTP: the items an approval gate and an agent's question open, who may
//! answer them, and the counts the Usage page reads.

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

/// draft saves a plan, gate asks a person to approve it (a rejection goes back to draft), and
/// ship follows.
fn gated() -> Arc<Sources> {
    let files = MemSource::new()
        .with("gated/kiln-package.yaml", "name: gated\nversion: 1.0.0\n")
        .with(
            "gated/workflow.yaml",
            "version: 2\nstages:\n  - id: draft\n    kind: prompt\n    agent: builder\n    prompt: Draft\n    outputs:\n      - { artifact: plan }\n  - id: gate\n    kind: approval\n    approval:\n      artifacts: [plan]\n    on:\n      needs_work: draft\n  - id: ship\n    kind: prompt\n    agent: builder\n    prompt: Ship\n",
        );
    Arc::new(Sources(vec![Source::new("test", false, files)]))
}

/// A running card on `gated`, started by `starter`, and the app it runs in.
async fn running(starter: &str) -> (App, String) {
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
        catalog: gated(),
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
    let (status, card) = call(
        &app,
        "CreateCard",
        starter,
        json!({"title": "Write the docs", "workflow": "gated"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{card}");
    let card = card["id"].as_str().unwrap().to_owned();
    let (status, run) = call(&app, "StartStage", starter, json!({"cardId": card})).await;
    assert_eq!(status, StatusCode::OK, "{run}");
    (app, card)
}

/// The builder saves the plan and reports draft passed, so the card waits at the gate.
async fn at_the_gate() -> (App, String) {
    let (app, card) = running(OWNER).await;
    let (status, body) = call(
        &app,
        "SaveArtifact",
        BUILDER,
        json!({"cardId": card, "name": "plan", "kind": "text", "content": "Step one."}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(
        &app,
        "StageDone",
        BUILDER,
        json!({"cardId": card, "stage": "draft", "visit": 1, "result": "passed"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    (app, card)
}

async fn inbox(app: &App, who: &str, card: &str, resolved: bool) -> Vec<Value> {
    let (status, body) = call(
        app,
        "ListInbox",
        who,
        json!({"cardId": card, "includeResolved": resolved}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["items"].as_array().cloned().unwrap_or_default()
}

async fn state(app: &App, card: &str) -> Value {
    let (_, got) = call(app, "GetCard", MEMBER, json!({"cardId": card})).await;
    got["state"].clone()
}

#[tokio::test]
async fn an_approval_gate_is_listed_for_a_reader_and_approved_by_a_person() {
    let (app, card) = at_the_gate().await;
    // A member who can only read sees it.
    let items = inbox(&app, MEMBER, &card, false).await;
    assert_eq!(items.len(), 1, "{items:?}");
    let item = &items[0];
    assert_eq!(item["kind"], "approval");
    assert_eq!(item["state"], "open");
    assert_eq!(item["stage"], "gate");
    assert_eq!(item["cardTitle"], "Write the docs");
    assert_eq!(item["choices"], json!(["approve", "reject"]));
    assert_eq!(
        serde_json::from_str::<Value>(item["detailJson"].as_str().unwrap()).unwrap()["artifacts"]
            [0]["name"],
        "plan"
    );

    let (status, approved) = call(&app, "Approve", OWNER, json!({"cardId": card})).await;
    assert_eq!(status, StatusCode::OK, "{approved}");
    assert_eq!(approved["state"], "running");
    assert!(inbox(&app, OWNER, &card, false).await.is_empty());
    let resolved = inbox(&app, OWNER, &card, true).await;
    assert_eq!(resolved[0]["state"], "resolved");
    assert_eq!(resolved[0]["answerJson"], "approve");
    assert_eq!(resolved[0]["resolvedBy"], OWNER);

    // A reader may not approve.
    let (status, body) = call(&app, "Approve", MEMBER, json!({"cardId": card})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn sending_a_gate_back_needs_a_reason_and_only_a_person_may() {
    let (app, card) = at_the_gate().await;
    let (status, body) = call(
        &app,
        "SendBack",
        OWNER,
        json!({"cardId": card, "comments": " "}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_argument");

    for agent in [BUILDER, AGENT] {
        let (status, body) = call(
            &app,
            "SendBack",
            agent,
            json!({"cardId": card, "comments": "Not yet."}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{agent}: {body}");
        assert_eq!(body["code"], "permission_denied");
    }
    assert_eq!(inbox(&app, OWNER, &card, false).await.len(), 1);

    let (status, body) = call(
        &app,
        "SendBack",
        OWNER,
        json!({"cardId": card, "comments": "Add a rollback step."}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "running");
    assert!(inbox(&app, OWNER, &card, false).await.is_empty());

    // The reason is in the card's activity, as an answer.
    let (status, listed) = call(
        &app,
        "ListActivity",
        MEMBER,
        json!({"cardId": card, "types": ["answer"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let entries = listed["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{listed}");
    assert_eq!(entries[0]["detail"], "Add a rollback step.");
    assert_eq!(entries[0]["author"]["id"], OWNER);
    let newest: i64 = listed["seq"].as_str().unwrap().parse().unwrap();
    let entry_seq: i64 = entries[0]["seq"].as_str().unwrap().parse().unwrap();
    assert!(newest >= entry_seq, "{listed}");

    // The entry's whole event can be read back.
    let run_id = entries[0]["runId"].as_str().unwrap();
    let seq = entries[0]["eventSeq"].as_str().unwrap();
    let (status, full) = call(
        &app,
        "GetFullPayload",
        MEMBER,
        json!({"runId": run_id, "fullRef": seq}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{full}");
    let payload: Value = serde_json::from_str(full["payloadJson"].as_str().unwrap()).unwrap();
    assert_eq!(payload["detail"], "Add a rollback step.");
}

#[tokio::test]
async fn an_item_is_answered_by_a_person_with_a_choice_it_takes() {
    let (app, card) = at_the_gate().await;
    let id = inbox(&app, OWNER, &card, false).await[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    for agent in [BUILDER, AGENT] {
        let (status, body) = call(
            &app,
            "AnswerInbox",
            agent,
            json!({"itemId": id, "choice": "approve"}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{agent}: {body}");
    }
    let (status, body) = call(
        &app,
        "AnswerInbox",
        MEMBER,
        json!({"itemId": id, "choice": "approve"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let cases = [
        (
            json!({"itemId": id, "choice": "reply", "text": "x"}),
            "not an answer",
        ),
        (json!({"itemId": id, "choice": "reject"}), "reason"),
        (
            json!({"itemId": id, "choice": "approve", "valuesJson": "{}"}),
            "values_json",
        ),
        (
            json!({"itemId": id, "choice": "approve", "answers": ["a"]}),
            "values_json",
        ),
        (json!({"itemId": "nope", "choice": "approve"}), "item_id"),
    ];
    for (body, word) in cases {
        let (status, got) = call(&app, "AnswerInbox", OWNER, body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {got}");
        assert!(
            got["message"].as_str().unwrap().contains(word),
            "{body}: {got}"
        );
    }

    let (status, done) = call(
        &app,
        "AnswerInbox",
        OWNER,
        json!({"itemId": id, "choice": "reject", "text": "Add a rollback step."}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["state"], "resolved");
    assert_eq!(done["answerJson"], "reject");
    // A resolved item takes no answer: the empty list is left out of the JSON.
    assert_eq!(done["choices"], Value::Null);

    let (status, again) = call(
        &app,
        "AnswerInbox",
        OWNER,
        json!({"itemId": id, "choice": "approve"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{again}");
    assert_eq!(again["code"], "failed_precondition");
}

#[tokio::test]
async fn an_agent_asks_a_question_and_a_person_replies() {
    let (app, card) = running(OWNER).await;
    let ask = json!({"cardId": card, "stage": "draft", "visit": 1, "question": "Which database?"});

    // Only the stage's own agent may ask.
    let (status, body) = call(&app, "AskGate", AGENT, ask.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = call(
        &app,
        "AskGate",
        BUILDER,
        json!({"cardId": card, "stage": "draft", "visit": 1, "question": " "}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, run) = call(&app, "AskGate", BUILDER, ask).await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(run["state"], "waiting");
    let items = inbox(&app, MEMBER, &card, false).await;
    assert_eq!(items[0]["kind"], "question");
    assert!(items[0]["title"]
        .as_str()
        .unwrap()
        .contains("Which database?"));

    let (status, answered) = call(
        &app,
        "AnswerInbox",
        OWNER,
        json!({"itemId": items[0]["id"], "choice": "reply", "text": "Postgres."}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answered}");
    assert_eq!(state(&app, &card).await, "running");

    // The person can write to the agent in the stage's thread too, and an agent cannot.
    let (status, sent) = call(
        &app,
        "SendMessage",
        OWNER,
        json!({"cardId": card, "text": "Use staging."}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sent}");
    assert_eq!(sent["messageId"].as_str().unwrap().len(), 32);
    let (status, body) = call(
        &app,
        "SendMessage",
        BUILDER,
        json!({"cardId": card, "text": "hi"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn an_agent_may_approve_a_gate_with_an_artifact_and_is_refused_without_one() {
    let (app, card) = running(OWNER).await;
    // Move the card past draft without a plan: the builder reports passed, a person skips.
    let (status, body) = call(
        &app,
        "StageDone",
        BUILDER,
        json!({"cardId": card, "stage": "draft", "visit": 1, "result": "passed"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let items = inbox(&app, OWNER, &card, false).await;
    assert_eq!(items[0]["kind"], "missing_output");
    let (status, body) = call(&app, "SkipStage", OWNER, json!({"cardId": card})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = call(&app, "Approve", BUILDER, json!({"cardId": card})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("plan"),
        "the refusal names the artifact: {body}"
    );
}

#[tokio::test]
async fn usage_counts_only_what_agents_started_and_has_no_cost_figures() {
    let (app, _card) = running(OWNER).await;
    let (status, usage) = call(&app, "GetUsage", MEMBER, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{usage}");
    assert_eq!(usage["runs"], 1);
    let weeks = usage["weeks"].as_array().unwrap();
    assert_eq!(weeks.len(), 8);
    let now = weeks.last().unwrap();
    // A person created the card and started the run.
    assert_eq!(now["cardsCreatedByAgents"], Value::Null, "{now}");
    assert_eq!(now["runsStartedByAgents"], Value::Null, "{now}");

    // Now an agent creates a card and starts it.
    let (status, card) = call(
        &app,
        "CreateCard",
        BUILDER,
        json!({"title": "From an agent", "workflow": "gated"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{card}");
    let (status, body) = call(&app, "StartStage", BUILDER, json!({"cardId": card["id"]})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, usage) = call(&app, "GetUsage", MEMBER, json!({})).await;
    assert_eq!(usage["runs"], 2);
    let now = usage["weeks"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(now["cardsCreatedByAgents"], 1, "{now}");
    assert_eq!(now["runsStartedByAgents"], 1, "{now}");

    // Counts of activity only: no field names a token or a cost.
    let text = usage.to_string().to_lowercase();
    for word in ["token", "cost", "dollar", "price"] {
        assert!(!text.contains(word), "{word} in {text}");
    }
}

#[tokio::test]
async fn an_agent_with_no_person_behind_it_cannot_turn_on_auto_mode() {
    let (app, card) = at_the_gate().await;
    // The agent holds Run, so the route admits it; the rule is about who it acts for.
    let (status, body) = call(
        &app,
        "SetAutoMode",
        BUILDER,
        json!({"cardId": card, "enabled": true}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("Run or above"),
        "{body}"
    );
    // The setting is the same switch, so it is refused there as well.
    let (status, body) = call(
        &app,
        "SetSetting",
        BUILDER,
        json!({"scope": "card", "scopeId": card, "key": "card.auto_mode", "valueJson": "true"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (_, got) = call(&app, "GetCard", MEMBER, json!({"cardId": card})).await;
    assert_ne!(got["autoMode"], true, "the card stayed out of auto mode");
    assert_eq!(inbox(&app, OWNER, &card, false).await.len(), 1);

    // Turning it off needs no person: nothing is answered without one.
    let (status, body) = call(
        &app,
        "SetAutoMode",
        BUILDER,
        json!({"cardId": card, "enabled": false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn a_reader_cannot_change_auto_mode() {
    let (app, card) = at_the_gate().await;
    let (status, body) = call(
        &app,
        "SetAutoMode",
        MEMBER,
        json!({"cardId": card, "enabled": true}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn a_person_turns_on_auto_mode_and_the_gate_passes_with_its_reason_in_activity() {
    let (app, card) = at_the_gate().await;
    assert_eq!(inbox(&app, OWNER, &card, false).await.len(), 1);

    let (status, got) = call(
        &app,
        "SetAutoMode",
        OWNER,
        json!({"cardId": card, "enabled": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["autoMode"], true);
    assert!(inbox(&app, OWNER, &card, false).await.is_empty());
    assert_eq!(state(&app, &card).await, "running", "ship started");

    // Read level lists the decisions.
    let (status, decisions) =
        call(&app, "ListAutoDecisions", MEMBER, json!({"cardId": card})).await;
    assert_eq!(status, StatusCode::OK, "{decisions}");
    let decisions = decisions["decisions"].as_array().unwrap();
    assert_eq!(decisions.len(), 1, "{decisions:?}");
    assert_eq!(decisions[0]["kind"], "approval");
    assert_eq!(decisions[0]["answer"], "approve");
    assert_eq!(decisions[0]["reason"], "approved plan v1");
    assert!(!decisions[0]["itemId"].as_str().unwrap().is_empty());

    // The entry is in Activity, written by the Beehive account.
    let (status, activity) = call(&app, "ListActivity", MEMBER, json!({"cardId": card})).await;
    assert_eq!(status, StatusCode::OK, "{activity}");
    let entry = activity["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["type"] == "auto")
        .unwrap_or_else(|| panic!("no auto entry: {activity}"));
    assert_eq!(entry["author"]["id"], HIVE);
    assert_eq!(entry["author"]["kind"], "kiln");
    assert_eq!(entry["detail"], "approved plan v1");

    // Turning it off is the same call, and the card reads as out of auto mode.
    let (status, got) = call(
        &app,
        "SetAutoMode",
        OWNER,
        json!({"cardId": card, "enabled": false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_ne!(got["autoMode"], true);
}

#[tokio::test]
async fn the_auto_decisions_of_a_card_that_does_not_exist_are_not_found() {
    let (app, _card) = at_the_gate().await;
    let (status, body) = call(
        &app,
        "ListAutoDecisions",
        OWNER,
        json!({"cardId": "00000000-0000-0000-0000-0000000000ff"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}
