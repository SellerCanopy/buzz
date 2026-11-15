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
            Json(json!({"method": method, "bearer": bearer, "body": body})),
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
    });
    let app = Router::new()
        .route(buzz_hive_api::AUTH_TOKEN_PATH, post(trade))
        .route(&buzz_hive_api::routes::path("{method}"), post(procedure))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, fake)
}

fn client(url: &str, keys: Keys, clock: Arc<TestClock>) -> HiveClient {
    HiveClient::new(
        Config {
            hive_url: url.to_owned(),
            keys,
        },
        clock,
    )
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
