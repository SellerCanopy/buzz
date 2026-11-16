//! A scripted agent creates a card, starts it, receives its stage task and reports `passed`
//! through the real `buzz-hive-mcp` binary. The card ends `done` and the events name the
//! agent as the actor.
//!
//! Needs a local relay and the database it uses:
//!
//! ```text
//! BUZZ_TEST_DATABASE_URL=postgres://buzz:buzz_dev@localhost:5432/buzz \
//! BUZZ_TEST_RELAY_URL=ws://localhost:3000 \
//! BUZZ_TEST_COMMUNITY_HOST=localhost \
//! cargo nextest run -p buzz-hive --run-ignored all --test e2e_one_stage
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

const TITLE: &str = "e2e one stage";

#[tokio::test]
#[ignore = "requires local relay"]
async fn an_agent_creates_starts_and_finishes_a_card_through_the_shim() {
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
    let files = MemSource::new()
        .with("e2e/kiln-package.yaml", "name: e2e\nversion: 1.0.0\n")
        .with(
            "e2e/workflow.yaml",
            format!(
                "version: 2\nstages:\n  - id: research\n    name: Research\n    kind: prompt\n    agent: {}\n    prompt: \"Research {{{{issue.title}}}}\"\n",
                world.agent_name
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
        relay: None,
        outbox: Arc::new(tokio::sync::Notify::new()),
        timers: Arc::new(tokio::sync::Notify::new()),
        code_host: None,
        intake: Arc::new(tokio::sync::Notify::new()),
        supervisor: None,
        secrets: None,
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
        catalog: app.catalog.clone(),
        code_host: None,
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
    assert!(
        ["create_card", "start_run", "stage_done"]
            .iter()
            .all(|t| tools.iter().any(|name| name == t)),
        "{tools:?}"
    );

    let (replies, mentions) = script
        .play(vec![
            Step::Call(
                "create_card",
                json!({"title": TITLE, "description": "It is red", "workflow": "e2e"}),
            ),
            Step::Call("start_run", json!({"cardId": "$card"})),
            Step::AwaitMention,
        ])
        .await;
    assert!(replies.iter().all(|r| !r.is_error), "{replies:?}");
    let card = replies[0].json()["id"].as_str().unwrap().to_owned();
    assert_eq!(replies[1].json()["state"], "running");

    // The task the agent received names the card and the tool to end the stage with.
    let task = &mentions[0];
    assert_eq!(task.pubkey.to_hex(), app.hive_pubkey);
    assert!(
        task.content.contains(&format!("Research {TITLE}")),
        "{}",
        task.content
    );
    assert!(task.content.contains(&card), "{}", task.content);
    assert!(task.content.contains("stage_done"), "{}", task.content);
    assert!(
        task.content.contains("stage `research`, visit 1"),
        "{}",
        task.content
    );

    // A result the stage does not have is refused with its Connect code and the allowed ones.
    let (replies, _) = script
        .play(vec![
            Step::Call("stage_done", json!({"cardId": "$card", "stage": "research", "visit": 1, "result": "great"})),
            Step::Call(
                "stage_done",
                json!({"cardId": "$card", "stage": "research", "visit": 1, "result": "passed", "summary": "scripted"}),
            ),
        ])
        .await;
    assert!(replies[0].is_error, "{replies:?}");
    assert!(
        replies[0].text.starts_with("[invalid_argument] "),
        "{}",
        replies[0].text
    );
    assert!(replies[0].text.contains("passed"), "{}", replies[0].text);
    assert!(!replies[1].is_error, "{replies:?}");
    assert_eq!(replies[1].json()["state"], "ended");

    // The card is done, and the events name the agent.
    let (_, got) = call(app, "GetCard", owner, json!({"cardId": card})).await;
    assert_eq!(got["state"], "done", "{got}");
    let (_, polled) = call(app, "PollEvents", owner, json!({})).await;
    let events: Vec<&Value> = polled["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["cardId"] == card.as_str())
        .collect();
    let by_agent = |kind: &str| {
        events
            .iter()
            .find(|e| e["type"] == kind)
            .unwrap_or_else(|| panic!("no {kind} event: {events:#?}"))
    };
    for kind in [
        "card.created",
        "run.started",
        "result.reported",
        "run.ended",
    ] {
        let event = by_agent(kind);
        assert_eq!(event["actorPubkey"], agent, "{kind}");
        assert_eq!(event["actorKind"], "agent", "{kind}");
    }

    script.finish().await;
    let _ = stop.send(true);
    drain_task.await.unwrap();
}
