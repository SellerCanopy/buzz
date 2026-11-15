//! Starting a run posts the card status and the first stage task to a real relay, and the
//! relay returns both.
//!
//! Needs a local relay and the database it uses:
//!
//! ```text
//! BUZZ_TEST_DATABASE_URL=postgres://buzz:buzz_dev@localhost:5432/buzz \
//! BUZZ_TEST_RELAY_URL=ws://localhost:3000 \
//! BUZZ_TEST_COMMUNITY_HOST=localhost \
//! cargo nextest run -p buzz-hive --run-ignored all --test e2e_start
//! ```
//!
//! The test adds a channel, an owner and an agent with random names to that database and
//! removes them at the end. It changes the community's update channel while it runs and
//! sets it back.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use buzz_hive::drain::Drain;
use buzz_hive::relay_adapter::RelayAdapter;
use buzz_hive_api::replay::{ReplayGuard, REPLAY_TTL};
use buzz_hive_api::routes::path;
use buzz_hive_api::token::{SystemClock, TokenMinter};
use buzz_hive_api::{ApiConfig, App};
use buzz_hive_core::bus::Bus;
use buzz_hive_core::ports::relay::{Delivery, Relay};
use buzz_hive_core::ports::store::{OutboxRow, Posting};
use buzz_hive_core::workflow::source::{MemSource, Source, Sources};
use buzz_hive_store::pg::PgStore;
use buzz_hive_store::relay_read::RelayRead;
use buzz_ws_client::{NostrWsConnection, RelayMessage};
use futures_util::FutureExt;
use nostr::Keys;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

fn setting(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

async fn call(app: &App, method: &str, pubkey: &str, body: Value) -> (StatusCode, Value) {
    let (token, _) = app.minter.mint(pubkey, app.community, None).unwrap();
    let request = Request::post(path(method))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    let router = buzz_hive_api::finish(buzz_hive_api::routes(), app.clone());
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

/// The messages in `channel`, as the relay returns them to `reader`.
async fn read_channel(relay_url: &str, reader: &Keys, channel: Uuid) -> Vec<nostr::Event> {
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

async fn wait_for_outbox(app: &App, rows: usize) -> Vec<OutboxRow> {
    for _ in 0..120 {
        let mut tx = app.store.begin(app.community).await.unwrap();
        let all = tx.outbox().all().await.unwrap();
        if all.len() >= rows && all.iter().all(|r| r.done_at.is_some()) {
            return all;
        }
        drop(tx);
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("the outbox did not drain in 60 seconds");
}

#[tokio::test]
#[ignore = "requires local relay"]
async fn a_started_run_posts_the_status_and_the_task_and_the_relay_returns_both() {
    let database_url = std::env::var("BUZZ_TEST_DATABASE_URL")
        .expect("set BUZZ_TEST_DATABASE_URL to the relay's database");
    let relay_url = setting("BUZZ_TEST_RELAY_URL", "ws://localhost:3000");
    let host = setting("BUZZ_TEST_COMMUNITY_HOST", "localhost");

    let pool = buzz_hive_store::migrate::connect(&database_url)
        .await
        .expect("connect to the database");
    buzz_hive_store::migrate::migrate(&pool).await.unwrap();
    // The relay's own side of the database. `pool` pins `search_path` to `hive`, which the
    // relay's write-fence triggers on `public` cannot resolve from.
    let relay_pool = sqlx::PgPool::connect(&database_url)
        .await
        .expect("connect as the relay");
    let directory = RelayRead::new(pool.clone());
    let community = directory
        .community_for_host(&host)
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("no community has host {host}"));

    // The relay's side: a person, an agent they own, a channel and its members.
    let (hive, owner, builder) = (Keys::generate(), Keys::generate(), Keys::generate());
    let tag = Uuid::new_v4().simple().to_string()[..8].to_owned();
    let agent_name = format!("e2e-builder-{tag}");
    let channel = Uuid::new_v4();
    let bytes = |keys: &Keys| keys.public_key().to_bytes().to_vec();
    for (keys, name, owned) in [
        (&owner, format!("e2e-owner-{tag}"), false),
        (&hive, format!("e2e-hive-{tag}"), false),
        (&builder, agent_name.clone(), true),
    ] {
        sqlx::query(
            "INSERT INTO public.users (community_id, pubkey, display_name, agent_owner_pubkey) \
             VALUES ($1, $2, $3, CASE WHEN $4 THEN $5 END)",
        )
        .bind(community)
        .bind(bytes(keys))
        .bind(name)
        .bind(owned)
        .bind(bytes(&owner))
        .execute(&relay_pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO public.relay_members (community_id, pubkey, role) VALUES ($1, $2, 'owner')",
    )
    .bind(community)
    .bind(owner.public_key().to_hex())
    .execute(&relay_pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO public.channels (id, community_id, name, created_by) VALUES ($1, $2, $3, $4)",
    )
    .bind(channel)
    .bind(community)
    .bind(format!("e2e-{tag}"))
    .bind(bytes(&owner))
    .execute(&relay_pool)
    .await
    .unwrap();
    for keys in [&owner, &hive, &builder] {
        sqlx::query(
            "INSERT INTO public.channel_members (community_id, channel_id, pubkey) \
             VALUES ($1, $2, $3)",
        )
        .bind(community)
        .bind(channel)
        .bind(bytes(keys))
        .execute(&relay_pool)
        .await
        .unwrap();
    }

    let files = MemSource::new()
        .with("e2e/kiln-package.yaml", "name: e2e\nversion: 1.0.0\n")
        .with(
            "e2e/workflow.yaml",
            format!(
                "version: 2\nstages:\n  - id: research\n    name: Research\n    kind: prompt\n    agent: {agent_name}\n    prompt: \"Research {{{{issue.title}}}}\"\n"
            ),
        );
    let adapter = Arc::new(RelayAdapter::new(hive.clone(), relay_url.clone()));
    let app = App {
        config: Arc::new(ApiConfig {
            public_url: "http://hive.test".into(),
            allowed_origins: Vec::new(),
        }),
        store: Arc::new(PgStore::new(pool.clone())),
        directory: Arc::new(directory),
        minter: Arc::new(
            TokenMinter::new(b"0123456789abcdef0123456789abcdef", Arc::new(SystemClock)).unwrap(),
        ),
        replay: Arc::new(ReplayGuard::new(REPLAY_TTL)),
        auth_limit: buzz_hive_api::rate::auth_limiter(),
        bus: Bus::new(0),
        catalog: Arc::new(Sources(vec![Source::new("e2e", false, files)])),
        community,
        hive_pubkey: adapter.pubkey(),
        outbox: Arc::new(tokio::sync::Notify::new()),
        timers: Arc::new(tokio::sync::Notify::new()),
    };
    let me = owner.public_key().to_hex();

    // Remember the update channel to put it back.
    let (_, before) = call(&app, "GetSettings", &me, json!({"scope": "user"})).await;
    let previous = before["settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["key"] == "update_channel" && s["source"]["layer"] == "user")
        .map(|s| s["valueJson"].as_str().unwrap().to_owned())
        .unwrap_or_default();

    let result = std::panic::AssertUnwindSafe(async {
        let (status, saved) = call(
            &app,
            "SetSetting",
            &me,
            json!({"scope": "user", "key": "update_channel", "valueJson": format!("\"{channel}\"")}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        let (status, card) = call(
            &app,
            "CreateCard",
            &me,
            json!({"title": "Fix the build", "description": "It is red", "workflow": "e2e"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{card}");
        let card_id = card["id"].as_str().unwrap().to_owned();

        let (stop, stopped) = tokio::sync::watch::channel(false);
        let drain = Drain {
            store: app.store.clone(),
            directory: app.directory.clone(),
            relay: adapter.clone(),
            bus: app.bus.clone(),
            community,
            hive: app.hive_pubkey.clone(),
            wake: app.outbox.clone(),
        };
        let task = tokio::spawn(drain.run(stopped));

        let (status, run) = call(&app, "StartStage", &me, json!({"cardId": card_id})).await;
        assert_eq!(status, StatusCode::OK, "{run}");
        let rows = wait_for_outbox(&app, 2).await;
        let _ = stop.send(true);
        task.await.unwrap();

        // The relay returns both messages, in the channel, from the Beehive account.
        let events = read_channel(&relay_url, &builder, channel).await;
        assert_eq!(events.len(), 2, "{events:#?}");
        let hive_hex = hive.public_key().to_hex();
        assert!(events.iter().all(|e| e.pubkey.to_hex() == hive_hex));
        let mentions = |e: &nostr::Event| -> Vec<String> {
            e.tags
                .iter()
                .map(|t| t.clone().to_vec())
                .filter(|t| t[0] == "p")
                .map(|t| t[1].clone())
                .collect()
        };
        let status_event = events
            .iter()
            .find(|e| e.content.contains("run started on e2e"))
            .expect("the status post");
        assert!(status_event.content.starts_with("Fix the build: "));
        assert!(mentions(status_event).is_empty());
        let task_event = events
            .iter()
            .find(|e| e.content.contains("Research Fix the build"))
            .expect("the stage task post");
        assert_eq!(mentions(task_event), [builder.public_key().to_hex()]);

        // Sending a post again, as a retry after a crash would, is the same relay event.
        for row in &rows {
            let delivery = Delivery {
                key: row.key.clone(),
                at: row.created_at,
            };
            let again = match &row.posting {
                Posting::Status { channel, text } => {
                    adapter.post_status(*channel, text, &delivery).await
                }
                Posting::StageTask {
                    channel,
                    agent,
                    text,
                    ..
                } => adapter.post_stage_task(*channel, agent, text, &delivery).await,
                other => panic!("a one-stage run queues no {other:?}"),
            }
            .expect("a repeat of an accepted post is accepted");
            assert!(events.iter().any(|e| e.id.to_hex() == again));
        }
        assert_eq!(read_channel(&relay_url, &builder, channel).await.len(), 2);

        // The thread root is recorded for the stage.
        let mut tx = app.store.begin(community).await.unwrap();
        let thread = tx
            .runs()
            .thread(card_id.parse().unwrap(), "research", 1)
            .await
            .unwrap()
            .expect("the stage thread is recorded");
        assert_eq!(thread.root_event_id, task_event.id.to_hex());
    })
    .catch_unwind()
    .await;

    // Put the update channel back and remove what the test added.
    let restore = if previous.is_empty() {
        json!({"scope": "user", "key": "update_channel", "valueJson": ""})
    } else {
        json!({"scope": "user", "key": "update_channel", "valueJson": previous})
    };
    let _ = call(&app, "SetSetting", &me, restore).await;
    let _ = sqlx::query(
        "DELETE FROM hive.cards WHERE community_id = $1 AND title = 'Fix the build' AND created_by = $2",
    )
    .bind(community)
    .bind(&me)
    .execute(&pool)
    .await;
    let _ = sqlx::query("DELETE FROM public.channels WHERE community_id = $1 AND id = $2")
        .bind(community)
        .bind(channel)
        .execute(&relay_pool)
        .await;
    let _ = sqlx::query("DELETE FROM public.relay_members WHERE community_id = $1 AND pubkey = $2")
        .bind(community)
        .bind(&me)
        .execute(&relay_pool)
        .await;
    let _ = sqlx::query("DELETE FROM public.users WHERE community_id = $1 AND pubkey = ANY($2)")
        .bind(community)
        .bind(vec![bytes(&owner), bytes(&hive), bytes(&builder)])
        .execute(&relay_pool)
        .await;
    // Report the failure after the cleanup, not instead of it.
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
