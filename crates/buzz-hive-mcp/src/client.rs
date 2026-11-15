//! Calls Beehive as one Buzz agent: trade a signed NIP-98 event for a token, keep the token
//! until shortly before it expires, and forward each call.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use buzz_hive_api::routes::path;
use buzz_hive_api::AUTH_TOKEN_PATH;
use nostr::nips::nip98::{HttpData, HttpMethod};
use nostr::{EventBuilder, JsonUtil, Keys, Tag, TagKind, Url};
use serde_json::Value;
use tokio::sync::Mutex;

/// A token is traded for a new one this long before it expires.
pub const RENEW_BEFORE_SECS: i64 = 60;
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
}

// Not derived: the key must never reach a log line.
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("hive_url", &self.hive_url)
            .field("agent", &self.keys.public_key().to_hex())
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
        Ok(Self { hive_url, keys })
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

pub struct HiveClient {
    http: reqwest::Client,
    base: String,
    keys: Keys,
    clock: Arc<dyn Clock>,
    token: Mutex<Option<Token>>,
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
            clock,
            token: Mutex::new(None),
        }
    }

    /// Calls the route with `arguments` as the request body and returns the reply's JSON.
    /// A call refused as unauthenticated is tried once more on a new token, in case this one
    /// was cut short. Several calls refused on the same token share one new one.
    pub async fn call(&self, route: &str, arguments: Value) -> Result<Value, CallError> {
        let token = self.token(None).await?;
        match self.send(route, &arguments, &token).await {
            Err(error) if error.code == "unauthenticated" => {
                let renewed = self.token(Some(&token)).await?;
                self.send(route, &arguments, &renewed).await
            }
            other => other,
        }
    }

    async fn send(&self, route: &str, arguments: &Value, token: &str) -> Result<Value, CallError> {
        let response = self
            .http
            .post(format!("{}{}", self.base, path(route)))
            .bearer_auth(token)
            .json(arguments)
            .send()
            .await
            .map_err(|e| self.unreachable(e))?;
        read_reply(response).await
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
