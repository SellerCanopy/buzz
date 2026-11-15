//! A stand-in for an outside MCP server that wants to know who an agent acts for.
//!
//! It speaks the one JSON-RPC call of MCP over HTTP that a test needs, checks the
//! `x-hive-claim` header against the JWKS Beehive publishes, and records who the claim
//! names. It trusts nothing but that key set, so it shows that a claim reaches a server
//! that never calls Beehive back about the person.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use serde_json::{json, Value};

/// What a good claim named.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Seen {
    /// The person the agent acts for.
    pub sub: String,
    /// The agent that holds the claim.
    pub agent: String,
    /// `chat`, or `run:<id>`.
    pub via: String,
}

#[derive(Clone)]
struct Echo {
    jwks_url: String,
    http: reqwest::Client,
    seen: Arc<Mutex<Vec<Seen>>>,
}

pub struct ClaimEcho {
    /// Where to send MCP calls.
    pub url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl ClaimEcho {
    /// Starts the server. Claims are checked against the key set at `jwks_url`.
    pub async fn start(jwks_url: &str) -> Self {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let echo = Echo {
            jwks_url: jwks_url.to_owned(),
            http: reqwest::Client::new(),
            seen: seen.clone(),
        };
        let router = Router::new().route("/mcp", post(handle)).with_state(echo);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self { url, seen }
    }

    /// The claims accepted so far, oldest first.
    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

async fn handle(
    State(echo): State<Echo>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let id = request["id"].clone();
    let refuse = |why: &str| {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32001, "message": why}})),
        )
    };
    match request["method"].as_str() {
        Some("initialize") => (
            StatusCode::OK,
            Json(json!({"jsonrpc": "2.0", "id": id, "result": {
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "claim-echo", "version": "0"},
            }})),
        ),
        Some("tools/call") => {
            let Some(claim) = headers.get("x-hive-claim").and_then(|v| v.to_str().ok()) else {
                return refuse("no claim");
            };
            match verify(&echo, claim).await {
                Ok(seen) => {
                    let sub = seen.sub.clone();
                    echo.seen.lock().unwrap().push(seen);
                    (
                        StatusCode::OK,
                        Json(json!({"jsonrpc": "2.0", "id": id, "result": {
                            "content": [{"type": "text", "text": sub}],
                        }})),
                    )
                }
                Err(why) => refuse(&why),
            }
        }
        _ => (
            StatusCode::OK,
            Json(
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "no such method"}}),
            ),
        ),
    }
}

/// Checks the signature against the published key, and the expiry against the clock.
async fn verify(echo: &Echo, claim: &str) -> Result<Seen, String> {
    let header = jsonwebtoken::decode_header(claim).map_err(|e| format!("not a claim: {e}"))?;
    if header.alg != Algorithm::EdDSA {
        return Err("a claim is signed with EdDSA".into());
    }
    let jwks: Value = echo
        .http
        .get(&echo.jwks_url)
        .send()
        .await
        .map_err(|e| format!("no key set: {e}"))?
        .json()
        .await
        .map_err(|e| format!("the key set is not JSON: {e}"))?;
    let key = jwks["keys"]
        .as_array()
        .and_then(|keys| {
            keys.iter()
                .find(|k| header.kid.is_some() && k["kid"].as_str() == header.kid.as_deref())
        })
        .ok_or("no key with the claim's kid")?;
    let decoding = DecodingKey::from_ed_components(key["x"].as_str().ok_or("the key has no x")?)
        .map_err(|e| format!("the key is unusable: {e}"))?;
    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.leeway = 0;
    validation.required_spec_claims = ["exp".to_owned()].into();
    jsonwebtoken::decode::<Seen>(claim, &decoding, &validation)
        .map(|data| data.claims)
        .map_err(|e| format!("bad claim: {e}"))
}
