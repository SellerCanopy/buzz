//! An outside MCP server accepts a good requester claim and records who it names, and
//! refuses a forged claim, an expired claim and a call with none. No relay is needed: the
//! key set is served from the same signing key Beehive uses.

mod support;

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use axum::routing::get;
use axum::Json;
use buzz_hive_api::token::{Clock, TokenMinter};
use serde_json::{json, Value};
use support::claim_echo_mcp::{ClaimEcho, Seen};
use uuid::Uuid;

const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";
const OTHER_SECRET: &[u8] = b"fedcba9876543210fedcba9876543210";
const PERSON: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const AGENT: &str = "2222222222222222222222222222222222222222222222222222222222222222";

struct At(AtomicI64);

impl Clock for At {
    fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn minter(secret: &[u8], now: i64) -> Arc<TokenMinter> {
    Arc::new(TokenMinter::new(secret, Arc::new(At(AtomicI64::new(now)))).unwrap())
}

fn now() -> i64 {
    buzz_hive_api::token::SystemClock.now()
}

/// Serves `minter`'s key set the way Beehive does, and returns where.
async fn serve_jwks(minter: Arc<TokenMinter>) -> String {
    let router = axum::Router::new().route(
        buzz_hive_api::JWKS_PATH,
        get(move || {
            let minter = minter.clone();
            async move { Json(minter.jwks()) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}{}",
        listener.local_addr().unwrap(),
        buzz_hive_api::JWKS_PATH
    );
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    url
}

async fn call(echo: &ClaimEcho, claim: Option<&str>) -> (u16, Value) {
    let mut request = reqwest::Client::new().post(&echo.url).json(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "whoami", "arguments": {}},
    }));
    if let Some(claim) = claim {
        request = request.header("x-hive-claim", claim);
    }
    let response = request.send().await.unwrap();
    (response.status().as_u16(), response.json().await.unwrap())
}

#[tokio::test]
async fn a_good_claim_is_recorded_on_each_call_and_a_bad_one_is_refused() {
    let signer = minter(SECRET, now());
    let echo = ClaimEcho::start(&serve_jwks(signer.clone()).await).await;
    let community = Uuid::from_u128(7);

    let good = signer
        .sign_claim(PERSON, community, AGENT, "chat")
        .unwrap()
        .0;
    for _ in 0..2 {
        let (status, body) = call(&echo, Some(&good)).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["result"]["content"][0]["text"], PERSON);
    }
    let via_run = signer
        .sign_claim(PERSON, community, AGENT, "run:r1")
        .unwrap()
        .0;
    assert_eq!(call(&echo, Some(&via_run)).await.0, 200);
    let seen = |via: &str| Seen {
        sub: PERSON.into(),
        agent: AGENT.into(),
        via: via.into(),
    };
    assert_eq!(echo.seen(), [seen("chat"), seen("chat"), seen("run:r1")]);

    // Signed by another key, signed long ago, not a claim at all, and no claim.
    let forged = minter(OTHER_SECRET, now())
        .sign_claim(PERSON, community, AGENT, "chat")
        .unwrap()
        .0;
    let expired = minter(SECRET, now() - 3_600)
        .sign_claim(PERSON, community, AGENT, "chat")
        .unwrap()
        .0;
    for bad in [
        Some(forged.as_str()),
        Some(expired.as_str()),
        Some("nope"),
        None,
    ] {
        let (status, body) = call(&echo, bad).await;
        assert_eq!(status, 401, "{bad:?}: {body}");
    }
    assert_eq!(echo.seen().len(), 3, "a refused call records nothing");
}
