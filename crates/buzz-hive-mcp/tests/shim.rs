//! The shim against a fake Beehive: its tool list, its token handling and its errors.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use base64::Engine as _;
use buzz_hive_api::tools::TOOLS;
use buzz_hive_mcp::{tool_list, CallError, Clock, Config, Hive, HiveClient, RENEW_BEFORE_SECS};
use nostr::{Event, JsonUtil, Keys};
use serde_json::{json, Value};

struct TestClock(AtomicI64);

impl TestClock {
    fn at(now: i64) -> Arc<Self> {
        Arc::new(Self(AtomicI64::new(now)))
    }
    fn advance(&self, secs: i64) {
        self.0.fetch_add(secs, Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

const LIFETIME: i64 = 900;

struct Fake {
    clock: Arc<TestClock>,
    trades: AtomicUsize,
    /// The pubkey and `u` tag of each trade's event.
    signers: Mutex<Vec<(String, String)>>,
    reject_once: AtomicBool,
    /// Every ListRuns that carries this bearer is refused as unauthenticated.
    reject_bearer: Mutex<Option<String>>,
    /// The id of each trade's event.
    event_ids: Mutex<Vec<String>>,
    /// The body of each claim request.
    claim_requests: Mutex<Vec<Value>>,
    /// When set, a claim request is refused with this message.
    refuse_claims: Mutex<Option<String>>,
    /// How long, in seconds, a claim the fake issues lasts.
    claim_lifetime: AtomicI64,
    /// When set, the claim route answers with this status and plain text.
    claim_failure: Mutex<Option<(StatusCode, &'static str)>>,
    /// When set, the claim route answers 200 with this body in place of a claim.
    claim_reply: Mutex<Option<Value>>,
}

async fn claim(
    State(fake): State<Arc<Fake>>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    fake.claim_requests.lock().unwrap().push(body);
    if let Some((status, text)) = *fake.claim_failure.lock().unwrap() {
        return (status, Json(json!(text)));
    }
    if let Some(reply) = fake.claim_reply.lock().unwrap().clone() {
        return (StatusCode::OK, Json(reply));
    }
    if let Some(message) = fake.refuse_claims.lock().unwrap().clone() {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"code": "permission_denied", "message": message})),
        );
    }
    let n = fake.claim_requests.lock().unwrap().len();
    (
        StatusCode::OK,
        Json(json!({
            "claim": format!("claim{n}"),
            "expires_at": fake.clock.now() + fake.claim_lifetime.load(Ordering::SeqCst),
            "requester": "person",
        })),
    )
}

async fn trade(State(fake): State<Arc<Fake>>, headers: HeaderMap) -> (StatusCode, Json<Value>) {
    let header = headers["authorization"].to_str().unwrap();
    let encoded = header.strip_prefix("Nostr ").expect("a Nostr header");
    let json = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let event = Event::from_json(String::from_utf8(json).unwrap()).unwrap();
    event.verify().expect("the event is signed");
    let url = event
        .tags
        .iter()
        .find(|t| t.as_slice().first().map(String::as_str) == Some("u"))
        .and_then(|t| t.as_slice().get(1).cloned())
        .unwrap();
    fake.event_ids.lock().unwrap().push(event.id.to_hex());
    fake.signers
        .lock()
        .unwrap()
        .push((event.pubkey.to_hex(), url));
    let n = fake.trades.fetch_add(1, Ordering::SeqCst) + 1;
    (
        StatusCode::OK,
        Json(json!({"token": format!("t{n}"), "expires_at": fake.clock.now() + LIFETIME})),
    )
}

async fn procedure(
    State(fake): State<Arc<Fake>>,
    Path(method): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let bearer = headers["authorization"].to_str().unwrap().to_owned();
    match method.as_str() {
        "StageDone" => (
            StatusCode::FORBIDDEN,
            Json(json!({"code": "permission_denied", "message": "needs Run, you have Read"})),
        ),
        "ListRuns" if fake.reject_bearer.lock().unwrap().as_deref() == Some(bearer.as_str()) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"code": "unauthenticated", "message": "token expired"})),
        ),
        "ListRuns" if fake.reject_once.swap(false, Ordering::SeqCst) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"code": "unauthenticated", "message": "token expired"})),
        ),
        _ => (
            StatusCode::OK,
            Json(json!({
                "method": method,
                "bearer": bearer,
                "body": body,
                "claim": headers.get("x-hive-claim").and_then(|v| v.to_str().ok()),
            })),
        ),
    }
}

async fn start(clock: Arc<TestClock>) -> (String, Arc<Fake>) {
    let fake = Arc::new(Fake {
        clock,
        trades: AtomicUsize::new(0),
        signers: Mutex::new(Vec::new()),
        reject_once: AtomicBool::new(false),
        reject_bearer: Mutex::new(None),
        event_ids: Mutex::new(Vec::new()),
        claim_requests: Mutex::new(Vec::new()),
        refuse_claims: Mutex::new(None),
        claim_lifetime: AtomicI64::new(300),
        claim_failure: Mutex::new(None),
        claim_reply: Mutex::new(None),
    });
    let app = Router::new()
        .route(buzz_hive_api::AUTH_TOKEN_PATH, post(trade))
        .route(buzz_hive_api::CLAIM_PATH, post(claim))
        // A proxy's page, and an empty reply, where Beehive's JSON should be.
        .route(
            &buzz_hive_api::routes::path("Html"),
            post(|| async { (StatusCode::OK, "<html>sign in</html>") }),
        )
        .route(
            &buzz_hive_api::routes::path("Empty"),
            post(|| async { StatusCode::OK }),
        )
        .route(&buzz_hive_api::routes::path("{method}"), post(procedure))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, fake)
}

fn client(url: &str, keys: Keys, clock: Arc<TestClock>) -> HiveClient {
    client_with_file(url, keys, clock, None)
}

fn client_with_file(
    url: &str,
    keys: Keys,
    clock: Arc<TestClock>,
    requester_file: Option<std::path::PathBuf>,
) -> HiveClient {
    HiveClient::new(
        Config {
            hive_url: url.to_owned(),
            keys,
            requester_file,
        },
        clock,
    )
}

/// A requester file holding `event_id`, in a directory of its own.
fn requester_file(event_id: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("turn");
    std::fs::write(&file, format!("{event_id}\n")).unwrap();
    (dir, file)
}

#[test]
fn the_tool_list_is_the_table() {
    let listed = tool_list();
    assert_eq!(listed.len(), TOOLS.len());
    for (tool, row) in listed.iter().zip(TOOLS) {
        assert_eq!(tool.name, row.name);
        assert_eq!(tool.description.as_deref(), Some(row.description));
        assert_eq!(
            Value::Object((*tool.input_schema).clone()),
            (row.schema)(),
            "{}",
            row.name
        );
    }
    assert!(listed.iter().any(|t| t.name == "stage_done"));
}

#[tokio::test]
async fn a_call_trades_one_signed_event_and_sends_the_token_and_the_arguments() {
    let clock = TestClock::at(1_000);
    let (url, fake) = start(clock.clone()).await;
    let keys = Keys::generate();
    let client = client(&url, keys.clone(), clock);

    let reply = client
        .call("GetCard", json!({"cardId": "c1"}))
        .await
        .unwrap();
    assert_eq!(reply["method"], "GetCard");
    assert_eq!(reply["bearer"], "Bearer t1");
    assert_eq!(reply["body"], json!({"cardId": "c1"}));
    let signers = fake.signers.lock().unwrap();
    assert_eq!(
        signers.as_slice(),
        [(keys.public_key().to_hex(), format!("{url}/auth/token"))]
    );
}

#[tokio::test]
async fn the_token_is_kept_until_sixty_seconds_before_it_expires() {
    let clock = TestClock::at(1_000);
    let (url, fake) = start(clock.clone()).await;
    let client = client(&url, Keys::generate(), clock.clone());

    client.call("GetCard", json!({})).await.unwrap();
    client.call("GetCard", json!({})).await.unwrap();
    assert_eq!(fake.trades.load(Ordering::SeqCst), 1);

    // One second before the renewal point.
    clock.advance(LIFETIME - RENEW_BEFORE_SECS - 1);
    let reply = client.call("GetCard", json!({})).await.unwrap();
    assert_eq!(fake.trades.load(Ordering::SeqCst), 1);
    assert_eq!(reply["bearer"], "Bearer t1");

    // At the renewal point the next call trades again and uses the new token.
    clock.advance(1);
    let reply = client.call("GetCard", json!({})).await.unwrap();
    assert_eq!(fake.trades.load(Ordering::SeqCst), 2);
    assert_eq!(reply["bearer"], "Bearer t2");
}

#[tokio::test]
async fn a_refusal_for_the_token_is_retried_once_on_a_new_one() {
    let clock = TestClock::at(1_000);
    let (url, fake) = start(clock.clone()).await;
    let client = client(&url, Keys::generate(), clock);
    client.call("GetCard", json!({})).await.unwrap();

    fake.reject_once.store(true, Ordering::SeqCst);
    let reply = client.call("ListRuns", json!({})).await.unwrap();
    assert_eq!(reply["bearer"], "Bearer t2");
    assert_eq!(fake.trades.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn calls_refused_on_the_same_token_share_one_new_token() {
    let clock = TestClock::at(1_000);
    let (url, fake) = start(clock.clone()).await;
    let client = Arc::new(client(&url, Keys::generate(), clock));
    client.call("GetCard", json!({})).await.unwrap();
    // Beehive now refuses the held token, `Bearer t1`.
    *fake.reject_bearer.lock().unwrap() = Some("Bearer t1".into());

    let calls: Vec<_> = (0..4)
        .map(|_| {
            let client = client.clone();
            tokio::spawn(async move { client.call("ListRuns", json!({})).await })
        })
        .collect();
    for call in calls {
        let reply = call.await.unwrap().unwrap();
        assert_eq!(reply["bearer"], "Bearer t2");
    }
    // The first trade, and one more for all four refused calls.
    assert_eq!(fake.trades.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn each_trade_signs_a_different_event_even_within_one_second() {
    let clock = TestClock::at(1_000);
    let (url, fake) = start(clock.clone()).await;
    let client = client(&url, Keys::generate(), clock.clone());
    client.call("GetCard", json!({})).await.unwrap();
    // Past the renewal window, so the next call trades again, probably in the same second.
    clock.advance(LIFETIME);
    client.call("GetCard", json!({})).await.unwrap();
    let ids = fake.event_ids.lock().unwrap();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1], "a repeated event id would be refused");
}

#[tokio::test]
async fn a_forwarded_error_keeps_its_connect_code_in_the_tool_result() {
    let clock = TestClock::at(1_000);
    let (url, _fake) = start(clock.clone()).await;
    let hive = Hive::new(Arc::new(client(&url, Keys::generate(), clock)));

    let result = hive
        .forward(
            "stage_done",
            json!({"cardId": "c", "stage": "build", "visit": 1, "result": "passed"})
                .as_object()
                .cloned(),
        )
        .await
        .expect("stage_done is a tool");
    let result = serde_json::to_value(&result).unwrap();
    assert_eq!(result["isError"], true, "{result}");
    assert_eq!(
        result["content"][0]["text"],
        "[permission_denied] needs Run, you have Read"
    );

    let ok = hive
        .forward("get_card", json!({"cardId": "c"}).as_object().cloned())
        .await
        .unwrap();
    let ok = serde_json::to_value(&ok).unwrap();
    assert_ne!(ok["isError"], true, "{ok}");
    assert!(ok["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("GetCard"));

    assert!(hive.forward("no_such_tool", None).await.is_none());
}

#[tokio::test]
async fn an_unreachable_beehive_is_unavailable() {
    let clock = TestClock::at(1_000);
    // A port nothing listens on.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let error = client(&url, Keys::generate(), clock)
        .call("GetCard", json!({}))
        .await
        .unwrap_err();
    assert_eq!(error.code, "unavailable");
    assert!(error
        .to_string()
        .starts_with("[unavailable] could not reach Beehive"));
}

#[test]
fn the_error_text_is_the_code_in_brackets_then_the_message() {
    let error = CallError {
        code: "not_found".into(),
        message: "card c1 does not exist".into(),
    };
    assert_eq!(error.to_string(), "[not_found] card c1 does not exist");
}

fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
    move |name| {
        pairs
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| (*v).to_owned())
    }
}

#[test]
fn config_reads_the_key_and_finds_beehive() {
    let keys = Keys::generate();
    let hex = keys.secret_key().to_secret_hex();
    let nsec = nostr::ToBech32::to_bech32(keys.secret_key()).unwrap();
    for key in [hex, nsec] {
        let key = key.clone();
        let config = Config::from_env(|name| match name {
            "BUZZ_PRIVATE_KEY" => Some(key.clone()),
            "BUZZ_HIVE_URL" => Some("http://localhost:3100/".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(config.keys.public_key(), keys.public_key());
        assert_eq!(config.hive_url, "http://localhost:3100");
    }

    let from_relay = |relay: &'static str| {
        let hex = keys.secret_key().to_secret_hex();
        Config::from_env(move |name| match name {
            "BUZZ_PRIVATE_KEY" => Some(hex.clone()),
            "BUZZ_RELAY_URL" => Some(relay.to_owned()),
            _ => None,
        })
    };
    assert_eq!(
        from_relay("wss://buzz.example.org").unwrap().hive_url,
        "https://buzz.example.org"
    );
    assert_eq!(
        from_relay("ws://localhost:3000/").unwrap().hive_url,
        "http://localhost:3000"
    );
    assert!(from_relay("ftp://x").is_err());

    assert!(Config::from_env(env(&[]))
        .unwrap_err()
        .contains("BUZZ_PRIVATE_KEY"));
    assert!(Config::from_env(env(&[("BUZZ_PRIVATE_KEY", "nope")]))
        .unwrap_err()
        .contains("not a valid key"));
}

#[tokio::test]
async fn a_call_with_no_requester_file_and_no_run_carries_no_claim() {
    let clock = TestClock::at(1_000);
    let (url, fake) = start(clock.clone()).await;
    let reply = client(&url, Keys::generate(), clock)
        .call("GetCard", json!({"cardId": "c1"}))
        .await
        .unwrap();
    assert_eq!(reply["claim"], Value::Null);
    assert!(fake.claim_requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_call_carries_a_claim_for_the_message_that_started_the_turn() {
    let clock = TestClock::at(1_000);
    let (url, fake) = start(clock.clone()).await;
    let (_dir, file) = requester_file("aa11");
    let client = client_with_file(&url, Keys::generate(), clock.clone(), Some(file));

    let reply = client.call("GetCard", json!({})).await.unwrap();
    assert_eq!(reply["claim"], "claim1");
    assert_eq!(
        fake.claim_requests.lock().unwrap().as_slice(),
        [json!({"event_id": "aa11"})]
    );

    // A burst of calls shares the claim for sixty seconds.
    client.call("GetCard", json!({})).await.unwrap();
    clock.advance(59);
    let reply = client.call("GetCard", json!({})).await.unwrap();
    assert_eq!(reply["claim"], "claim1");
    assert_eq!(fake.claim_requests.lock().unwrap().len(), 1);

    clock.advance(1);
    let reply = client.call("GetCard", json!({})).await.unwrap();
    assert_eq!(reply["claim"], "claim2");
    assert_eq!(fake.claim_requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_new_turn_asks_for_a_new_claim_at_once() {
    let clock = TestClock::at(1_000);
    let (url, fake) = start(clock.clone()).await;
    let (_dir, file) = requester_file("aa11");
    let client = client_with_file(&url, Keys::generate(), clock, Some(file.clone()));
    client.call("GetCard", json!({})).await.unwrap();

    std::fs::write(&file, "bb22").unwrap();
    let reply = client.call("GetCard", json!({})).await.unwrap();
    assert_eq!(reply["claim"], "claim2");
    assert_eq!(
        fake.claim_requests.lock().unwrap().last().unwrap(),
        &json!({"event_id": "bb22"})
    );
}

#[tokio::test]
async fn a_call_that_names_a_run_claims_from_the_run_when_there_is_no_turn_file() {
    let clock = TestClock::at(1_000);
    let (url, fake) = start(clock.clone()).await;
    let reply = client(&url, Keys::generate(), clock)
        .call("StageDone", json!({"runId": "r1"}))
        .await;
    // The fake refuses StageDone, but the claim was asked for and sent.
    assert!(reply.is_err());
    assert_eq!(
        fake.claim_requests.lock().unwrap().as_slice(),
        [json!({"run_id": "r1"})]
    );
}

#[tokio::test]
async fn the_message_wins_over_the_run() {
    let clock = TestClock::at(1_000);
    let (url, fake) = start(clock.clone()).await;
    let (_dir, file) = requester_file("aa11");
    let client = client_with_file(&url, Keys::generate(), clock, Some(file));
    client
        .call("GetCard", json!({"runId": "r1"}))
        .await
        .unwrap();
    assert_eq!(
        fake.claim_requests.lock().unwrap().as_slice(),
        [json!({"event_id": "aa11"})]
    );
}

#[tokio::test]
async fn a_refused_claim_goes_ahead_unclaimed_and_explains_a_refusal() {
    let clock = TestClock::at(1_000);
    let (url, fake) = start(clock.clone()).await;
    *fake.refuse_claims.lock().unwrap() = Some("the message is too old".into());
    let (_dir, file) = requester_file("aa11");
    let hive = Hive::new(Arc::new(client_with_file(
        &url,
        Keys::generate(),
        clock,
        Some(file),
    )));

    // A read still goes through, with no claim.
    let ok = hive
        .forward("get_card", json!({"cardId": "c"}).as_object().cloned())
        .await
        .unwrap();
    let ok = serde_json::to_value(&ok).unwrap();
    assert_ne!(ok["isError"], true, "{ok}");
    let echoed: Value = serde_json::from_str(ok["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(echoed["claim"], Value::Null);

    // A refusal for want of a level says why no person is named.
    let denied = hive
        .forward(
            "stage_done",
            json!({"cardId": "c", "stage": "build", "visit": 1, "result": "passed"})
                .as_object()
                .cloned(),
        )
        .await
        .unwrap();
    let denied = serde_json::to_value(&denied).unwrap();
    assert_eq!(denied["isError"], true);
    assert_eq!(
        denied["content"][0]["text"],
        "[permission_denied] needs Run, you have Read. No person is named for this turn: the message is too old"
    );
}

#[test]
fn config_reads_the_requester_file_from_the_environment() {
    let keys = Keys::generate();
    let hex = keys.secret_key().to_secret_hex();
    let with = |file: Option<&'static str>| {
        let hex = hex.clone();
        Config::from_env(move |name| match name {
            "BUZZ_PRIVATE_KEY" => Some(hex.clone()),
            "BUZZ_HIVE_URL" => Some("http://localhost:3100".into()),
            "BUZZ_REQUESTER_FILE" => file.map(str::to_owned),
            _ => None,
        })
        .unwrap()
    };
    assert_eq!(
        with(Some("/tmp/turn")).requester_file,
        Some(std::path::PathBuf::from("/tmp/turn"))
    );
    assert_eq!(with(Some("  ")).requester_file, None);
    assert_eq!(with(None).requester_file, None);
}

/// A claim request for each call, whatever came of the one before.
fn claim_count(fake: &Fake) -> usize {
    fake.claim_requests.lock().unwrap().len()
}

#[tokio::test]
async fn a_run_is_named_by_the_argument_the_schemas_declare() {
    // The shim reads the argument from the schema's own name, so a tool that declares one is
    // a tool whose calls fall back to a run claim.
    for tool in ["get_run", "cancel_run", "retry_stage"] {
        let row = TOOLS.iter().find(|t| t.name == tool).unwrap();
        let schema = (row.schema)();
        assert!(
            schema["properties"][buzz_hive_api::tools::RUN_ID_ARG].is_object(),
            "{tool}: {schema}"
        );
    }
    let clock = TestClock::at(1_000);
    let (url, fake) = start(clock.clone()).await;
    client(&url, Keys::generate(), clock)
        .call("GetRun", json!({"runId": "r9"}))
        .await
        .unwrap();
    assert_eq!(
        fake.claim_requests.lock().unwrap().as_slice(),
        [json!({"run_id": "r9"})]
    );
}

#[tokio::test]
async fn a_refused_claim_is_asked_for_again_on_each_call() {
    // A refusal can clear within a minute (a stage thread the relay has not recorded yet), so
    // it is not held the way a claim is.
    let clock = TestClock::at(1_000);
    let (url, fake) = start(clock.clone()).await;
    *fake.refuse_claims.lock().unwrap() = Some("it is too old".into());
    let (_dir, file) = requester_file("aa11");
    let client = client_with_file(&url, Keys::generate(), clock, Some(file));
    for _ in 0..5 {
        client.call("GetCard", json!({})).await.unwrap();
    }
    assert_eq!(claim_count(&fake), 5);
}

#[tokio::test]
async fn a_call_refused_as_unauthenticated_is_retried_on_a_new_token_and_a_new_claim() {
    let clock = TestClock::at(1_000);
    let (url, fake) = start(clock.clone()).await;
    fake.reject_once.store(true, Ordering::SeqCst);
    let (_dir, file) = requester_file("aa11");
    let client = client_with_file(&url, Keys::generate(), clock, Some(file));
    let reply = client.call("ListRuns", json!({})).await.unwrap();
    assert_eq!(fake.trades.load(Ordering::SeqCst), 2);
    assert_eq!(reply["bearer"], "Bearer t2");
    // The retry did not reuse the claim the first send carried.
    assert_eq!(claim_count(&fake), 2);
    assert_eq!(reply["claim"], "claim2");
}

#[tokio::test]
async fn a_claim_close_to_its_own_expiry_is_fetched_again_before_the_cache_runs_out() {
    let clock = TestClock::at(1_000);
    let (url, fake) = start(clock.clone()).await;
    // Good for less than the renewal margin, so it cannot be held.
    fake.claim_lifetime
        .store(RENEW_BEFORE_SECS / 2, Ordering::SeqCst);
    let (_dir, file) = requester_file("aa11");
    let client = client_with_file(&url, Keys::generate(), clock, Some(file));
    client.call("GetCard", json!({})).await.unwrap();
    let reply = client.call("GetCard", json!({})).await.unwrap();
    assert_eq!(claim_count(&fake), 2);
    assert_eq!(reply["claim"], "claim2");
}

#[tokio::test]
async fn a_claim_reply_with_no_claim_goes_ahead_unclaimed_and_says_so() {
    let clock = TestClock::at(1_000);
    let (url, fake) = start(clock.clone()).await;
    *fake.claim_reply.lock().unwrap() = Some(json!({"requester": "person"}));
    let (_dir, file) = requester_file("aa11");
    let hive = Hive::new(Arc::new(client_with_file(
        &url,
        Keys::generate(),
        clock,
        Some(file),
    )));
    let denied = hive
        .forward(
            "stage_done",
            json!({"cardId": "c", "stage": "build", "visit": 1, "result": "passed"})
                .as_object()
                .cloned(),
        )
        .await
        .unwrap();
    let denied = serde_json::to_value(&denied).unwrap();
    assert_eq!(denied["isError"], true);
    assert_eq!(
        denied["content"][0]["text"],
        "[permission_denied] needs Run, you have Read. No person is named for this turn: Beehive's claim reply has no claim or expiry"
    );
}

#[tokio::test]
async fn a_claim_route_that_is_down_goes_ahead_unclaimed() {
    let clock = TestClock::at(1_000);
    let (url, fake) = start(clock.clone()).await;
    *fake.claim_failure.lock().unwrap() = Some((StatusCode::BAD_GATEWAY, "bad gateway"));
    let (_dir, file) = requester_file("aa11");
    let client = client_with_file(&url, Keys::generate(), clock, Some(file));
    let reply = client.call("GetCard", json!({})).await.unwrap();
    assert_eq!(reply["claim"], Value::Null);
    assert_eq!(claim_count(&fake), 1);
}

#[tokio::test]
async fn an_empty_requester_file_names_no_turn() {
    let clock = TestClock::at(1_000);
    let (url, fake) = start(clock.clone()).await;
    for text in ["", "  \n"] {
        let (_dir, file) = requester_file("aa11");
        std::fs::write(&file, text).unwrap();
        let reply = client_with_file(&url, Keys::generate(), clock.clone(), Some(file))
            .call("GetCard", json!({}))
            .await
            .unwrap();
        assert_eq!(reply["claim"], Value::Null);
    }
    assert_eq!(claim_count(&fake), 0);
}

#[tokio::test]
async fn a_success_whose_body_is_not_json_is_an_error_and_an_empty_one_is_null() {
    let clock = TestClock::at(1_000);
    let (url, _fake) = start(clock.clone()).await;
    let client = client(&url, Keys::generate(), clock);
    let error = client.call("Html", json!({})).await.unwrap_err();
    assert_eq!(error.code, "internal");
    assert!(error.message.contains("not JSON"), "{error}");
    assert_eq!(client.call("Empty", json!({})).await.unwrap(), Value::Null);
}
