//! A card in auto mode passes its approval gate with no person. A scripted agent drafts a plan
//! and reports `passed`; its call to `set_auto_mode` is refused while no person stands behind
//! it; once its owner asks it to, the call succeeds, the gate passes by rule with its reason
//! in Activity, and the agent receives the next stage's task and finishes the card.
//!
//! Needs a local relay and the database it uses:
//!
//! ```text
//! BUZZ_TEST_DATABASE_URL=postgres://buzz:buzz_dev@localhost:5432/buzz \
//! BUZZ_TEST_RELAY_URL=ws://localhost:3000 \
//! BUZZ_TEST_COMMUNITY_HOST=localhost \
//! cargo nextest run -p buzz-hive --run-ignored all --test e2e_auto_mode
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

const TITLE: &str = "e2e auto mode";

#[tokio::test]
#[ignore = "requires local relay"]
async fn auto_mode_passes_a_gate_with_no_person_and_an_agent_turns_it_on_only_for_a_person() {
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

    let outcome = std::panic::AssertUnwindSafe(scenario(&app, world, &hive_url, &owner, adapter))
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
        tools.iter().any(|name| name == "set_auto_mode"),
        "set_auto_mode in {tools:?}"
    );

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
    let items = inbox(app, owner, &card).await;
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["kind"], "approval", "{items:?}");

    // The agent has Run, yet no turn is open for it, so no person stands behind its call and
    // it acts at Read: it cannot turn auto mode on.
    script.forget_requester();
    let (replies, _) = script
        .play(vec![Step::Call(
            "set_auto_mode",
            json!({"cardId": "$card", "enabled": true}),
        )])
        .await;
    assert!(replies[0].is_error, "{replies:?}");
    assert!(
        replies[0].text.starts_with("[permission_denied] "),
        "the refusal keeps its code: {}",
        replies[0].text
    );
    assert_eq!(
        inbox(app, owner, &card).await.len(),
        1,
        "the gate still waits"
    );

    // The owner, who holds Admin, asks the agent to turn it on. The agent acts for the owner
    // at its own level, Run, which is enough. The gate lists a saved document, so it passes
    // by rule, and ship's task reaches the agent.
    script
        .ask(
            &world.owner,
            &world.relay_url,
            "turn auto mode on for this card",
        )
        .await;
    let (replies, _) = script
        .play(vec![Step::Call(
            "set_auto_mode",
            json!({"cardId": "$card", "enabled": true}),
        )])
        .await;
    assert!(!replies[0].is_error, "{replies:?}");
    assert_eq!(replies[0].json()["autoMode"], true, "{replies:?}");
    assert!(inbox(app, owner, &card).await.is_empty());
    let (replies, mentions) = script
        .play(vec![
            Step::AwaitMention,
            Step::Call(
                "stage_done",
                json!({"cardId": "$card", "stage": "ship", "visit": 1, "result": "passed"}),
            ),
        ])
        .await;
    assert!(replies.iter().all(|r| !r.is_error), "{replies:?}");
    assert!(
        mentions[0].content.contains("Ship it"),
        "{}",
        mentions[0].content
    );

    // The card is done. The one item was answered by the Beehive account, not a person, and
    // Activity says why.
    let (_, got) = call(app, "GetCard", owner, json!({"cardId": card})).await;
    assert_eq!(got["state"], "done", "{got}");
    let (_, listed) = call(
        app,
        "ListInbox",
        owner,
        json!({"cardId": card, "includeResolved": true}),
    )
    .await;
    let items = listed["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{listed}");
    assert_eq!(items[0]["resolvedBy"], app.hive_pubkey.as_str(), "{listed}");
    assert_eq!(items[0]["answerJson"], "approve", "{listed}");
    let (_, decisions) = call(app, "ListAutoDecisions", owner, json!({"cardId": card})).await;
    assert_eq!(
        decisions["decisions"][0]["reason"], "approved plan v1",
        "{decisions}"
    );
    let (_, activity) = call(
        app,
        "ListActivity",
        owner,
        json!({"cardId": card, "types": ["auto"]}),
    )
    .await;
    let entries = activity["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{activity}");
    assert_eq!(
        entries[0]["author"]["id"],
        app.hive_pubkey.as_str(),
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
