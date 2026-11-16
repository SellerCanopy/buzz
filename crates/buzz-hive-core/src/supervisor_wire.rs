//! The calls between the engine and the host supervisor, and how they are signed.
//!
//! The two meet on a Unix socket and speak one JSON line each way. A request is an envelope
//! `{method, body, timestamp, signature}`: `body` is the call's JSON as text, `timestamp` is
//! milliseconds, and `signature` is Ed25519 over `method`, `body` and `timestamp` joined by
//! newlines. The supervisor refuses a call that is unsigned, badly signed, more than a minute
//! off its clock, a repeat of one it already saw, or not one of the three methods, before it
//! reads the body.
//!
//! The signing key is derived from the engine's token secret, as the claim key is, so no
//! second secret is stored. The supervisor holds the public half only.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};
use serde::{Deserialize, Serialize};

use crate::domain::tool_server::{check_list, ToolServer};

/// How far a call's timestamp may be from the supervisor's clock.
pub const WINDOW_MS: i64 = 60_000;
/// The longest request line the supervisor reads.
pub const MAX_LINE_BYTES: usize = 1 << 20;

/// The supervisor's three calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    /// Make `servers` the agent's whole list and restart it if its files changed.
    ApplyAgentConfig {
        agent: String,
        servers: Vec<ToolServer>,
    },
    /// Restart the agent.
    RestartAgent { agent: String },
    /// Read the agent's settings.
    AgentSettings { agent: String },
}

#[derive(Serialize, Deserialize)]
struct Apply {
    agent: String,
    servers: Vec<ToolServer>,
}

#[derive(Serialize, Deserialize)]
struct Named {
    agent: String,
}

impl Call {
    pub fn method(&self) -> &'static str {
        match self {
            Self::ApplyAgentConfig { .. } => "ApplyAgentConfig",
            Self::RestartAgent { .. } => "RestartAgent",
            Self::AgentSettings { .. } => "AgentSettings",
        }
    }

    fn body(&self) -> String {
        match self {
            Self::ApplyAgentConfig { agent, servers } => serde_json::to_string(&Apply {
                agent: agent.clone(),
                servers: servers.clone(),
            }),
            Self::RestartAgent { agent } | Self::AgentSettings { agent } => {
                serde_json::to_string(&Named {
                    agent: agent.clone(),
                })
            }
        }
        .expect("a call serializes")
    }

    fn parse(method: &str, body: &str) -> Result<Self, Refusal> {
        let bad = |e: serde_json::Error| Refusal::BadBody(e.to_string());
        match method {
            "ApplyAgentConfig" => {
                let Apply { agent, servers } = serde_json::from_str(body).map_err(bad)?;
                // A spec is checked again here, so a caller that skipped the engine's checks
                // still cannot write what the engine would refuse.
                for server in &servers {
                    ToolServer::new(&server.name, server.transport, &server.spec)
                        .map_err(Refusal::BadBody)?;
                }
                check_list(&servers).map_err(Refusal::BadBody)?;
                Ok(Self::ApplyAgentConfig { agent, servers })
            }
            "RestartAgent" => Ok(Self::RestartAgent {
                agent: serde_json::from_str::<Named>(body).map_err(bad)?.agent,
            }),
            "AgentSettings" => Ok(Self::AgentSettings {
                agent: serde_json::from_str::<Named>(body).map_err(bad)?.agent,
            }),
            other => Err(Refusal::UnknownMethod(other.to_owned())),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    method: String,
    body: String,
    timestamp: i64,
    #[serde(default)]
    signature: String,
}

fn message(method: &str, body: &str, timestamp: i64) -> Vec<u8> {
    format!("{method}\n{body}\n{timestamp}").into_bytes()
}

/// The engine's side: signs calls.
pub struct Signer {
    pair: Ed25519KeyPair,
    last: AtomicI64,
}

impl Signer {
    /// The key `secret` derives. The same secret gives the same key on every start.
    pub fn from_secret(secret: &[u8]) -> Result<Self, String> {
        let key = aws_lc_rs::hmac::Key::new(aws_lc_rs::hmac::HMAC_SHA256, secret);
        let seed = aws_lc_rs::hmac::sign(&key, b"buzz-hive supervisor request signing key v1");
        let pair = Ed25519KeyPair::from_seed_unchecked(seed.as_ref())
            .map_err(|_| "could not derive the supervisor signing key")?;
        Ok(Self {
            pair,
            last: AtomicI64::new(0),
        })
    }

    /// The public key as hex, which the supervisor is configured with.
    pub fn public_hex(&self) -> String {
        hex::encode(self.pair.public_key().as_ref())
    }

    /// The request line for `call` at `now_ms`. Two calls never share a timestamp, so two
    /// identical calls in the same millisecond are still different requests.
    pub fn sign(&self, call: &Call, now_ms: i64) -> String {
        let mut timestamp = now_ms;
        let mut last = self.last.load(Ordering::Relaxed);
        loop {
            timestamp = timestamp.max(last + 1);
            match self
                .last
                .compare_exchange(last, timestamp, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(seen) => last = seen,
            }
        }
        self.line(call.method(), call.body(), timestamp)
    }

    /// A signed line for any method and body, which a test needs to reach the supervisor's
    /// refusal of a method that is not one of the three.
    #[cfg(any(test, feature = "testing"))]
    pub fn sign_raw(&self, method: &str, body: &str, now_ms: i64) -> String {
        self.line(method, body.to_owned(), now_ms)
    }

    fn line(&self, method: &str, body: String, timestamp: i64) -> String {
        let signature = hex::encode(self.pair.sign(&message(method, &body, timestamp)).as_ref());
        serde_json::to_string(&Envelope {
            method: method.to_owned(),
            body,
            timestamp,
            signature,
        })
        .expect("an envelope serializes")
    }
}

/// Why the supervisor would not act on a line.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("the request is not a signed call")]
    Unsigned,
    #[error("the signature does not match")]
    BadSignature,
    #[error("the request is more than a minute off the supervisor's clock")]
    Stale,
    #[error("the request was already seen")]
    Replayed,
    #[error("unknown method {0}")]
    UnknownMethod(String),
    #[error("the request body is not accepted: {0}")]
    BadBody(String),
}

impl Refusal {
    /// The short code a reply carries.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unsigned | Self::BadSignature => "unauthenticated",
            Self::Stale => "stale",
            Self::Replayed => "replayed",
            Self::UnknownMethod(_) => "unknown_method",
            Self::BadBody(_) => "bad_request",
        }
    }
}

/// The supervisor's side: checks lines against the engine's public key.
pub struct Verifier {
    public: Vec<u8>,
    /// Signatures seen inside the window, with the timestamp each came with.
    seen: Mutex<HashMap<String, i64>>,
}

impl Verifier {
    pub fn new(public_hex: &str) -> Result<Self, String> {
        let public = hex::decode(public_hex.trim())
            .ok()
            .filter(|bytes| bytes.len() == 32)
            .ok_or("the supervisor public key must be 64 hex characters")?;
        Ok(Self {
            public,
            seen: Mutex::default(),
        })
    }

    /// The call a request line carries, or why it is refused. Nothing is read from the body
    /// until the line is signed, fresh and new.
    pub fn open(&self, line: &str, now_ms: i64) -> Result<Call, Refusal> {
        let envelope: Envelope = serde_json::from_str(line).map_err(|_| Refusal::Unsigned)?;
        if envelope.signature.is_empty() {
            return Err(Refusal::Unsigned);
        }
        if (now_ms - envelope.timestamp).abs() > WINDOW_MS {
            return Err(Refusal::Stale);
        }
        let signature = hex::decode(&envelope.signature).map_err(|_| Refusal::BadSignature)?;
        UnparsedPublicKey::new(&ED25519, &self.public)
            .verify(
                &message(&envelope.method, &envelope.body, envelope.timestamp),
                &signature,
            )
            .map_err(|_| Refusal::BadSignature)?;
        {
            let mut seen = self.seen.lock().expect("the replay set is never poisoned");
            seen.retain(|_, at| (now_ms - *at).abs() <= WINDOW_MS);
            if seen
                .insert(envelope.signature, envelope.timestamp)
                .is_some()
            {
                return Err(Refusal::Replayed);
            }
        }
        Call::parse(&envelope.method, &envelope.body)
    }
}

/// The supervisor's answer, one line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// A short code: `not_found`, `failed`, or a [`Refusal::code`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl Reply {
    pub fn done(result: serde_json::Value) -> Self {
        Self {
            ok: true,
            result: Some(result),
            error: None,
            message: None,
        }
    }

    pub fn failed(code: &str, message: impl ToString) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(code.to_owned()),
            message: Some(message.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::tool_server::Transport;
    use serde_json::json;

    fn pair() -> (Signer, Verifier) {
        let signer = Signer::from_secret(b"a secret of some length").unwrap();
        let verifier = Verifier::new(&signer.public_hex()).unwrap();
        (signer, verifier)
    }

    fn apply() -> Call {
        Call::ApplyAgentConfig {
            agent: "coder".into(),
            servers: vec![
                ToolServer::new("fs", Transport::Stdio, &json!({"command": "x"})).unwrap(),
            ],
        }
    }

    #[test]
    fn a_signed_call_opens_and_the_same_secret_gives_the_same_key() {
        let (signer, verifier) = pair();
        assert_eq!(
            verifier.open(&signer.sign(&apply(), 1_000_000), 1_000_000),
            Ok(apply())
        );
        assert_eq!(
            Signer::from_secret(b"a secret of some length")
                .unwrap()
                .public_hex(),
            signer.public_hex()
        );
    }

    #[test]
    fn unsigned_forged_stale_and_replayed_lines_are_refused() {
        let (signer, verifier) = pair();
        let now = 5_000_000;
        assert_eq!(verifier.open("not json", now), Err(Refusal::Unsigned));
        let line = signer.sign(&apply(), now);
        let mut unsigned: serde_json::Value = serde_json::from_str(&line).unwrap();
        unsigned.as_object_mut().unwrap().remove("signature");
        assert_eq!(
            verifier.open(&unsigned.to_string(), now),
            Err(Refusal::Unsigned)
        );
        let mut swapped = unsigned.clone();
        swapped["signature"] = json!("00".repeat(64));
        assert_eq!(
            verifier.open(&swapped.to_string(), now),
            Err(Refusal::BadSignature)
        );
        // A body changed after signing.
        let mut tampered: serde_json::Value = serde_json::from_str(&line).unwrap();
        tampered["body"] = json!(r#"{"agent":"other","servers":[]}"#);
        assert_eq!(
            verifier.open(&tampered.to_string(), now),
            Err(Refusal::BadSignature)
        );
        // Signed by another key.
        let other = Signer::from_secret(b"another secret entirely").unwrap();
        assert_eq!(
            verifier.open(&other.sign(&apply(), now), now),
            Err(Refusal::BadSignature)
        );
        // Over a minute old, or from the future.
        // A fresh signer, since one signer never goes back in time.
        let (old, _) = pair();
        assert_eq!(
            verifier.open(&old.sign(&apply(), now - WINDOW_MS - 1), now),
            Err(Refusal::Stale)
        );
        let (early, _) = pair();
        assert_eq!(
            verifier.open(&early.sign(&apply(), now + WINDOW_MS + 1), now),
            Err(Refusal::Stale)
        );
        // The same line twice.
        assert!(verifier.open(&line, now).is_ok());
        assert_eq!(verifier.open(&line, now), Err(Refusal::Replayed));
    }

    #[test]
    fn two_identical_calls_in_one_millisecond_are_both_accepted() {
        let (signer, verifier) = pair();
        let call = Call::RestartAgent {
            agent: "coder".into(),
        };
        assert!(verifier.open(&signer.sign(&call, 9_000), 9_000).is_ok());
        assert!(verifier.open(&signer.sign(&call, 9_000), 9_000).is_ok());
    }

    #[test]
    fn a_seen_signature_is_forgotten_once_the_window_passes() {
        let (signer, verifier) = pair();
        let call = Call::AgentSettings {
            agent: "coder".into(),
        };
        assert!(verifier.open(&signer.sign(&call, 1_000), 1_000).is_ok());
        assert!(verifier
            .open(
                &signer.sign(&call, 1_000 + 2 * WINDOW_MS),
                1_000 + 2 * WINDOW_MS
            )
            .is_ok());
        assert_eq!(verifier.seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn an_unknown_method_and_a_bad_body_are_refused_after_the_signature_checks() {
        let (signer, verifier) = pair();
        let sign = |method: &str, body: &str, timestamp: i64| {
            let signature =
                hex::encode(signer.pair.sign(&message(method, body, timestamp)).as_ref());
            json!({"method": method, "body": body, "timestamp": timestamp, "signature": signature})
                .to_string()
        };
        assert_eq!(
            verifier.open(&sign("RunShell", "{}", 10), 10),
            Err(Refusal::UnknownMethod("RunShell".into()))
        );
        assert!(matches!(
            verifier.open(&sign("RestartAgent", r#"{"path":"/etc"}"#, 11), 11),
            Err(Refusal::BadBody(_))
        ));
        let literal = r#"{"agent":"coder","servers":[{"name":"a","transport":"stdio","spec":{"command":"x","env":{"K":"sk-live"}}}]}"#;
        assert!(matches!(
            verifier.open(&sign("ApplyAgentConfig", literal, 12), 12),
            Err(Refusal::BadBody(_))
        ));
    }

    #[test]
    fn a_bad_public_key_is_refused() {
        assert!(Verifier::new("zz").is_err());
        assert!(Verifier::new(&"ab".repeat(31)).is_err());
    }
}
