//! Intake end to end against the replay server: the real GitHub adapter, the real loops and
//! the real routes, over an in-memory store. Nothing here reaches GitHub. A request the
//! replay has no answer for fails the test and is named.

mod support;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use buzz_hive::github_adapter::GitHub;
use buzz_hive::intake::{IntakeSupervisor, Random, Shared};
use buzz_hive_api::replay::{ReplayGuard, REPLAY_TTL};
use buzz_hive_api::routes::path;
use buzz_hive_api::token::{SystemClock, TokenMinter};
use buzz_hive_api::{ApiConfig, App};
use buzz_hive_core::access::RelayRole;
use buzz_hive_core::background::intake::Gate;
use buzz_hive_core::bus::Bus;
use buzz_hive_core::ports::store::{CommunityId, Store};
use buzz_hive_core::testing::{FakeDirectory, MemStore};
use buzz_hive_core::workflow::builtin::builtin_source;
use buzz_hive_core::workflow::source::Sources;
use serde_json::{json, Value};
use support::github_replay::{Exchange, Replay};
use tokio::sync::{watch, Notify};
use tower::ServiceExt;

const OWNER: &str = "aa";
const HIVE: &str = "ee";

fn community() -> CommunityId {
    CommunityId::from_u128(1)
}

fn list() -> Exchange {
    Exchange::get("/repos/example-org/api/issues")
        .query("state", "open")
        .query("labels", "bug")
        .query("per_page", "100")
}

/// An app and a running supervisor over `store` and the GitHub address `replay`. Every random
/// draw is zero, so a rule's first pass is at once.
struct Stack {
    app: App,
    stop: watch::Sender<bool>,
    supervisor: tokio::task::JoinHandle<()>,
}

async fn stack(store: Arc<MemStore>, replay: &Replay) -> Stack {
    let host = Arc::new(GitHub::new(replay.url(), "example-token").unwrap());
    let directory = Arc::new(FakeDirectory::new().with_role(OWNER, RelayRole::Owner));
    let catalog = Arc::new(Sources(vec![builtin_source()]));
    let bus = Bus::new(0);
    let wake = Arc::new(Notify::new());
    let app = App {
        config: Arc::new(ApiConfig {
            public_url: "http://hive.test".into(),
            allowed_origins: Vec::new(),
        }),
        store: store.clone(),
        directory: directory.clone(),
        minter: Arc::new(
            TokenMinter::new(b"0123456789abcdef0123456789abcdef", Arc::new(SystemClock)).unwrap(),
        ),
        replay: Arc::new(ReplayGuard::new(REPLAY_TTL)),
        auth_limit: buzz_hive_api::rate::auth_limiter(),
        bus: bus.clone(),
        catalog: catalog.clone(),
        community: community(),
        hive_pubkey: HIVE.into(),
        outbox: Arc::new(Notify::new()),
        timers: Arc::new(Notify::new()),
        code_host: Some(host.clone()),
        intake: wake.clone(),
    };
    let random: Random = Arc::new(|| 0.0);
    let shared = Arc::new(Shared {
        store,
        directory,
        catalog,
        host,
        bus,
        community: community(),
        hive: HIVE.into(),
        outbox: app.outbox.clone(),
        gate: Gate::default(),
        random,
    });
    let (stop, stopped) = watch::channel(false);
    let supervisor = tokio::spawn(IntakeSupervisor { shared, wake }.run(stopped));
    Stack {
        app,
        stop,
        supervisor,
    }
}

impl Stack {
    async fn call(&self, method: &str, body: Value) -> (StatusCode, Value) {
        let (token, _) = self
            .app
            .minter
            .mint(OWNER, self.app.community, None)
            .unwrap();
        let request = Request::post(path(method))
            .header("authorization", format!("Bearer {token}"))
            .body(Body::from(body.to_string()))
            .unwrap();
        let router = buzz_hive_api::finish(buzz_hive_api::routes(), self.app.clone());
        let response = router.oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    async fn cards(&self) -> Vec<Value> {
        let (status, body) = self.call("ListCards", json!({})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["cards"].as_array().cloned().unwrap_or_default()
    }

    async fn stop(self) {
        self.stop.send_replace(true);
        tokio::time::timeout(Duration::from_secs(15), self.supervisor)
            .await
            .expect("the supervisor stopped")
            .unwrap();
    }
}

/// How many passes the log of events records as finished.
async fn passes(store: &MemStore) -> usize {
    let mut tx = store.begin(community()).await.unwrap();
    tx.events()
        .after(0, 1000)
        .await
        .unwrap()
        .iter()
        .filter(|e| e.kind == "intake.refreshed")
        .count()
}

async fn until<F: std::future::Future<Output = bool>>(what: &str, mut ready: impl FnMut() -> F) {
    for _ in 0..200 {
        if ready().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("gave up waiting for {what}");
}

fn rule() -> Value {
    json!({"rule": {
        "name": "Bugs",
        "enabled": true,
        "workflow": "rpi",
        "intake": {"host": "github.com", "repos": ["example-org/api"], "label": "bug", "intervalS": 3600}
    }})
}

#[tokio::test]
async fn a_saved_rule_turns_issues_into_cards_and_a_restart_makes_no_second_card() {
    let store = Arc::new(MemStore::new());

    // First run: the rule is saved through the route, and its first pass reads the list once.
    let replay = Replay::start(vec![list()
        .header("etag", "\"v1\"")
        .fixture("issues_open.json")])
    .await;
    let first = stack(store.clone(), &replay).await;
    let (status, saved) = first.call("SaveIntakeRule", rule()).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    until("the first pass", || async { passes(&store).await == 1 }).await;
    let cards = first.cards().await;
    let mut titles: Vec<_> = cards
        .iter()
        .map(|c| c["title"].as_str().unwrap().to_owned())
        .collect();
    titles.sort();
    assert_eq!(
        titles,
        ["Add a retry to the sync call", "Crash when saving a draft"],
        "the pull request in the list is not a card: {cards:?}"
    );
    let numbers: Vec<_> = cards.iter().map(|c| c["issue"]["number"].clone()).collect();
    assert!(
        numbers.contains(&json!(42)) && numbers.contains(&json!(43)),
        "{cards:?}"
    );
    first.stop().await;
    let sent = replay.requests();
    assert_eq!(sent.len(), 1, "one list read, no per-issue reads");
    assert_eq!(
        sent[0].header("authorization"),
        Some("Bearer example-token")
    );
    replay.finish();

    // Restart over the same store with a new adapter, so no etag is kept: it reads the same
    // list again, and the issues it finds are the cards it already has.
    let replay = Replay::start(vec![list().fixture("issues_open.json")]).await;
    let second = stack(store.clone(), &replay).await;
    until("the pass after the restart", || async {
        passes(&store).await == 2
    })
    .await;
    assert_eq!(second.cards().await.len(), 2, "still two cards");
    second.stop().await;
    assert!(replay.requests()[0].header("if-none-match").is_none());
    replay.finish();
}

#[tokio::test]
async fn an_issue_that_left_the_search_is_read_once_and_its_card_stays_with_a_reason() {
    let store = Arc::new(MemStore::new());
    let replay = Replay::start(vec![list().fixture("issues_open.json")]).await;
    let first = stack(store.clone(), &replay).await;
    first.call("SaveIntakeRule", rule()).await;
    until("the first pass", || async { passes(&store).await == 1 }).await;
    first.stop().await;
    replay.finish();

    // Issue 42 is closed and 43 has lost its label: the list now holds neither. The pass reads
    // each issue that left (two reads), and the cards stay on the board.
    let replay = Replay::start(vec![
        list().body("[]"),
        Exchange::get("/repos/example-org/api/issues/42")
            .fixture("issue_42_closed.json")
            .any_order(),
        Exchange::get("/repos/example-org/api/issues/43")
            .body(
                r#"{"number": 43, "title": "Add a retry to the sync call", "body": null,
                "state": "open", "labels": [{"name": "kiln"}], "assignees": [],
                "user": {"login": "mallory"},
                "html_url": "https://github.example.org/example-org/api/issues/43"}"#,
            )
            .any_order(),
    ])
    .await;
    let second = stack(store.clone(), &replay).await;
    until("the second pass", || async { passes(&store).await == 2 }).await;
    let cards = second.cards().await;
    assert_eq!(cards.len(), 2, "no card is removed: {cards:?}");
    let mut tx = store.begin(community()).await.unwrap();
    let left = tx
        .events()
        .after(0, 1000)
        .await
        .unwrap()
        .iter()
        .filter(|e| e.kind == "card.left_intake")
        .count();
    assert_eq!(left, 2);
    drop(tx);
    second.stop().await;
    replay.finish();
}
