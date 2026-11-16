//! An agent acts at the lower of its own level and the level of the person who asked it.
//!
//! The agent holds Run. A person at Read asks it to start a run and is refused. A person at
//! Run asks, and the run starts for that person. The claim the shim fetched also reaches an
//! outside MCP server, which checks it against the key set Beehive publishes and records who
//! it names.
//!
//! Needs a local relay and the database it uses:
//!
//! ```text
//! BUZZ_TEST_DATABASE_URL=postgres://buzz:buzz_dev@localhost:5432/buzz \
//! BUZZ_TEST_RELAY_URL=ws://localhost:3000 \
//! BUZZ_TEST_COMMUNITY_HOST=localhost \
//! cargo nextest run -p buzz-hive --run-ignored all --test e2e_requester
//! ```
//!
//! The test adds a channel, people and an agent with random names to that database and
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
use serde_json::json;
use support::claim_echo_mcp::{ClaimEcho, Seen};
use support::script_agent::ScriptAgent;
use support::{call, World};

const TITLE: &str = "e2e requester";

#[tokio::test]
#[ignore = "requires local relay"]
async fn a_run_agent_starts_a_run_for_a_run_person_and_not_for_a_read_person() {
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

    // The agent holds Run, and its owner is a relay owner, so it is not capped below that.
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

    // Two people. One is a relay member, so at Read. The other is raised to Run by the owner.
    let reader = world.add_member("reader").await;
    let runner = world.add_member("runner").await;
    let runner_hex = runner.public_key().to_hex();
    let mut tx = app.store.begin(world.community).await.unwrap();
    tx.access()
        .set_level(&runner_hex, Level::Run, ActorKind::Person, owner)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let (status, card) = call(
        app,
        "CreateCard",
        owner,
        json!({"title": TITLE, "description": "It is red", "workflow": "e2e"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{card}");
    let card = card["id"].as_str().unwrap().to_owned();

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

    // The person at Read asks. The agent could start a run on its own level, and may not on
    // theirs.
    script
        .ask(&reader, &world.relay_url, "please start the run")
        .await;
    let (replies, _) = script
        .play(vec![support::script_agent::Step::Call(
            "start_run",
            json!({"cardId": card}),
        )])
        .await;
    assert!(replies[0].is_error, "{replies:?}");
    assert!(
        replies[0].text.starts_with("[permission_denied] "),
        "{}",
        replies[0].text
    );
    let (_, got) = call(app, "GetCard", owner, json!({"cardId": card})).await;
    assert_ne!(got["state"], "running", "no run started: {got}");
    assert!(runs(app, &card).await.is_empty());

    // The person at Run asks, and the run starts for them.
    let asked = script
        .ask(&runner, &world.relay_url, "please start the run")
        .await;
    let (replies, _) = script
        .play(vec![support::script_agent::Step::Call(
            "start_run",
            json!({"cardId": card}),
        )])
        .await;
    assert!(!replies[0].is_error, "{replies:?}");
    assert_eq!(replies[0].json()["state"], "running", "{replies:?}");
    let started = runs(app, &card).await;
    assert_eq!(started.len(), 1, "{started:?}");
    assert_eq!(started[0].as_deref(), Some(runner_hex.as_str()));

    // The claim the shim fetched for that message reaches an outside server, which learns the
    // person from the published key and nothing else. Beehive refuses a claim for a message
    // that is not the agent's to use.
    let (token, _) = app
        .minter
        .mint(&world.agent.public_key().to_hex(), world.community, None)
        .unwrap();
    let http = reqwest::Client::new();
    let reply = http
        .post(format!("{hive_url}{}", buzz_hive_api::CLAIM_PATH))
        .bearer_auth(&token)
        .json(&json!({"event_id": asked.id.to_hex()}))
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), 200);
    let reply: serde_json::Value = reply.json().await.unwrap();
    assert_eq!(reply["requester"], runner_hex.as_str(), "{reply}");
    let echo = ClaimEcho::start(&format!("{hive_url}{}", buzz_hive_api::JWKS_PATH)).await;
    let outside = http
        .post(&echo.url)
        .header("x-hive-claim", reply["claim"].as_str().unwrap())
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "whoami"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(outside.status(), 200);
    assert_eq!(
        echo.seen(),
        [Seen {
            sub: runner_hex.clone(),
            agent: world.agent.public_key().to_hex(),
            via: "chat".into(),
        }]
    );

    script.finish().await;
    let _ = stop.send(true);
    drain_task.await.unwrap();
}

/// Who started each run of the card, oldest first.
async fn runs(app: &App, card: &str) -> Vec<Option<String>> {
    let mut tx = app.store.begin(app.community).await.unwrap();
    tx.runs()
        .list(Some(card.parse().unwrap()))
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.requester)
        .collect()
}
