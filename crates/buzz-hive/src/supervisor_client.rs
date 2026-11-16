//! The engine's side of the host supervisor: signed calls over a Unix socket.
//!
//! One connection per call, one JSON line each way. Every failure comes back as a
//! [`SupervisorError`], so a supervisor that is down never fails the change that was saved.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use buzz_hive_core::domain::agent_login::{CallbackUrl, LoginState};
use buzz_hive_core::domain::secret::SecretNames;
use buzz_hive_core::domain::tool_server::ToolServer;
use buzz_hive_core::ports::secrets::SecretString;
use buzz_hive_core::ports::supervisor::{AgentSettings, Supervisor, SupervisorError};
use buzz_hive_core::supervisor_wire::{Call, Reply, Signer, MAX_LINE_BYTES};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// How long each kind of call waits for the supervisor's answer.
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    /// An apply waits for the agent's restart, which can take a while. The supervisor itself
    /// gives up on a call that waited more than 30 seconds for the change ahead of it, and
    /// drops a call whose connection this side has closed.
    pub apply: Duration,
    /// Reading settings only reads a file.
    pub read: Duration,
    /// A sign-in start waits up to 30 seconds for the CLI's link, a paste up to 30 for it to
    /// take the address. This leaves room for the socket round trip.
    pub login: Duration,
    /// `claude mcp list` health-checks every server, so a status can take a minute.
    pub login_status: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            apply: Duration::from_secs(120),
            read: Duration::from_secs(10),
            login: Duration::from_secs(40),
            login_status: Duration::from_secs(70),
        }
    }
}

pub struct UnixSupervisor {
    socket: PathBuf,
    signer: Signer,
    timeouts: Timeouts,
}

impl UnixSupervisor {
    /// Signs with the key `token_secret` derives, so the supervisor needs only its public
    /// half ([`Self::public_hex`]).
    pub fn new(socket: PathBuf, token_secret: &[u8]) -> Result<Self, String> {
        Ok(Self {
            socket,
            signer: Signer::from_secret(token_secret)?,
            timeouts: Timeouts::default(),
        })
    }

    /// The same client waiting `timeouts` for its answers, which a test shortens.
    pub fn with_timeouts(mut self, timeouts: Timeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    /// The key the supervisor's `BUZZ_HIVE_SUPERVISOR_PUBKEY` must hold.
    pub fn public_hex(&self) -> String {
        self.signer.public_hex()
    }

    async fn call(
        &self,
        call: &Call,
        timeout: Duration,
    ) -> Result<serde_json::Value, SupervisorError> {
        let line = self.signer.sign(call, now_ms());
        let reply = tokio::time::timeout(timeout, exchange(&self.socket, line))
            .await
            .map_err(|_| {
                SupervisorError::Unavailable("the supervisor did not answer in time".into())
            })?
            .map_err(SupervisorError::Unavailable)?;
        if reply.ok {
            return Ok(reply.result.unwrap_or_default());
        }
        let message = reply.message.unwrap_or_default();
        Err(match reply.error.as_deref() {
            Some("not_found") => SupervisorError::NotFound(message),
            // The supervisor tried and failed, for example a restart: worth trying again.
            Some("failed") => SupervisorError::Unavailable(message),
            // Anything else is a refusal of the call itself.
            code => SupervisorError::Refused(format!("{}: {message}", code.unwrap_or("refused"))),
        })
    }
}

#[async_trait::async_trait]
impl Supervisor for UnixSupervisor {
    async fn apply(
        &self,
        agent: &str,
        servers: &[ToolServer],
        secrets: &SecretNames,
    ) -> Result<bool, SupervisorError> {
        let result = self
            .call(
                &Call::ApplyAgentConfig {
                    agent: agent.to_owned(),
                    servers: servers.to_vec(),
                    secrets: secrets.clone(),
                },
                self.timeouts.apply,
            )
            .await?;
        result["restarted"]
            .as_bool()
            .ok_or_else(|| SupervisorError::Unavailable("the reply has no restarted".into()))
    }

    async fn settings(&self, agent: &str) -> Result<AgentSettings, SupervisorError> {
        let result = self
            .call(
                &Call::AgentSettings {
                    agent: agent.to_owned(),
                },
                self.timeouts.read,
            )
            .await?;
        match (
            result["session_policy"].as_str(),
            result["allowlisted"].as_bool(),
        ) {
            (Some(policy), Some(allowlisted)) => Ok(AgentSettings {
                session_policy: policy.to_owned(),
                allowlisted,
            }),
            _ => Err(SupervisorError::Unavailable(
                "the reply does not carry the agent's settings".into(),
            )),
        }
    }

    async fn login_start(
        &self,
        agent: &str,
        server: &str,
    ) -> Result<SecretString, SupervisorError> {
        let result = self
            .call(
                &Call::LoginStart {
                    agent: agent.to_owned(),
                    server: server.to_owned(),
                },
                self.timeouts.login,
            )
            .await?;
        // The reply is dropped here whatever it holds: only the link leaves, wrapped.
        result["url"]
            .as_str()
            .map(SecretString::new)
            .ok_or_else(|| SupervisorError::Unavailable("the reply has no sign-in link".into()))
    }

    async fn login_paste(
        &self,
        agent: &str,
        server: &str,
        url: &CallbackUrl,
    ) -> Result<(), SupervisorError> {
        let result = self
            .call(
                &Call::LoginPaste {
                    agent: agent.to_owned(),
                    server: server.to_owned(),
                    url: url.clone(),
                },
                self.timeouts.login,
            )
            .await?;
        match result["signed_in"].as_bool() {
            Some(true) => Ok(()),
            _ => Err(SupervisorError::Unavailable(
                "the reply does not say the sign-in finished".into(),
            )),
        }
    }

    async fn login_status(&self, agent: &str, server: &str) -> Result<LoginState, SupervisorError> {
        let result = self
            .call(
                &Call::LoginStatus {
                    agent: agent.to_owned(),
                    server: server.to_owned(),
                },
                self.timeouts.login_status,
            )
            .await?;
        result["state"]
            .as_str()
            .and_then(LoginState::parse)
            .ok_or_else(|| SupervisorError::Unavailable("the reply has no sign-in state".into()))
    }
}

async fn exchange(socket: &std::path::Path, mut line: String) -> Result<Reply, String> {
    let stream = UnixStream::connect(socket)
        .await
        .map_err(|e| format!("cannot reach {}: {e}", socket.display()))?;
    let (read, mut write) = stream.into_split();
    line.push('\n');
    write
        .write_all(line.as_bytes())
        .await
        .map_err(|e| format!("cannot send to the supervisor: {e}"))?;
    let mut reader = BufReader::new(read.take(MAX_LINE_BYTES as u64));
    let mut answer = String::new();
    reader
        .read_line(&mut answer)
        .await
        .map_err(|e| format!("cannot read the supervisor's answer: {e}"))?;
    serde_json::from_str(answer.trim_end())
        .map_err(|_| "the supervisor's answer is not a reply".to_owned())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}
