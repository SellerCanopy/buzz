//! A scripted agent drafts a plan and reports `passed`, and the card waits at an approval gate.
//! A person sends it back with a reason, the agent receives the reason in its next task, drafts
//! again, and the person approves. The ship stage then asks a question, the person replies, and
//! the agent receives the reply in the stage thread and finishes the card.
//!
//! Needs a local relay and the database it uses:
//!
//! ```text
//! BUZZ_TEST_DATABASE_URL=postgres://buzz:buzz_dev@localhost:5432/buzz \
//! BUZZ_TEST_RELAY_URL=ws://localhost:3000 \
//! BUZZ_TEST_COMMUNITY_HOST=localhost \
//! cargo nextest run -p buzz-hive --run-ignored all --test e2e_approval
//! ```
//!
//! The test adds a channel, an owner and an agent with random names to that database and
//! removes them at the end. It changes the community's update channel while it runs and
//! sets it back.

mod support;

use std::sync::Arc;

use axum::http::StatusCode;
use buzz_hive::drain::Drain;
use buzz_hive::relay_adapter::RelayAdapter;
use buzz_hive_api::replay::{ReplayGuard, REPLAY_TTL};
use buzz_hive_api::token::{SystemClock, TokenMinter};
use buzz_hive_api::{ApiConfig, App};
use buzz_hive_core::access::{ActorKind, Level};
use buzz_hive_core::bus::Bus;
use buzz_hive_core::workflow::source::{MemSource, Source, Sources};
use buzz_hive_store::pg::PgStore;
use buzz_hive_store::relay_read::RelayRead;
use futures_util::FutureExt;
use serde_json::{json, Value};
use support::script_agent::{ScriptAgent, Step};
use support::{call, World};

const TITLE: &str = "e2e approval";

#[tokio::test]
#[ignore = "requires local relay"]
async fn a_person_sends_a_gate_back_approves_it_and_answers_a_question() {
    let world = World::seed().await;
    let result = std::panic::AssertUnwindSafe(run(&world))
        .catch_unwind()
        .await;
    world.cleanup(TITLE).await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn run(world: &World) {
    let agent_name = &world.agent_name;
    let files = MemSource::new()
        .with("e2e/kiln-package.yaml", "name: e2e\nversion: 1.0.0\n")
        .with(
            "e2e/workflow.yaml",
            format!(
                "version: 2\nstages:\n  - id: draft\n    kind: prompt\n    agent: {agent_name}\n    prompt: \"Draft the plan for {{{{issue.title}}}}\"\n    outputs:\n      - {{ artifact: plan }}\n  - id: gate\n    kind: approval\n    approval:\n      artifacts: [plan]\n    on:\n      needs_work: draft\n  - id: ship\n    kind: prompt\n    agent: {agent_name}\n    prompt: \"Ship it\"\n"
            ),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let hive_url = format!("http://{}", listener.local_addr().unwrap());
    let adapter = Arc::new(RelayAdapter::new(
        world.hive.clone(),
        world.relay_url.clone(),
    ));
    let app = App {
        config: Arc::new(ApiConfig {
            public_url: hive_url.clone(),
            allowed_origins: Vec::new(),
        }),
        store: Arc::new(PgStore::new(world.pool.clone())),
        directory: Arc::new(RelayRead::new(world.pool.clone())),
        minter: Arc::new(
            TokenMinter::new(b"0123456789abcdef0123456789abcdef", Arc::new(SystemClock)).unwrap(),
        ),
        replay: Arc::new(ReplayGuard::new(REPLAY_TTL)),
        auth_limit: buzz_hive_api::rate::auth_limiter(),
        bus: Bus::new(0),
        catalog: Arc::new(Sources(vec![Source::new("e2e", false, files)])),
        community: world.community,
        hive_pubkey: adapter.pubkey(),
        outbox: Arc::new(tokio::sync::Notify::new()),
        timers: Arc::new(tokio::sync::Notify::new()),
        code_host: None,
        intake: Arc::new(tokio::sync::Notify::new()),
    };
    let router = buzz_hive_api::finish(buzz_hive_api::routes(), app.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    // The agent holds Run. Its owner is a relay owner, so it is not capped below that.
    let (agent, owner) = (
        world.agent.public_key().to_hex(),
        world.owner.public_key().to_hex(),
    );
    let mut tx = app.store.begin(world.community).await.unwrap();
    tx.access()
        .set_level(&agent, Level::Run, ActorKind::Agent, &owner)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // Remember the update channel to put it back.
    let (_, before) = call(&app, "GetSettings", &owner, json!({"scope": "user"})).await;
    let previous = before["settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["key"] == "update_channel" && s["source"]["layer"] == "user")
        .map(|s| s["valueJson"].as_str().unwrap().to_owned())
        .unwrap_or_default();

    let outcome =
        std::panic::AssertUnwindSafe(scenario(&app, world, &hive_url, &agent, &owner, adapter))
            .catch_unwind()
            .await;
    let restore = json!({"scope": "user", "key": "update_channel", "valueJson": previous});
    let _ = call(&app, "SetSetting", &owner, restore).await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

async fn scenario(
    app: &App,
    world: &World,
    hive_url: &str,
    agent: &str,
    owner: &str,
    adapter: Arc<RelayAdapter>,
) {
    let (status, saved) = call(
        app,
        "SetSetting",
        owner,
        json!({"scope": "user", "key": "update_channel", "valueJson": format!("\"{}\"", world.channel)}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");

    let (stop, stopped) = tokio::sync::watch::channel(false);
    let drain = Drain {
        store: app.store.clone(),
        directory: app.directory.clone(),
        relay: adapter,
        bus: app.bus.clone(),
        community: world.community,
        hive: app.hive_pubkey.clone(),
        wake: app.outbox.clone(),
    };
    let drain_task = tokio::spawn(drain.run(stopped));

    let mut script = ScriptAgent::start(
        world.agent.clone(),
        &world.relay_url,
        hive_url,
        world.channel,
    )
    .await;
    // A person asks the agent, as a real turn begins. The agent acts for that person.
    script
        .ask(&world.owner, &world.relay_url, "please take this card")
        .await;
    let tools = script.tool_names().await;
    for tool in ["ask_gate", "approve_gate", "get_inbox", "save_artifact"] {
        assert!(tools.iter().any(|name| name == tool), "{tool} in {tools:?}");
    }

    let (replies, mentions) = script
        .play(vec![
            Step::Call(
                "create_card",
                json!({"title": TITLE, "description": "It is red", "workflow": "e2e"}),
            ),
            Step::Call("start_run", json!({"cardId": "$card"})),
            Step::AwaitMention,
            Step::Call(
                "save_artifact",
                json!({"cardId": "$card", "name": "plan", "content": "Step one."}),
            ),
            Step::Call(
                "stage_done",
                json!({"cardId": "$card", "stage": "draft", "visit": 1, "result": "passed"}),
            ),
        ])
        .await;
    assert!(replies.iter().all(|r| !r.is_error), "{replies:?}");
    assert!(
        mentions[0].content.contains("Draft the plan"),
        "{}",
        mentions[0].content
    );
    let card = replies[0].json()["id"].as_str().unwrap().to_owned();

    // The card waits at the gate, and a person can see what it waits for.
    let items = inbox(app, owner, &card).await;
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["kind"], "approval", "{items:?}");
    assert_eq!(items[0]["stage"], "gate", "{items:?}");
    let (_, got) = call(app, "GetCard", owner, json!({"cardId": card})).await;
    assert_eq!(got["state"], "waiting", "{got}");

    // An agent cannot send the gate back; a person does, with a reason.
    let (status, refused) = call(
        app,
        "SendBack",
        agent,
        json!({"cardId": card, "comments": "Looks fine to me."}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    let reason = "Add a rollback step.";
    let (status, sent) = call(
        app,
        "SendBack",
        owner,
        json!({"cardId": card, "comments": reason}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sent}");

    // The agent is told again, and the task carries the reason.
    let (replies, mentions) = script
        .play(vec![
            Step::AwaitMention,
            Step::Call(
                "save_artifact",
                json!({"cardId": "$card", "name": "plan", "content": "Step one. Step two: roll back."}),
            ),
            Step::Call(
                "stage_done",
                json!({"cardId": "$card", "stage": "draft", "visit": 2, "result": "passed"}),
            ),
        ])
        .await;
    assert!(replies.iter().all(|r| !r.is_error), "{replies:?}");
    assert!(
        mentions[0].content.contains(reason),
        "the second task omits the reason: {}",
        mentions[0].content
    );
    assert!(inbox(app, owner, &card).await.len() == 1);

    // The person approves, and the card moves on to ship.
    let (status, approved) = call(app, "Approve", owner, json!({"cardId": card})).await;
    assert_eq!(status, StatusCode::OK, "{approved}");
    let (replies, mentions) = script
        .play(vec![
            Step::AwaitMention,
            Step::Call(
                "ask_gate",
                json!({"cardId": "$card", "stage": "ship", "visit": 1, "question": "Which region?"}),
            ),
        ])
        .await;
    assert!(
        mentions[0].content.contains("Ship it"),
        "{}",
        mentions[0].content
    );
    assert!(!replies[0].is_error, "{replies:?}");

    // The agent's question is an item. A person replies, and the agent hears it in the thread.
    let items = inbox(app, owner, &card).await;
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["kind"], "question", "{items:?}");
    let (status, answered) = call(
        app,
        "AnswerInbox",
        owner,
        json!({"itemId": items[0]["id"], "choice": "reply", "text": "Use eu-west."}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answered}");
    let (replies, mentions) = script
        .play(vec![
            Step::AwaitMention,
            Step::Call(
                "stage_done",
                json!({"cardId": "$card", "stage": "ship", "visit": 1, "result": "passed"}),
            ),
        ])
        .await;
    assert!(
        mentions[0].content.contains("Use eu-west."),
        "{}",
        mentions[0].content
    );
    assert!(!replies[0].is_error, "{replies:?}");

    // The card is done, and nothing is left to answer. Three items were answered.
    let (_, got) = call(app, "GetCard", owner, json!({"cardId": card})).await;
    assert_eq!(got["state"], "done", "{got}");
    assert!(inbox(app, owner, &card).await.is_empty());
    let (_, listed) = call(
        app,
        "ListInbox",
        owner,
        json!({"cardId": card, "includeResolved": true}),
    )
    .await;
    let answers: Vec<&str> = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["answerJson"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(answers.len(), 3, "{listed}");
    for answer in ["reject", "approve", "reply"] {
        assert!(answers.contains(&answer), "{answer} in {answers:?}");
    }
    let (_, activity) = call(
        app,
        "ListActivity",
        owner,
        json!({"cardId": card, "types": ["answer"]}),
    )
    .await;
    assert!(
        activity["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["detail"] == reason && e["author"]["id"] == owner),
        "{activity}"
    );

    script.finish().await;
    let _ = stop.send(true);
    drain_task.await.unwrap();
}

/// The open items of the card.
async fn inbox(app: &App, who: &str, card: &str) -> Vec<Value> {
    let (status, body) = call(app, "ListInbox", who, json!({"cardId": card})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["items"].as_array().cloned().unwrap_or_default()
}
