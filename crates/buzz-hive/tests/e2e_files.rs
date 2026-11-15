//! A file saved by one stage downloads in the next, with the relay refusing media reads to
//! anyone who cannot see the message that holds them.
//!
//! The scripted agent works two stages. In the first it uploads an image with `buzz upload
//! file`, posts it in the stage's thread, and saves it with `save_artifact` as a file next to
//! a document. The second stage's task names both, and the test downloads the image with
//! `buzz media get` from the command in the task. A relay member outside the channel cannot.
//!
//! Needs a local relay started with `BUZZ_MEDIA_READ_MODE=enforce`, its Redis and S3 store,
//! and the database it uses:
//!
//! ```text
//! BUZZ_TEST_DATABASE_URL=postgres://buzz:buzz_dev@localhost:5432/buzz \
//! BUZZ_TEST_RELAY_URL=ws://localhost:3000 \
//! BUZZ_TEST_COMMUNITY_HOST=localhost \
//! cargo nextest run -p buzz-hive --run-ignored all --test e2e_files
//! ```
//!
//! The test adds a channel, an owner, an agent and an outsider with random names to that
//! database and removes them at the end.

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
use nostr::Keys;
use serde_json::{json, Value};
use support::script_agent::{ScriptAgent, Step};
use support::{call, cli, pubkey_bytes, unique_png, World};

const TITLE: &str = "e2e files";

#[tokio::test]
#[ignore = "requires local relay"]
async fn a_file_saved_by_one_stage_downloads_in_the_next_with_media_reads_enforced() {
    let world = World::seed().await;
    let outsider = Keys::generate();
    let result = std::panic::AssertUnwindSafe(run(&world, &outsider))
        .catch_unwind()
        .await;
    let _ = sqlx::query("DELETE FROM public.relay_members WHERE pubkey = $1")
        .bind(outsider.public_key().to_hex())
        .execute(&world.relay_pool)
        .await;
    let _ = sqlx::query("DELETE FROM public.users WHERE pubkey = $1")
        .bind(pubkey_bytes(&outsider))
        .execute(&world.relay_pool)
        .await;
    world.cleanup(TITLE).await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn run(world: &World, outsider: &Keys) {
    // A relay member who is not in the channel.
    sqlx::query(
        "INSERT INTO public.users (community_id, pubkey, display_name) VALUES ($1, $2, $3)",
    )
    .bind(world.community)
    .bind(pubkey_bytes(outsider))
    .bind(format!("e2e-outsider-{}", &world.agent_name[10..]))
    .execute(&world.relay_pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO public.relay_members (community_id, pubkey, role) VALUES ($1, $2, 'member')",
    )
    .bind(world.community)
    .bind(outsider.public_key().to_hex())
    .execute(&world.relay_pool)
    .await
    .unwrap();

    // An open channel is readable by every member, so make this one private.
    sqlx::query("UPDATE public.channels SET visibility = 'private' WHERE id = $1")
        .bind(world.channel)
        .execute(&world.relay_pool)
        .await
        .unwrap();

    let stages = |inputs: &str, outputs: &str| {
        format!(
            "    kind: prompt\n    agent: {}\n    prompt: \"Do it\"\n{inputs}{outputs}",
            world.agent_name
        )
    };
    let files = MemSource::new()
        .with("e2e/kiln-package.yaml", "name: e2e\nversion: 1.0.0\n")
        .with(
            "e2e/workflow.yaml",
            format!(
                "version: 2\nstages:\n  - id: make\n{}  - id: use\n{}",
                stages(
                    "",
                    "    outputs:\n      - { artifact: report }\n      - { artifact: picture }\n"
                ),
                stages(
                    "    inputs:\n      - { name: report, required: true }\n      - { name: picture, required: true }\n",
                    ""
                ),
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
    let previous = before["settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["key"] == "update_channel" && s["source"]["layer"] == "user")
        .map(|s| s["valueJson"].as_str().unwrap().to_owned())
        .unwrap_or_default();

    let outcome =
        std::panic::AssertUnwindSafe(scenario(&app, world, &hive_url, &owner, outsider, adapter))
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
    outsider: &Keys,
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
    for tool in ["save_artifact", "read_artifact", "update_plan", "log_work"] {
        assert!(tools.iter().any(|name| name == tool), "{tools:?}");
    }

    // Stage one: the task says what to save and how.
    let (replies, mentions) = script
        .play(vec![
            Step::Call(
                "create_card",
                json!({"title": TITLE, "description": "Make a picture", "workflow": "e2e"}),
            ),
            Step::Call("start_run", json!({"cardId": "$card"})),
            Step::AwaitMention,
        ])
        .await;
    assert!(replies.iter().all(|r| !r.is_error), "{replies:?}");
    let card = replies[0].json()["id"].as_str().unwrap().to_owned();
    let first = &mentions[0];
    for want in [
        "Outputs to save before you report a result:",
        "- `report`",
        "- `picture`",
        "buzz upload file --file <path>",
        &format!("--channel {}", world.channel),
        "--reply-to <the id of the message that mentioned you>",
    ] {
        assert!(first.content.contains(want), "{want}: {}", first.content);
    }

    // The agent uploads the image and posts it in its stage's thread.
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("picture.png");
    let bytes = unique_png();
    std::fs::write(&image, &bytes).unwrap();
    let image_path = image.to_str().unwrap();
    let relay = world.relay_url.as_str();
    let uploaded = cli(
        &world.agent,
        relay,
        &["upload", "file", "--file", image_path],
    )
    .await;
    assert!(uploaded.ok, "{}", uploaded.stderr);
    let blob: Value = serde_json::from_str(uploaded.stdout.trim())
        .unwrap_or_else(|e| panic!("upload printed no descriptor: {e}: {}", uploaded.stdout));
    let (url, sha256, size, kind) = (
        blob["url"].as_str().unwrap().to_owned(),
        blob["sha256"].as_str().unwrap().to_owned(),
        blob["size"].as_i64().unwrap(),
        blob["type"].as_str().unwrap().to_owned(),
    );
    assert_eq!(size, i64::try_from(bytes.len()).unwrap());
    let reply_to = first.id.to_hex();
    let channel = world.channel.to_string();
    let posted = cli(
        &world.agent,
        relay,
        &[
            "messages",
            "send",
            "--channel",
            &channel,
            "--reply-to",
            &reply_to,
            "--file",
            image_path,
            "--content",
            "picture",
        ],
    )
    .await;
    assert!(posted.ok, "{}", posted.stderr);

    // It saves a document and the file, then reports.
    let (replies, mentions) = script
        .play(vec![
            Step::Call(
                "save_artifact",
                json!({"cardId": "$card", "name": "report", "content": "It is a dot."}),
            ),
            Step::Call(
                "save_artifact",
                json!({
                    "cardId": "$card", "name": "picture", "kind": "file",
                    "sha256": sha256, "size": size, "mediaType": kind, "mediaRef": url,
                }),
            ),
            Step::Call(
                "stage_done",
                json!({"cardId": "$card", "stage": "make", "visit": 1, "result": "passed", "summary": "made"}),
            ),
            Step::AwaitMention,
        ])
        .await;
    assert!(replies.iter().all(|r| !r.is_error), "{replies:?}");
    assert_eq!(replies[1].json()["mediaRef"], url.as_str());

    // Stage two's task names both inputs and says how to download the file.
    let second = &mentions[0];
    assert!(
        second.content.contains("Input artifacts:")
            && second.content.contains("- `report` (text, version 1, 12 bytes):")
            && second.content.contains("It is a dot.")
            && second.content.contains(&format!(
                "- `picture` (file, version 1, {kind}, {size} bytes): download it with `buzz media get {url} -o <path>`."
            )),
        "{}",
        second.content
    );

    // The agent downloads it from the command in the task. The bytes are the ones uploaded.
    let out = dir.path().join("downloaded.png");
    let got = cli(
        &world.agent,
        relay,
        &["media", "get", &url, "-o", out.to_str().unwrap()],
    )
    .await;
    assert!(
        got.ok,
        "agent in the channel could not read: {}",
        got.stderr
    );
    assert_eq!(std::fs::read(&out).unwrap(), bytes);

    // Reads are enforced: a relay member outside the private channel is refused.
    let refused = cli(
        outsider,
        relay,
        &[
            "media",
            "get",
            &url,
            "-o",
            dir.path().join("stolen.png").to_str().unwrap(),
        ],
    )
    .await;
    assert!(
        !refused.ok,
        "the relay served a member outside the channel: {}",
        refused.stdout
    );
    assert!(!dir.path().join("stolen.png").exists());

    // The card lists the document and the file for the Files tab.
    let (_, listed) = call(app, "ListArtifacts", owner, json!({"cardId": card})).await;
    let names: Vec<&str> = listed["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["picture", "report"], "{listed}");
    let (_, files) = call(app, "ListCardFiles", owner, json!({"cardId": card})).await;
    assert_eq!(files["files"][0]["url"], url.as_str(), "{files}");

    let (replies, _) = script
        .play(vec![Step::Call(
            "stage_done",
            json!({"cardId": "$card", "stage": "use", "visit": 1, "result": "passed"}),
        )])
        .await;
    assert_eq!(replies[0].json()["state"], "ended", "{replies:?}");

    script.finish().await;
    let _ = stop.send(true);
    drain_task.await.unwrap();
}
