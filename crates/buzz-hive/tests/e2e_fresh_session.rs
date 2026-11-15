//! A card runs through three stages worked by one agent. Each stage opens its own top-level
//! thread, so the agent's session for the second stage holds nothing of the first. A repeat
//! round of the middle stage replies in that stage's thread, and the build stage starts
//! clean again.
//!
//! Needs a local relay and the database it uses:
//!
//! ```text
//! BUZZ_TEST_DATABASE_URL=postgres://buzz:buzz_dev@localhost:5432/buzz \
//! BUZZ_TEST_RELAY_URL=ws://localhost:3000 \
//! BUZZ_TEST_COMMUNITY_HOST=localhost \
//! cargo nextest run -p buzz-hive --run-ignored all --test e2e_fresh_session
//! ```
//!
//! The test adds a channel, an owner and an agent with random names to that database and
//! removes them at the end. It changes the community's update channel while it runs and
//! sets it back.

mod support;

use std::sync::Arc;
use std::time::Duration;

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
use support::script_agent::{thread_of, ScriptAgent, Step};
use support::{call, World};

const TITLE: &str = "e2e fresh session";

#[tokio::test]
#[ignore = "requires local relay"]
async fn each_stage_of_a_card_is_a_fresh_thread_and_a_repeat_round_replies_in_its_thread() {
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
                "version: 2\nstages:\n  - id: scout\n    kind: prompt\n    agent: {name}\n    prompt: \"ALPHA scout {{{{issue.title}}}}\"\n  - id: plan\n    kind: prompt\n    agent: {name}\n    prompt: \"BRAVO plan {{{{issue.title}}}}\"\n    repeat:\n      until: agent_done\n      max_rounds: 3\n  - id: build\n    kind: prompt\n    agent: {name}\n    prompt: \"CHARLIE build {{{{issue.title}}}}\"\n",
                name = world.agent_name
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
    _agent: &str,
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

    // Stage one opens a thread.
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
    let scout = mentions[0].clone();
    assert!(scout.content.contains("ALPHA scout"), "{}", scout.content);

    // Stage two is a new top-level post, not a reply in stage one's thread.
    let (replies, mentions) = script
        .play(vec![
            Step::Call(
                "stage_done",
                json!({"cardId": "$card", "stage": "scout", "visit": 1, "result": "passed", "summary": "SCOUTED-ALPHA"}),
            ),
            Step::AwaitMention,
        ])
        .await;
    assert!(!replies[0].is_error, "{replies:?}");
    assert_eq!(replies[0].json()["stageId"], "plan");
    let plan = mentions[0].clone();
    assert!(plan.content.contains("BRAVO plan"), "{}", plan.content);
    assert_eq!(
        thread_of(&plan),
        plan.id.to_hex(),
        "stage two opens a thread"
    );
    assert_ne!(plan.id, scout.id);

    // A repeat round replies in the plan thread. The agent's session for it holds both rounds.
    let (replies, mentions) = script
        .play(vec![
            Step::Call(
                "stage_done",
                json!({"cardId": "$card", "stage": "plan", "visit": 1, "result": "needs_work", "summary": "SECOND-LOOK"}),
            ),
            Step::AwaitMention,
        ])
        .await;
    assert!(!replies[0].is_error, "{replies:?}");
    let round = mentions[0].clone();
    assert_eq!(
        thread_of(&round),
        plan.id.to_hex(),
        "round two replies in the plan thread"
    );
    assert!(
        round.content.contains("This is round 2"),
        "{}",
        round.content
    );

    // The build stage is the third thread.
    let (replies, mentions) = script
        .play(vec![
            Step::Call(
                "stage_done",
                json!({"cardId": "$card", "stage": "plan", "visit": 1, "result": "passed", "summary": "PLANNED-BRAVO"}),
            ),
            Step::AwaitMention,
        ])
        .await;
    assert!(!replies[0].is_error, "{replies:?}");
    let build = mentions[0].clone();
    assert!(build.content.contains("CHARLIE build"), "{}", build.content);
    assert_eq!(
        thread_of(&build),
        build.id.to_hex(),
        "stage three opens a thread"
    );

    // Three threads, three sessions. Stage two's history has nothing of stage one, and
    // stage three's has nothing of either.
    assert_eq!(script.session_count(), 3);
    let history = |event: &nostr::Event| script.session(&thread_of(event)).join("\n");
    let (alpha, bravo, charlie) = (history(&scout), history(&plan), history(&build));
    assert!(
        alpha.contains("ALPHA") && !alpha.contains("BRAVO"),
        "{alpha}"
    );
    assert!(
        bravo.contains("BRAVO") && bravo.contains("round 2"),
        "{bravo}"
    );
    assert!(
        !bravo.contains("ALPHA") && !bravo.contains("SCOUTED"),
        "{bravo}"
    );
    assert!(charlie.contains("CHARLIE"), "{charlie}");
    assert!(
        !["ALPHA", "BRAVO", "SCOUTED", "PLANNED", "round 2"]
            .iter()
            .any(|text| charlie.contains(text)),
        "{charlie}"
    );
    assert_eq!(script.session(&thread_of(&plan)).len(), 2);

    // The stage_threads rows are the same three roots. The drain records a row after the
    // relay accepts the post, which can be after the agent has read it, so wait for it.
    let rows = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let rows: Vec<(String, i32, String)> = sqlx::query_as(
                "SELECT stage, visit, root_event_id FROM hive.stage_threads \
                 WHERE community_id = $1 AND card_id = $2::uuid ORDER BY stage",
            )
            .bind(world.community)
            .bind(&card)
            .fetch_all(&world.pool)
            .await
            .unwrap();
            if rows.len() == 3 {
                return rows;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the three stage_threads rows were recorded");
    let stages: Vec<_> = rows.iter().map(|(s, v, _)| (s.as_str(), *v)).collect();
    assert_eq!(stages, [("build", 1), ("plan", 1), ("scout", 1)]);
    assert_eq!(rows[0].2, build.id.to_hex());
    assert_eq!(rows[1].2, plan.id.to_hex());
    assert_eq!(rows[2].2, scout.id.to_hex());

    // Each thread's first post, as the relay holds it, ends with the link that opens the card
    // in buzz-app, and GetCard hands the Live tab the threads in the order they opened.
    let link = format!(
        "Open the card in Beehive: buzz://plugin?id=buzz.beehive&page=beehive&route.card={card}"
    );
    for post in [&scout, &plan, &build] {
        assert!(post.content.ends_with(&link), "{}", post.content);
    }
    let (_, got) = call(app, "GetCard", owner, json!({"cardId": card})).await;
    let threads: Vec<_> = got["stages"]
        .as_array()
        .unwrap_or_else(|| panic!("GetCard returned no stages: {got}"))
        .iter()
        .map(|t| {
            (
                t["stage"].as_str().unwrap().to_owned(),
                t["rootEventId"].as_str().unwrap().to_owned(),
                t["channelId"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let channel = world.channel.to_string();
    assert_eq!(
        threads,
        [
            ("scout".to_owned(), scout.id.to_hex(), channel.clone()),
            ("plan".to_owned(), plan.id.to_hex(), channel.clone()),
            ("build".to_owned(), build.id.to_hex(), channel),
        ]
    );
    assert_eq!(got["stageId"], "build");

    let (replies, _) = script
        .play(vec![Step::Call(
            "stage_done",
            json!({"cardId": "$card", "stage": "build", "visit": 1, "result": "passed"}),
        )])
        .await;
    assert_eq!(replies[0].json()["state"], "ended", "{replies:?}");
    let (_, got) = call(app, "GetCard", owner, json!({"cardId": card})).await;
    assert_eq!(got["state"], "done", "{got}");

    script.finish().await;
    let _ = stop.send(true);
    drain_task.await.unwrap();
}
