//! Calls Beehive as one Buzz agent: trade a signed NIP-98 event for a token, keep the token
//! until shortly before it expires, and forward each call.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use buzz_hive_api::routes::path;
use buzz_hive_api::unary::CLAIM_HEADER;
use buzz_hive_api::{AUTH_TOKEN_PATH, CLAIM_PATH};
use nostr::nips::nip98::{HttpData, HttpMethod};
use nostr::{EventBuilder, JsonUtil, Keys, Tag, TagKind, Url};
use serde_json::Value;
use tokio::sync::Mutex;

/// A token is traded for a new one this long before it expires.
pub const RENEW_BEFORE_SECS: i64 = 60;
/// A requester claim is kept this long for the event or run it was fetched for. A person's
/// level can change, and a newer mention replaces the last, so a claim is asked for again
/// soon. Beehive reads the person's level on every call, so a held claim never holds a level.
pub const CLAIM_CACHE_SECS: i64 = 60;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Seconds since the epoch, injectable so a test can move time.
pub trait Clock: Send + Sync {
    fn now(&self) -> i64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default()
    }
}

pub struct Config {
    /// Where Beehive answers, without a trailing slash.
    pub hive_url: String,
    pub keys: Keys,
    /// The file the agent harness keeps the id of the message that started the current turn
    /// in (`BUZZ_REQUESTER_FILE`). Without it, no claim is made from a chat message.
    pub requester_file: Option<std::path::PathBuf>,
}

// Not derived: the key must never reach a log line.
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("hive_url", &self.hive_url)
            .field("agent", &self.keys.public_key().to_hex())
            .field("requester_file", &self.requester_file)
            .finish_non_exhaustive()
    }
}

impl Config {
    /// Reads `BUZZ_PRIVATE_KEY` (a bech32 nsec or 64 hex characters), then `BUZZ_HIVE_URL`.
    /// With no `BUZZ_HIVE_URL`, Beehive is taken to share the relay's host, as it does behind
    /// the deployment's proxy: `BUZZ_RELAY_URL` with `wss` read as `https` and `ws` as `http`.
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let key = get("BUZZ_PRIVATE_KEY")
            .filter(|v| !v.trim().is_empty())
            .ok_or("BUZZ_PRIVATE_KEY is not set")?;
        let keys = Keys::parse(key.trim()).map_err(|_| "BUZZ_PRIVATE_KEY is not a valid key")?;
        let hive_url = match get("BUZZ_HIVE_URL").filter(|v| !v.trim().is_empty()) {
            Some(url) => url.trim().trim_end_matches('/').to_owned(),
            None => {
                let relay = get("BUZZ_RELAY_URL")
                    .filter(|v| !v.trim().is_empty())
                    .ok_or(
                        "set BUZZ_HIVE_URL, or BUZZ_RELAY_URL for a Beehive on the relay's host",
                    )?;
                hive_url_from_relay(&relay)
                    .ok_or("BUZZ_RELAY_URL must start with ws://, wss://, http:// or https://")?
            }
        };
        let requester_file = get("BUZZ_REQUESTER_FILE")
            .filter(|v| !v.trim().is_empty())
            .map(std::path::PathBuf::from);
        Ok(Self {
            hive_url,
            keys,
            requester_file,
        })
    }
}

/// The relay's address with its websocket scheme read as the matching HTTP one.
pub fn hive_url_from_relay(relay: &str) -> Option<String> {
    let relay = relay.trim().trim_end_matches('/');
    let (scheme, rest) = relay.split_once("://")?;
    let scheme = match scheme {
        "wss" | "https" => "https",
        "ws" | "http" => "http",
        _ => return None,
    };
    Some(format!("{scheme}://{rest}"))
}

/// A refusal from Beehive, kept with its Connect code: `[permission_denied] needs Run, you
/// have Read`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallError {
    pub code: String,
    pub message: String,
}

impl CallError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

struct Token {
    value: String,
    expires_at: i64,
}

/// A requester claim and what it was fetched for.
struct HeldClaim {
    /// `event:<id>` or `run:<id>`.
    basis: String,
    value: String,
    fetched_at: i64,
    expires_at: i64,
}

/// What asking for a claim came to.
enum Claim {
    /// Send this as `x-hive-claim`.
    Held(String),
    /// There was nothing to claim from: no turn started yet, and the call names no run.
    None,
    /// Beehive refused the claim, with its reason. The call goes ahead without one.
    Refused(CallError),
}

pub struct HiveClient {
    http: reqwest::Client,
    base: String,
    keys: Keys,
    requester_file: Option<std::path::PathBuf>,
    clock: Arc<dyn Clock>,
    token: Mutex<Option<Token>>,
    claim: Mutex<Option<HeldClaim>>,
}

impl HiveClient {
    pub fn new(config: Config, clock: Arc<dyn Clock>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("an HTTP client with default settings builds");
        Self {
            http,
            base: config.hive_url,
            keys: config.keys,
            requester_file: config.requester_file,
            clock,
            token: Mutex::new(None),
            claim: Mutex::new(None),
        }
    }

    /// Calls the route with `arguments` as the request body and returns the reply's JSON.
    /// A call refused as unauthenticated is tried once more on a new token and a new claim,
    /// in case either was cut short. Several calls refused on the same token share one new one.
    ///
    /// The call carries a requester claim when the turn that made it has one: the person
    /// who asked is what limits an agent, so an agent that cannot name one acts at Read.
    pub async fn call(&self, route: &str, arguments: Value) -> Result<Value, CallError> {
        let token = self.token(None).await?;
        let claim = self.requester_claim(&arguments, &token, false).await;
        let first = self.send(route, &arguments, &token, claim.header()).await;
        match first {
            Err(error) if error.code == "unauthenticated" => {
                let renewed = self.token(Some(&token)).await?;
                let claim = self.requester_claim(&arguments, &renewed, true).await;
                self.send(route, &arguments, &renewed, claim.header())
                    .await
                    .map_err(|e| claim.explain(e))
            }
            other => other.map_err(|e| claim.explain(e)),
        }
    }

    async fn send(
        &self,
        route: &str,
        arguments: &Value,
        token: &str,
        claim: Option<&str>,
    ) -> Result<Value, CallError> {
        let mut request = self
            .http
            .post(format!("{}{}", self.base, path(route)))
            .bearer_auth(token);
        if let Some(claim) = claim {
            request = request.header(CLAIM_HEADER, claim);
        }
        let response = request
            .json(arguments)
            .send()
            .await
            .map_err(|e| self.unreachable(e))?;
        read_reply(response).await
    }

    /// The claim for this call. The message that started the turn comes first, read from the
    /// harness's file on every call because a new turn rewrites it. A call that names a run
    /// of its own falls back to that. A claim is held for [`CLAIM_CACHE_SECS`] per basis, so a
    /// burst of tool calls makes one request. `fresh` skips the held claim.
    async fn requester_claim(&self, arguments: &Value, token: &str, fresh: bool) -> Claim {
        let event = self
            .requester_file
            .as_ref()
            .and_then(|file| std::fs::read_to_string(file).ok())
            .map(|text| text.trim().to_owned())
            .filter(|id| !id.is_empty());
        let run = arguments["run_id"].as_str().filter(|id| !id.is_empty());
        let (basis, body) = match (event, run) {
            (Some(id), _) => (format!("event:{id}"), serde_json::json!({ "event_id": id })),
            (None, Some(id)) => (format!("run:{id}"), serde_json::json!({ "run_id": id })),
            (None, None) => return Claim::None,
        };
        let mut held = self.claim.lock().await;
        let now = self.clock.now();
        if let Some(claim) = held.as_ref() {
            if !fresh
                && claim.basis == basis
                && now - claim.fetched_at < CLAIM_CACHE_SECS
                && claim.expires_at - RENEW_BEFORE_SECS > now
            {
                return Claim::Held(claim.value.clone());
            }
        }
        *held = None;
        let reply = self
            .http
            .post(format!("{}{CLAIM_PATH}", self.base))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(|e| self.unreachable(e));
        let reply = match reply {
            Ok(response) => read_reply(response).await,
            Err(error) => Err(error),
        };
        match reply {
            Ok(reply) => match (reply["claim"].as_str(), reply["expires_at"].as_i64()) {
                (Some(value), Some(expires_at)) => {
                    *held = Some(HeldClaim {
                        basis,
                        value: value.to_owned(),
                        fetched_at: now,
                        expires_at,
                    });
                    Claim::Held(value.to_owned())
                }
                _ => Claim::Refused(CallError::new(
                    "internal",
                    "Beehive's claim reply has no claim or expiry",
                )),
            },
            Err(error) => {
                tracing::warn!(%error, %basis, "no requester claim");
                Claim::Refused(error)
            }
        }
    }

    /// A token that is good for at least [`RENEW_BEFORE_SECS`] more, trading for one when
    /// there is none. `rejected` is a token Beehive just refused: the held token is kept only
    /// when it is a different one, because another call may already have traded for it. One
    /// trade runs at a time.
    async fn token(&self, rejected: Option<&str>) -> Result<String, CallError> {
        let mut held = self.token.lock().await;
        if let Some(token) = held.as_ref() {
            if token.expires_at - RENEW_BEFORE_SECS > self.clock.now()
                && rejected != Some(token.value.as_str())
            {
                return Ok(token.value.clone());
            }
        }
        let traded = self.trade().await?;
        let value = traded.value.clone();
        *held = Some(traded);
        Ok(value)
    }

    async fn trade(&self) -> Result<Token, CallError> {
        let url = format!("{}{AUTH_TOKEN_PATH}", self.base);
        let parsed = Url::parse(&url)
            .map_err(|e| CallError::new("invalid_argument", format!("{url} is not a URL: {e}")))?;
        // Events carry whole seconds, so two trades in one second would be the same event, and
        // Beehive refuses an event it has seen. The nonce makes each trade its own.
        let nonce = Tag::custom(
            TagKind::custom("nonce"),
            [uuid::Uuid::new_v4().simple().to_string()],
        );
        let event = EventBuilder::http_auth(HttpData::new(parsed, HttpMethod::POST))
            .tag(nonce)
            .sign_with_keys(&self.keys)
            .map_err(|e| CallError::new("internal", format!("could not sign the request: {e}")))?;
        let header = format!(
            "Nostr {}",
            base64::engine::general_purpose::STANDARD.encode(event.as_json())
        );
        let response = self
            .http
            .post(&url)
            .header("authorization", header)
            .send()
            .await
            .map_err(|e| self.unreachable(e))?;
        let reply = read_reply(response).await?;
        let value = reply["token"].as_str().map(str::to_owned);
        let expires_at = reply["expires_at"].as_i64();
        match (value, expires_at) {
            (Some(value), Some(expires_at)) => Ok(Token { value, expires_at }),
            _ => Err(CallError::new(
                "internal",
                "Beehive's token reply has no token or expiry",
            )),
        }
    }

    fn unreachable(&self, error: reqwest::Error) -> CallError {
        tracing::warn!(%error, "could not reach Beehive");
        CallError::new(
            "unavailable",
            format!("could not reach Beehive at {}", self.base),
        )
    }
}

impl Claim {
    fn header(&self) -> Option<&str> {
        match self {
            Claim::Held(value) => Some(value),
            _ => None,
        }
    }

    /// A refusal for want of a level, with why there was no claim, so the agent is told what
    /// to do and not only that it may not.
    fn explain(&self, mut error: CallError) -> CallError {
        if let (Claim::Refused(why), "permission_denied") = (self, error.code.as_str()) {
            error.message = format!(
                "{}. No person is named for this turn: {}",
                error.message, why.message
            );
        }
        error
    }
}

/// The JSON of a success, or the `{code, message}` of a refusal.
async fn read_reply(response: reqwest::Response) -> Result<Value, CallError> {
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|_| CallError::new("unavailable", "Beehive's reply was cut short"))?;
    let json: Option<Value> = serde_json::from_str(&text).ok();
    if status.is_success() {
        return Ok(json.unwrap_or(Value::Null));
    }
    let field = |name: &str| {
        json.as_ref()
            .and_then(|v| v[name].as_str())
            .map(str::to_owned)
    };
    Err(CallError {
        code: field("code").unwrap_or_else(|| {
            if status.is_server_error() {
                "unavailable"
            } else {
                "unknown"
            }
            .to_owned()
        }),
        message: field("message").unwrap_or_else(|| format!("Beehive answered {status}")),
    })
}
