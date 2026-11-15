//! The real `buzz-hive` process is killed in the middle of a run and started again. The run
//! carries on: its posts go out once, the agent that finishes its stage afterwards is heard
//! and the card ends `done`, and a stage timeout that fell due while the service was down is
//! carried out when it comes back.
//!
//! Needs a local relay and the database it uses:
//!
//! ```text
//! BUZZ_TEST_DATABASE_URL=postgres://buzz:buzz_dev@localhost:5432/buzz \
//! BUZZ_TEST_RELAY_URL=ws://localhost:3000 \
//! BUZZ_TEST_COMMUNITY_HOST=localhost \
//! cargo nextest run -p buzz-hive --run-ignored all --test e2e_restart
//! ```
//!
//! The tests add a channel, an owner and an agent with random names to that database and
//! remove them at the end. They change the community's update channel while they run and set
//! it back.

mod support;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use buzz_hive_api::replay::{ReplayGuard, REPLAY_TTL};
use buzz_hive_api::token::{SystemClock, TokenMinter};
use buzz_hive_api::{ApiConfig, App};
use buzz_hive_core::access::{ActorKind, Level};
use buzz_hive_core::bus::Bus;
use buzz_hive_core::workflow::source::{MemSource, Source, Sources};
use buzz_hive_store::pg::PgStore;
use buzz_hive_store::relay_read::RelayRead;
use buzz_ws_client::{NostrWsConnection, RelayMessage};
use futures_util::FutureExt;
use serde_json::{json, Value};
use support::script_agent::{ScriptAgent, Step};
use support::{call, World};
use tokio::process::{Child, Command};
use uuid::Uuid;

const TITLE: &str = "e2e restart";
const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";

#[tokio::test]
#[ignore = "requires local relay"]
async fn a_run_carries_on_when_the_service_is_killed_in_the_middle_of_a_stage() {
    with_world(|world, process, app| Box::pin(finishes_after_a_kill(world, process, app))).await;
}

#[tokio::test]
#[ignore = "requires local relay"]
async fn a_timeout_that_fell_due_while_the_service_was_down_is_carried_out_on_restart() {
    with_world(|world, process, app| Box::pin(times_out_after_a_kill(world, process, app))).await;
}

type Scenario = for<'a> fn(
    &'a World,
    &'a mut Process,
    &'a App,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + 'a>>;

/// Seeds the world, runs `scenario` against the real service, and cleans up.
async fn with_world(scenario: Scenario) {
    let world = World::seed().await;
    let result = std::panic::AssertUnwindSafe(async {
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join("e2e");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join("kiln-package.yaml"),
            "name: e2e\nversion: 1.0.0\n",
        )
        .unwrap();
        std::fs::write(
            package.join("workflow.yaml"),
            format!(
                "version: 2\nstages:\n  - id: research\n    name: Research\n    kind: prompt\n    agent: {}\n    prompt: \"Research {{{{issue.title}}}}\"\n",
                world.agent_name
            ),
        )
        .unwrap();
        let address = free_address().await;
        let mut process = Process::new(&world, address, dir.path());
        let app = in_process(&world, address);
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
        let (_, before) = call(&app, "GetSettings", &owner, json!({"scope": "user"})).await;
        let previous: std::collections::BTreeMap<String, String> = before["settings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["source"]["layer"] == "user")
            .map(|s| {
                (
                    s["key"].as_str().unwrap().to_owned(),
                    s["valueJson"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        let outcome = std::panic::AssertUnwindSafe(scenario(&world, &mut process, &app))
            .catch_unwind()
            .await;
        process.kill().await;
        // Put the settings the test changed back.
        for key in ["update_channel", "execution.timeout"] {
            let value = previous.get(key).cloned().unwrap_or_default();
            let restore = json!({"scope": "user", "key": key, "valueJson": value});
            let _ = call(&app, "SetSetting", &owner, restore).await;
        }
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    })
    .catch_unwind()
    .await;
    world.cleanup(TITLE).await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

/// An address the service can keep across restarts, so the shim's URL stays good.
async fn free_address() -> SocketAddr {
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    probe.local_addr().unwrap()
}

/// The routes in this process, on the same database and secret as the service. A call here is
/// the same as a call to the service: both read and write the one store.
fn in_process(world: &World, address: SocketAddr) -> App {
    App {
        config: Arc::new(ApiConfig {
            public_url: format!("http://{address}"),
            allowed_origins: Vec::new(),
        }),
        store: Arc::new(PgStore::new(world.pool.clone())),
        directory: Arc::new(RelayRead::new(world.pool.clone())),
        minter: Arc::new(TokenMinter::new(SECRET, Arc::new(SystemClock)).unwrap()),
        replay: Arc::new(ReplayGuard::new(REPLAY_TTL)),
        auth_limit: buzz_hive_api::rate::auth_limiter(),
        bus: Bus::new(0),
        catalog: Arc::new(Sources(vec![Source::new("e2e", false, MemSource::new())])),
        community: world.community,
        hive_pubkey: world.hive.public_key().to_hex(),
        outbox: Arc::new(tokio::sync::Notify::new()),
        timers: Arc::new(tokio::sync::Notify::new()),
    }
}

/// The real `buzz-hive` binary, started and killed as often as a test needs.
struct Process {
    child: Option<Child>,
    command: Command,
    address: SocketAddr,
}

impl Process {
    fn new(world: &World, address: SocketAddr, workflows: &std::path::Path) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_buzz-hive"));
        command
            .env(
                "BUZZ_HIVE_DATABASE_URL",
                std::env::var("BUZZ_TEST_DATABASE_URL").unwrap(),
            )
            .env("BUZZ_HIVE_LISTEN", address.to_string())
            .env("BUZZ_HIVE_PUBLIC_URL", format!("http://{address}"))
            .env(
                "BUZZ_HIVE_TOKEN_SECRET",
                std::str::from_utf8(SECRET).unwrap(),
            )
            .env(
                "BUZZ_HIVE_COMMUNITY_HOST",
                support::setting("BUZZ_TEST_COMMUNITY_HOST", "localhost"),
            )
            .env("BUZZ_HIVE_WORKFLOWS_DIR", workflows)
            .env(
                "BUZZ_HIVE_NOSTR_KEY",
                world.hive.secret_key().to_secret_hex(),
            )
            .env("BUZZ_HIVE_RELAY_URL", &world.relay_url)
            .kill_on_drop(true);
        Self {
            child: None,
            command,
            address,
        }
    }

    /// Starts the service and waits until it accepts connections.
    async fn start(&mut self) {
        assert!(self.child.is_none(), "the service is already running");
        self.child = Some(self.command.spawn().expect("start buzz-hive"));
        for _ in 0..200 {
            if tokio::net::TcpStream::connect(self.address).await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("buzz-hive did not listen on {}", self.address);
    }

    /// SIGKILL: no shutdown, no drain of what is in flight.
    async fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
        }
    }

    fn url(&self) -> String {
        format!("http://{}", self.address)
    }
}

async fn set_update_channel(app: &App, world: &World) {
    let owner = world.owner.public_key().to_hex();
    let (status, saved) = call(
        app,
        "SetSetting",
        &owner,
        json!({"scope": "user", "key": "update_channel", "valueJson": format!("\"{}\"", world.channel)}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
}

/// Creates a card and starts its run through the shim, as an agent would.
async fn start_card(world: &World, script: &mut ScriptAgent) -> String {
    // A person asks the agent, as a real turn begins. The agent acts for that person.
    script
        .ask(&world.owner, &world.relay_url, "please take this card")
        .await;
    let (replies, _) = script
        .play(vec![
            Step::Call(
                "create_card",
                json!({"title": TITLE, "description": "It is red", "workflow": "e2e"}),
            ),
            Step::Call("start_run", json!({"cardId": "$card"})),
        ])
        .await;
    assert!(replies.iter().all(|r| !r.is_error), "{replies:?}");
    assert_eq!(replies[1].json()["state"], "running");
    replies[0].json()["id"].as_str().unwrap().to_owned()
}

fn finishes_after_a_kill<'a>(
    world: &'a World,
    process: &'a mut Process,
    app: &'a App,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let owner = world.owner.public_key().to_hex();
        set_update_channel(app, world).await;
        process.start().await;
        let mut script = ScriptAgent::start(
            world.agent.clone(),
            &world.relay_url,
            &process.url(),
            world.channel,
        )
        .await;
        let card = start_card(world, &mut script).await;

        // Killed with the stage open, possibly before its post went out.
        process.kill().await;
        process.start().await;
        wait_for_drained(app).await;
        let posts = stage_posts(world).await;
        assert_eq!(posts, 1, "the stage was posted {posts} times");

        // And again, so a restart that finds nothing to send is covered too.
        process.kill().await;
        process.start().await;
        let (replies, _) = script
            .play(vec![Step::Call(
                "stage_done",
                json!({"cardId": "$card", "stage": "research", "visit": 1, "result": "passed", "summary": "after the restart"}),
            )])
            .await;
        assert!(!replies[0].is_error, "{replies:?}");
        assert_eq!(replies[0].json()["state"], "ended");
        let (_, got) = call(app, "GetCard", &owner, json!({"cardId": card})).await;
        assert_eq!(got["state"], "done", "{got}");
        wait_for_drained(app).await;

        // Each post was queued once, and the relay still holds one stage post.
        let rows = outbox(app).await;
        let mut keys: Vec<&String> = rows.iter().map(|(key, _)| key).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), rows.len(), "a post was queued twice: {rows:?}");
        assert_eq!(stage_posts(world).await, 1);
        script.finish().await;
    })
}

fn times_out_after_a_kill<'a>(
    world: &'a World,
    process: &'a mut Process,
    app: &'a App,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let owner = world.owner.public_key().to_hex();
        set_update_channel(app, world).await;
        let (status, saved) = call(
            app,
            "SetSetting",
            &owner,
            json!({"scope": "user", "key": "execution.timeout", "valueJson": "\"3s\""}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        process.start().await;
        let mut script = ScriptAgent::start(
            world.agent.clone(),
            &world.relay_url,
            &process.url(),
            world.channel,
        )
        .await;
        let card = start_card(world, &mut script).await;

        // The service is down while the stage's three seconds run out.
        process.kill().await;
        tokio::time::sleep(Duration::from_secs(5)).await;
        let (_, waiting) = call(app, "GetCard", &owner, json!({"cardId": card})).await;
        assert_eq!(
            waiting["state"], "running",
            "nothing fires without the service"
        );

        process.start().await;
        // The stage has no retries and its agent reported nothing, so the timeout asks a
        // person (a "no result" item) instead of failing the card.
        let mut got = Value::Null;
        for _ in 0..60 {
            (_, got) = call(app, "GetCard", &owner, json!({"cardId": card})).await;
            if got["state"] == "waiting" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        assert_eq!(got["state"], "waiting", "{got}");
        let (_, inbox) = call(app, "ListInbox", &owner, json!({"cardId": card})).await;
        let items = inbox["items"].as_array().cloned().unwrap_or_default();
        assert_eq!(items.len(), 1, "{inbox}");
        assert_eq!(items[0]["kind"], "no_result", "{inbox}");
        assert_eq!(items[0]["stage"], "research", "{inbox}");
        wait_for_drained(app).await;
        assert_eq!(stage_posts(world).await, 1);
        script.finish().await;
    })
}

/// The outbox as `(key, done)`, oldest first.
async fn outbox(app: &App) -> Vec<(String, bool)> {
    let mut tx = app.store.begin(app.community).await.unwrap();
    tx.outbox()
        .all()
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.key, row.done_at.is_some()))
        .collect()
}

async fn wait_for_drained(app: &App) {
    for _ in 0..120 {
        let rows = outbox(app).await;
        if !rows.is_empty() && rows.iter().all(|(_, done)| *done) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("the outbox did not drain in 60 seconds");
}

/// How many posts in the channel are the stage's task.
async fn stage_posts(world: &World) -> usize {
    read_channel(&world.relay_url, &world.owner, world.channel)
        .await
        .iter()
        .filter(|post| post.content.contains(&format!("Research {TITLE}")))
        .count()
}

/// The messages in `channel`, as the relay returns them to `reader`.
async fn read_channel(relay_url: &str, reader: &nostr::Keys, channel: Uuid) -> Vec<nostr::Event> {
    let mut conn = NostrWsConnection::connect_authenticated(relay_url, reader, None)
        .await
        .expect("connect to the relay");
    conn.send_raw(&json!(["REQ", "e2e", {"kinds": [9], "#h": [channel.to_string()]}]))
        .await
        .unwrap();
    let mut events = Vec::new();
    loop {
        match conn.next_event(Duration::from_secs(15)).await.unwrap() {
            RelayMessage::Event { event, .. } => events.push(*event),
            RelayMessage::Eose { .. } => break,
            RelayMessage::Closed { message, .. } => panic!("the relay closed the read: {message}"),
            _ => {}
        }
    }
    let _ = conn.disconnect().await;
    events
}
