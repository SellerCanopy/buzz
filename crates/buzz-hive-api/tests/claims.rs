//! Requester claims: an agent trades a chat message or a run for a signed statement of who it
//! acts for, and then acts at the lower of its own level and that person's.

mod common;

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use axum::http::StatusCode;
use buzz_hive_api::token::{Clock, TokenMinter};
use buzz_hive_api::{App, CLAIM_PATH};
use buzz_hive_core::access::{ActorKind, Level, RelayRole};
use buzz_hive_core::ports::store::{RelayEvent, StageThread};
use buzz_hive_core::testing::{FakeDirectory, MemStore};
use buzz_hive_core::workflow::source::{MemSource, Source, Sources};
use common::{app, call, call_for, call_unclaimed, community, send, AGENT, HIVE, MEMBER, OWNER};
use serde_json::{json, Value};

/// Runs, and asks.
const RUNNER: &str = "a1";
const BUILDER: &str = "b1";
const CHANNEL: &str = "00000000-0000-0000-0000-00000000004d";

fn id(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

struct TestClock(AtomicI64);

impl Clock for TestClock {
    fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn channel() -> uuid::Uuid {
    uuid::Uuid::parse_str(CHANNEL).unwrap()
}

/// A chat message in the channel. `thread` is the root it replies to, if any.
fn message(
    n: u8,
    author: &str,
    mentions: &[&str],
    thread: Option<u8>,
    age: i64,
    now: i64,
) -> RelayEvent {
    let mut tags: Vec<Vec<String>> = mentions
        .iter()
        .map(|p| vec!["p".to_owned(), (*p).to_owned()])
        .collect();
    if let Some(root) = thread {
        tags.push(vec!["e".into(), id(root), "".into(), "root".into()]);
    }
    RelayEvent {
        id: id(n),
        author: author.to_owned(),
        kind: 9,
        tags,
        created_at: now - age,
        channel_id: Some(channel()),
        deleted: false,
    }
}

fn solo() -> Arc<Sources> {
    let files = MemSource::new()
        .with("solo/kiln-package.yaml", "name: solo\nversion: 1.0.0\n")
        .with(
            "solo/workflow.yaml",
            "version: 2\nstages:\n  - id: only\n    kind: prompt\n    agent: builder\n    prompt: \"Do {{issue.title}}\"\n",
        );
    Arc::new(Sources(vec![Source::new("test", false, files)]))
}

struct World {
    app: App,
    clock: Arc<TestClock>,
    card: String,
    run: String,
}

/// `RUNNER` holds Run and `MEMBER` Read. `BUILDER` and `AGENT` hold Run. A run of `solo`
/// started by `RUNNER` waits at `builder`'s stage, whose thread is rooted at event 10.
async fn world() -> World {
    let w = unrecorded().await;
    record_threads(&w).await;
    w
}

/// [`world`] before the outbox has recorded the stage threads.
async fn unrecorded() -> World {
    let now = chrono::Utc::now().timestamp();
    let clock = Arc::new(TestClock(AtomicI64::new(now)));
    let thread = |n, author: &str, mentions: &[&str], root, age| {
        message(n, author, mentions, root, age, now)
    };
    let mut deleted = thread(5, RUNNER, &[BUILDER], None, 10);
    deleted.deleted = true;
    let mut reaction = thread(6, RUNNER, &[BUILDER], Some(1), 5);
    reaction.kind = 7;
    let directory = FakeDirectory::new()
        .with_role(OWNER, RelayRole::Owner)
        .with_role(MEMBER, RelayRole::Member)
        .with_role(RUNNER, RelayRole::Member)
        .with_agent(AGENT, OWNER)
        .with_named_agent("builder", BUILDER, OWNER)
        .with_channel(channel(), &[HIVE, BUILDER])
        // 1: a Run person asks. 2: a Read person asks. 3: asks someone else.
        .with_event(thread(1, RUNNER, &[BUILDER], None, 10))
        .with_event(thread(2, MEMBER, &[BUILDER], None, 10))
        .with_event(thread(3, RUNNER, &[AGENT], None, 10))
        // 4 asked first, 7 asked after it in the same thread: only 7 names a requester.
        .with_event(thread(4, RUNNER, &[BUILDER], None, 100))
        .with_event(thread(7, MEMBER, &[BUILDER], Some(4), 50))
        .with_event(deleted)
        // A reaction names the agent and the thread but asks nothing.
        .with_event(reaction)
        .with_event(thread(8, RUNNER, &[BUILDER], None, 3 * 60 * 60))
        // The stage task Beehive posted for `BUILDER`, and one it posted for `AGENT`.
        .with_event(thread(10, HIVE, &[BUILDER], None, 10))
        .with_event(thread(11, HIVE, &[BUILDER], None, 10))
        .with_event(thread(12, HIVE, &[BUILDER], None, 10))
        // An agent asks another, and an agent asks itself.
        .with_event(thread(13, AGENT, &[BUILDER], None, 10))
        .with_event(thread(14, BUILDER, &[BUILDER], None, 10))
        // A message that sits in no channel.
        .with_event(RelayEvent {
            channel_id: None,
            ..thread(15, RUNNER, &[BUILDER], None, 10)
        });
    let base = app(MemStore::new()).await;
    let app = App {
        directory: Arc::new(directory),
        catalog: solo(),
        minter: Arc::new(
            TokenMinter::new(b"0123456789abcdef0123456789abcdef", clock.clone()).unwrap(),
        ),
        ..base
    };
    let mut tx = app.store.begin(community()).await.unwrap();
    for (key, kind) in [(BUILDER, ActorKind::Agent), (RUNNER, ActorKind::Person)] {
        tx.access()
            .set_level(key, Level::Run, kind, OWNER)
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
    let (_, card) = call(
        &app,
        "CreateCard",
        RUNNER,
        json!({"title": "Fix the build", "workflow": "solo"}),
    )
    .await;
    let card = card["id"].as_str().unwrap().to_owned();
    let (status, run) = call(&app, "StartStage", RUNNER, json!({"cardId": card})).await;
    assert_eq!(status, StatusCode::OK, "{run}");
    let run_id = run["id"].as_str().unwrap().to_owned();
    World {
        app,
        clock,
        card,
        run: run_id,
    }
}

/// What the outbox does once the relay took a stage task: record the thread it opened.
async fn record_threads(w: &World) {
    let (app, card) = (&w.app, &w.card);
    let mut tx = app.store.begin(community()).await.unwrap();
    for (root, agent) in [(10, BUILDER), (11, AGENT)] {
        let card = uuid::Uuid::parse_str(card).unwrap();
        let thread = StageThread {
            card_id: card,
            stage: if agent == BUILDER {
                "only".into()
            } else {
                "other".into()
            },
            visit: 1,
            root_event_id: id(root),
            agent_pubkey: agent.into(),
            channel_id: channel(),
        };
        tx.runs().record_thread(&thread).await.unwrap();
    }
    tx.commit().await.unwrap();
}

async fn claim(app: &App, agent: &str, body: Value) -> (StatusCode, Value) {
    send(app, CLAIM_PATH.to_owned(), agent, None, body).await
}

/// A claim for event `n`, or the reason it was refused.
async fn claim_for_event(app: &App, agent: &str, n: u8) -> Result<String, (StatusCode, Value)> {
    let (status, body) = claim(app, agent, json!({"event_id": id(n)})).await;
    match status {
        StatusCode::OK => Ok(body["claim"].as_str().unwrap().to_owned()),
        _ => Err((status, body)),
    }
}

async fn create(app: &App, agent: &str, claim: Option<&str>) -> (StatusCode, Value) {
    send(
        app,
        buzz_hive_api::routes::path("CreateCard"),
        agent,
        claim,
        json!({"title": "from an agent"}),
    )
    .await
}

#[tokio::test]
async fn an_agent_acts_at_the_level_of_the_person_whose_message_started_its_turn() {
    let w = world().await;
    // A Run person asked, so the agent acts at Run.
    let held = claim_for_event(&w.app, BUILDER, 1).await.unwrap();
    let (status, body) = create(&w.app, BUILDER, Some(&held)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["id"].is_string(), "{body}");

    // A Read person asked: the agent holds Run and still acts at Read.
    let held = claim_for_event(&w.app, BUILDER, 2).await.unwrap();
    let (status, body) = create(&w.app, BUILDER, Some(&held)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("needs Run, you have Read"),
        "{body}"
    );
    let (status, _) = send(
        &w.app,
        buzz_hive_api::routes::path("ListCards"),
        BUILDER,
        Some(&held),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "it may still read");
    // The same agent with no claim reads and does not act.
    let (status, _) = create(&w.app, BUILDER, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_requesters_level_counts_when_the_claim_is_used_not_when_it_was_issued() {
    let w = world().await;
    let held = claim_for_event(&w.app, BUILDER, 1).await.unwrap();
    assert_eq!(create(&w.app, BUILDER, Some(&held)).await.0, StatusCode::OK);
    // The owner lowers the person who asked, and the claim already in hand follows.
    let (status, body) = call(
        &w.app,
        "SetAccessLevel",
        OWNER,
        json!({"pubkey": RUNNER, "level": "read"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        create(&w.app, BUILDER, Some(&held)).await.0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn a_message_that_does_not_ask_this_agent_to_act_names_no_requester() {
    let w = world().await;
    for (n, why) in [
        (3, "does not mention you"),
        (4, "a newer message in its thread mentions you"),
        (5, "it was deleted"),
        (6, "it is not a chat message"),
        (8, "too old"),
    ] {
        let (status, body) = claim_for_event(&w.app, BUILDER, n).await.unwrap_err();
        assert_eq!(status, StatusCode::FORBIDDEN, "event {n}: {body}");
        assert!(
            body["message"].as_str().unwrap().contains(why),
            "event {n}: {body}"
        );
    }
    // The newer message in that thread does.
    assert!(claim_for_event(&w.app, BUILDER, 7).await.is_ok());
    // A message the relay does not hold.
    let (status, _) = claim_for_event(&w.app, BUILDER, 99).await.unwrap_err();
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Text that is not an id.
    let (status, _) = claim(&w.app, BUILDER, json!({"event_id": "nope"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Neither or both of an event and a run.
    let (status, _) = claim(&w.app, BUILDER, json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = claim(&w.app, BUILDER, json!({"event_id": id(1), "run_id": w.run})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_stage_task_names_the_person_who_started_the_run() {
    let w = world().await;
    // Beehive posted the task and the run was started by `RUNNER`, who holds Run.
    let held = claim_for_event(&w.app, BUILDER, 10).await.unwrap();
    let (status, body) = send(
        &w.app,
        buzz_hive_api::routes::path("GetRun"),
        BUILDER,
        Some(&held),
        json!({"runId": w.run}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(create(&w.app, BUILDER, Some(&held)).await.0, StatusCode::OK);

    // A stage task for another agent's stage is not this agent's to claim from.
    let (status, body) = claim_for_event(&w.app, BUILDER, 11).await.unwrap_err();
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // Beehive's post that opens no stage thread names nobody.
    let (status, body) = claim_for_event(&w.app, BUILDER, 12).await.unwrap_err();
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("stage thread"),
        "{body}"
    );
}

#[tokio::test]
async fn a_stage_task_asked_about_before_its_thread_is_recorded_is_waited_for() {
    let w = unrecorded().await;
    // The drain posted the task, an agent that reacts at once asks, and the thread lands after.
    let late = {
        let w = World {
            app: w.app.clone(),
            clock: w.clock.clone(),
            card: w.card.clone(),
            run: w.run.clone(),
        };
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            record_threads(&w).await;
        })
    };
    assert!(claim_for_event(&w.app, BUILDER, 10).await.is_ok());
    late.await.unwrap();
}

#[tokio::test]
async fn a_message_an_agent_wrote_names_no_requester() {
    let w = world().await;
    // `AGENT` holds Run and is owned by the Admin. If its message named it, `BUILDER` would
    // act at `AGENT`'s level, which an agent with no person behind it is never given.
    let (status, body) = claim_for_event(&w.app, BUILDER, 13).await.unwrap_err();
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("written by an agent"),
        "{body}"
    );
    // Nor does an agent's message that mentions the agent itself.
    let (status, body) = claim_for_event(&w.app, BUILDER, 14).await.unwrap_err();
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("written by an agent"),
        "{body}"
    );
    // Nor does one that sits in no channel, where nothing shows it is the newest.
    let (status, body) = claim_for_event(&w.app, BUILDER, 15).await.unwrap_err();
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("not in a channel"),
        "{body}"
    );
}

#[tokio::test]
async fn a_claim_that_names_an_agent_is_refused_even_when_it_is_signed() {
    let w = world().await;
    let (named, _) = w
        .app
        .minter
        .sign_claim(AGENT, community(), BUILDER, "chat")
        .unwrap();
    let (status, body) = create(&w.app, BUILDER, Some(&named)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("names an agent"),
        "{body}"
    );
}

#[tokio::test]
async fn a_run_claim_stops_working_once_the_run_no_longer_holds_the_agents_stage() {
    let w = world().await;
    let (status, body) = claim(&w.app, BUILDER, json!({"run_id": w.run})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let held = body["claim"].as_str().unwrap().to_owned();
    assert_eq!(create(&w.app, BUILDER, Some(&held)).await.0, StatusCode::OK);

    let (status, body) = call(&w.app, "StopRun", OWNER, json!({"runId": w.run})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = create(&w.app, BUILDER, Some(&held)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("no longer holds"),
        "{body}"
    );
}

#[tokio::test]
async fn a_run_names_its_requester_only_to_the_agent_of_its_current_stage() {
    let w = world().await;
    let (status, body) = claim(&w.app, BUILDER, json!({"run_id": w.run})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["requester"], RUNNER);
    // Without this, an agent could pass the id of a run an admin started and borrow the
    // admin's level.
    let (status, body) = claim(&w.app, AGENT, json!({"run_id": w.run})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("belongs to another agent"),
        "{body}"
    );
    let (status, _) = claim(&w.app, BUILDER, json!({"run_id": uuid::Uuid::nil()})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = claim(&w.app, BUILDER, json!({"run_id": "nope"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_claim_that_is_forged_expired_or_someone_elses_is_refused() {
    let w = world().await;
    let held = claim_for_event(&w.app, BUILDER, 1).await.unwrap();

    // Another agent cannot use it.
    let (status, body) = create(&w.app, AGENT, Some(&held)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    // One signed by another key, and one that is not a claim at all.
    let other = TokenMinter::new(b"ffffffffffffffffffffffffffffffff", w.clock.clone()).unwrap();
    let (forged, _) = other
        .sign_claim(OWNER, community(), BUILDER, "chat")
        .unwrap();
    for bad in [forged.as_str(), "not a claim", ""] {
        let (status, body) = create(&w.app, BUILDER, Some(bad)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{bad:?}: {body}");
    }
    // A bearer token is not a claim.
    let (bearer, _) = w.app.minter.mint(OWNER, community(), None).unwrap();
    let (status, _) = create(&w.app, BUILDER, Some(&bearer)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A claim for another community.
    let (elsewhere, _) = w
        .app
        .minter
        .sign_claim(OWNER, uuid::Uuid::from_u128(9), BUILDER, "chat")
        .unwrap();
    let (status, _) = create(&w.app, BUILDER, Some(&elsewhere)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // And it stops working when it expires.
    assert_eq!(create(&w.app, BUILDER, Some(&held)).await.0, StatusCode::OK);
    w.clock
        .0
        .fetch_add(buzz_hive_api::token::CLAIM_LIFETIME_SECS, Ordering::SeqCst);
    // The bearer token lives longer than the claim, so the claim is what is refused.
    let (status, body) = create(&w.app, BUILDER, Some(&held)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("expired"),
        "{body}"
    );
}

#[tokio::test]
async fn a_person_sends_no_claim_and_is_not_held_to_one() {
    let w = world().await;
    let (status, body) = claim(&w.app, RUNNER, json!({"event_id": id(1)})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // A claim a person sends changes nothing: a person acts for themselves.
    let held = claim_for_event(&w.app, BUILDER, 2).await.unwrap();
    let (status, _) = send(
        &w.app,
        buzz_hive_api::routes::path("CreateCard"),
        RUNNER,
        Some(&held),
        json!({"title": "by a person"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call_unclaimed(&w.app, "ListCards", RUNNER, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    // No bearer token, no claim.
    let (status, _) = send(
        &w.app,
        CLAIM_PATH.to_owned(),
        "zz",
        None,
        json!({"event_id": id(1)}),
    )
    .await;
    assert_ne!(status, StatusCode::OK);
}

#[tokio::test]
async fn the_public_key_is_published_and_verifies_a_claim() {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;
    let w = world().await;
    let response = common::router(&w.app)
        .oneshot(
            Request::get("/.well-known/jwks.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let jwks: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(jwks["keys"][0]["kty"], "OKP");
    assert_eq!(jwks["keys"][0]["crv"], "Ed25519");
    assert_eq!(jwks["keys"][0]["alg"], "EdDSA");
    let held = claim_for_event(&w.app, BUILDER, 1).await.unwrap();
    let key = jsonwebtoken::DecodingKey::from_ed_components(jwks["keys"][0]["x"].as_str().unwrap())
        .unwrap();
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::EdDSA);
    validation.validate_exp = false;
    validation.required_spec_claims.clear();
    let read = jsonwebtoken::decode::<Value>(&held, &key, &validation).unwrap();
    assert_eq!(read.claims["sub"], RUNNER);
    assert_eq!(read.claims["agent"], BUILDER);
    assert_eq!(read.claims["via"], "chat");
    let _ = (&w.card, call_for);
}
