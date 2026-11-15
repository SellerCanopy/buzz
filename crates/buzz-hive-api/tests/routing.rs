//! A card through two stages over HTTP: results route it, a person holds, resumes and skips
//! it, and a batch of cards is sorted onto a workflow.

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
const REVIEWER: &str = "b2";
const CHANNEL: &str = "00000000-0000-0000-0000-00000000004d";

fn two() -> Arc<Sources> {
    let files = MemSource::new()
        .with("two/kiln-package.yaml", "name: two\nversion: 1.0.0\n")
        .with(
            "two/workflow.yaml",
            "version: 2\nstages:\n  - id: build\n    kind: prompt\n    agent: builder\n    prompt: \"Build {{issue.title}}\"\n  - id: review\n    kind: prompt\n    agent: reviewer\n    prompt: \"Review {{issue.title}}\"\n",
        )
        .with("one/kiln-package.yaml", "name: one\nversion: 1.0.0\n")
        .with(
            "one/workflow.yaml",
            "version: 2\nstages:\n  - id: only\n    kind: prompt\n    agent: builder\n    prompt: \"Do {{issue.title}}\"\n",
        );
    Arc::new(Sources(vec![Source::new("test", false, files)]))
}

async fn ready() -> App {
    let channel = uuid::Uuid::parse_str(CHANNEL).unwrap();
    let directory = FakeDirectory::new()
        .with_role(OWNER, RelayRole::Owner)
        .with_role(MEMBER, RelayRole::Member)
        .with_agent(AGENT, OWNER)
        .with_named_agent("builder", BUILDER, OWNER)
        .with_named_agent("reviewer", REVIEWER, OWNER)
        .with_channel(channel, &[HIVE, BUILDER, REVIEWER]);
    let base = app(MemStore::new()).await;
    let app = App {
        directory: Arc::new(directory),
        catalog: two(),
        ..base
    };
    let mut tx = app.store.begin(community()).await.unwrap();
    for agent in [BUILDER, REVIEWER] {
        tx.access()
            .set_level(agent, Level::Run, ActorKind::Agent, OWNER)
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();
    let (status, saved) = call(
        &app,
        "SetSetting",
        OWNER,
        json!({"scope": "user", "key": "update_channel", "valueJson": format!("\"{CHANNEL}\"")}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    app
}

async fn card(app: &App, workflow: &str) -> String {
    let (status, card) = call(
        app,
        "CreateCard",
        OWNER,
        json!({"title": "Fix the build", "workflow": workflow}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{card}");
    card["id"].as_str().unwrap().to_owned()
}

/// Whether the outbox has been woken since it was last asked, consuming the wake.
async fn woken(app: &App) -> Option<()> {
    tokio::time::timeout(std::time::Duration::from_millis(50), app.outbox.notified())
        .await
        .ok()
}

async fn state(app: &App, card: &str) -> Value {
    let (_, got) = call(app, "GetCard", MEMBER, json!({"cardId": card})).await;
    got["state"].clone()
}

#[tokio::test]
async fn a_passed_result_moves_the_card_to_the_next_stage() {
    let app = ready().await;
    let card = card(&app, "two").await;
    let (_, run) = call(&app, "StartStage", OWNER, json!({"cardId": card})).await;
    assert_eq!(run["stageId"], "build");
    // The start left a wake for the drain. Take it, so the next one is the report's.
    woken(&app).await.expect("a start wakes the outbox");
    assert!(woken(&app).await.is_none());

    let (status, run) = call(
        &app,
        "StageDone",
        BUILDER,
        json!({"cardId": card, "stage": "build", "visit": 1, "result": "passed", "summary": "built"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(run["state"], "running");
    assert_eq!(run["stageId"], "review");
    assert_eq!(run["steps"].as_array().unwrap().len(), 2);
    assert_eq!(run["steps"][0]["routedTo"], "review");
    // The review task waits in the outbox, so the report wakes the drain to send it.
    woken(&app)
        .await
        .expect("a report that queues a task wakes the outbox");
    assert_eq!(run["steps"][0]["summary"], "built");

    // The reviewer's stage is now the one that may report; the builder's is over.
    let (status, _) = call(
        &app,
        "StageDone",
        BUILDER,
        json!({"cardId": card, "stage": "review", "visit": 1, "result": "passed"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, run) = call(
        &app,
        "StageDone",
        REVIEWER,
        json!({"cardId": card, "stage": "review", "visit": 1, "result": "passed"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(run["state"], "ended");
    assert_eq!(state(&app, &card).await, "done");
}

#[tokio::test]
async fn an_agent_is_refused_holding_resuming_and_skipping_a_card() {
    let app = ready().await;
    let card = card(&app, "two").await;
    call(&app, "StartStage", OWNER, json!({"cardId": card})).await;
    for method in ["PauseCard", "ResumeCard", "SkipStage"] {
        for agent in [BUILDER, AGENT] {
            let (status, body) = call(&app, method, agent, json!({"cardId": card})).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method}: {body}");
            assert_eq!(body["code"], "permission_denied", "{method}");
        }
    }
    // The card still runs, and no tool offers these calls to an agent.
    assert_eq!(state(&app, &card).await, "running");
    for name in ["skip_stage", "pause_card", "resume_card"] {
        assert!(
            buzz_hive_api::tools::TOOLS.iter().all(|t| t.name != name),
            "{name} is still a tool"
        );
    }
}

#[tokio::test]
async fn a_person_holds_resumes_and_skips() {
    let app = ready().await;
    let card = card(&app, "two").await;
    call(&app, "StartStage", OWNER, json!({"cardId": card})).await;

    let (status, held) = call(&app, "PauseCard", OWNER, json!({"cardId": card})).await;
    assert_eq!(status, StatusCode::OK, "{held}");
    assert_eq!(held["state"], "waiting");
    let (status, _) = call(
        &app,
        "StageDone",
        BUILDER,
        json!({"cardId": card, "stage": "build", "visit": 1, "result": "passed"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "a held card takes no report");

    let (status, run) = call(&app, "ResumeCard", OWNER, json!({"cardId": card})).await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(run["state"], "running");
    assert_eq!(state(&app, &card).await, "running");

    // A running card cannot be skipped or resumed; a held one can be skipped.
    let (status, _) = call(&app, "SkipStage", OWNER, json!({"cardId": card})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = call(&app, "ResumeCard", OWNER, json!({"cardId": card})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    call(&app, "PauseCard", OWNER, json!({"cardId": card})).await;
    let (status, skipped) = call(&app, "SkipStage", OWNER, json!({"cardId": card})).await;
    assert_eq!(status, StatusCode::OK, "{skipped}");
    let (_, run) = call(
        &app,
        "GetRun",
        MEMBER,
        json!({"runId": held_run(&app, &card).await}),
    )
    .await;
    assert_eq!(run["stageId"], "review");
    assert_eq!(run["steps"][0]["skipped"], true);
}

async fn held_run(app: &App, card: &str) -> String {
    let (_, listed) = call(app, "ListRuns", MEMBER, json!({"cardId": card})).await;
    listed["runs"][0]["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn sorting_cards_is_all_or_nothing() {
    let app = ready().await;
    let a = card(&app, "").await;
    let b = card(&app, "").await;

    let (status, body) = call(
        &app,
        "SetCardWorkflow",
        OWNER,
        json!({"moves": [{"cardId": a, "workflow": "one"}, {"cardId": b, "workflow": "nowhere"}]}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (_, got) = call(&app, "GetCard", MEMBER, json!({"cardId": a})).await;
    assert_eq!(
        got["workflow"].as_str().unwrap_or_default(),
        "",
        "the refused batch moved nothing"
    );

    let (status, body) = call(
        &app,
        "SetCardWorkflow",
        OWNER,
        json!({"moves": [{"cardId": b, "workflow": "two"}, {"cardId": a, "workflow": "one"}]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["cards"][0]["id"], b);
    assert_eq!(body["cards"][0]["workflow"], "two");
    assert_eq!(body["cards"][1]["workflow"], "one");

    let (status, _) = call(&app, "SetCardWorkflow", OWNER, json!({"moves": []})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_agent_may_not_move_a_card_unless_the_workflow_allows_it() {
    let app = ready().await;
    let card = card(&app, "two").await;
    call(&app, "StartStage", OWNER, json!({"cardId": card})).await;
    let (status, body) = call(
        &app,
        "MoveCard",
        BUILDER,
        json!({"cardId": card, "stageId": "review", "stage": "build", "visit": 1}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body["message"]
        .as_str()
        .unwrap()
        .contains("agent_moves_cards"));
}

#[tokio::test]
async fn a_move_without_its_stage_and_visit_is_refused_before_anything_else() {
    let app = ready().await;
    let card = card(&app, "two").await;
    call(&app, "StartStage", OWNER, json!({"cardId": card})).await;
    for body in [
        json!({"cardId": card, "stageId": "review"}),
        json!({"cardId": card, "stageId": "review", "stage": "build"}),
        json!({"cardId": card, "stageId": "review", "stage": "build", "visit": 0}),
        json!({"cardId": card, "stageId": "review", "visit": 1}),
    ] {
        let (status, got) = call(&app, "MoveCard", BUILDER, body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {got}");
        assert_eq!(got["code"], "invalid_argument");
    }
}

#[tokio::test]
async fn get_card_returns_the_stage_threads_in_visit_order_and_the_stage_it_is_in() {
    let app = ready().await;
    let id = card(&app, "two").await;
    let (_, before) = call(&app, "GetCard", MEMBER, json!({"cardId": id})).await;
    assert!(before.get("stages").is_none(), "no thread before a run");
    assert!(before.get("stageId").is_none());

    call(&app, "StartStage", OWNER, json!({"cardId": id})).await;
    call(
        &app,
        "StageDone",
        BUILDER,
        json!({"cardId": id, "stage": "build", "visit": 1, "result": "passed"}),
    )
    .await;
    // The drain records a thread when the relay takes the post. Stage names sort the other
    // way from the order they were posted in, and the channel of the first differs.
    let card_id = uuid::Uuid::parse_str(&id).unwrap();
    let first_channel = uuid::Uuid::from_u128(0x99);
    let mut tx = app.store.begin(community()).await.unwrap();
    for (stage, visit, root, agent, channel) in [
        ("build", 1, "r1", BUILDER, first_channel),
        (
            "review",
            1,
            "r2",
            REVIEWER,
            uuid::Uuid::parse_str(CHANNEL).unwrap(),
        ),
    ] {
        tx.runs()
            .record_thread(&buzz_hive_core::ports::store::StageThread {
                card_id,
                stage: stage.into(),
                visit,
                root_event_id: root.into(),
                agent_pubkey: agent.into(),
                channel_id: channel,
            })
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();

    let (status, got) = call(&app, "GetCard", MEMBER, json!({"cardId": id})).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["stageId"], "review");
    let stages = got["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 2);
    assert_eq!(
        stages[0],
        json!({
            "stage": "build", "visit": 1, "rootEventId": "r1", "agentPubkey": BUILDER,
            "channelId": first_channel.to_string(),
        })
    );
    assert_eq!(stages[1]["stage"], "review");
    assert_eq!(stages[1]["channelId"], CHANNEL);
    // The board's list does not carry them.
    let (_, list) = call(&app, "ListCards", MEMBER, json!({})).await;
    assert!(list["cards"][0].get("stages").is_none());
}
