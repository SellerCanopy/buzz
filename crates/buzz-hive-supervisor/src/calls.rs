//! What the three calls do. A call names an agent and nothing else about the host: the
//! files it writes and the program it runs are fixed by the supervisor's own settings.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use buzz_hive_core::domain::tool_server::{check_agent, render_mcp, ToolServer};
use buzz_hive_core::supervisor_wire::{Call, Reply, Verifier};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::config::{Config, Restart};

/// The longest a restart may take. Compose waits up to the container's 60 second stop grace.
const RESTART_TIMEOUT: Duration = Duration::from_secs(180);

pub struct Service {
    config: Config,
    verifier: Verifier,
    /// One change at a time, so two applies never interleave their files and restarts.
    writes: Mutex<()>,
}

enum Failure {
    NotFound(String),
    Failed(String),
}

impl Failure {
    fn failed(message: impl ToString) -> Self {
        Self::Failed(message.to_string())
    }
}

impl Service {
    pub fn new(config: Config) -> Result<Self, String> {
        let verifier = Verifier::new(&config.engine_key)?;
        Ok(Self {
            config,
            verifier,
            writes: Mutex::new(()),
        })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The reply to one request line. A line that is unsigned, badly signed, stale, repeated
    /// or not one of the three calls is refused before any file is read.
    pub async fn handle(&self, line: &str, now_ms: i64) -> Reply {
        let call = match self.verifier.open(line, now_ms) {
            Ok(call) => call,
            Err(refusal) => {
                tracing::warn!(code = refusal.code(), %refusal, "refused a call");
                return Reply::failed(refusal.code(), refusal);
            }
        };
        let method = call.method();
        let (agent, outcome) = match call {
            Call::ApplyAgentConfig { agent, servers } => {
                let outcome = self.apply(&agent, &servers).await;
                (
                    agent,
                    outcome.map(|restarted| json!({ "restarted": restarted })),
                )
            }
            Call::RestartAgent { agent } => {
                let outcome = self.restart_known(&agent).await;
                (agent, outcome.map(|()| json!({ "restarted": true })))
            }
            Call::AgentSettings { agent } => {
                let outcome = self.settings(&agent);
                (agent, outcome)
            }
        };
        match outcome {
            Ok(result) => {
                tracing::info!(method, %agent, "call done");
                Reply::done(result)
            }
            Err(Failure::NotFound(message)) => {
                tracing::warn!(method, %agent, %message, "call refused");
                Reply::failed("not_found", message)
            }
            Err(Failure::Failed(message)) => {
                tracing::error!(method, %agent, %message, "call failed");
                Reply::failed("failed", message)
            }
        }
    }

    /// The agents a call may name: the folders under `deploy/aws/agents`. The name has no `/`
    /// or `..`, so it picks a folder and never a path.
    fn known(&self, agent: &str) -> Result<(), Failure> {
        if check_agent(agent).is_ok() && self.config.agents_src.join(agent).is_dir() {
            Ok(())
        } else {
            Err(Failure::NotFound(format!("no agent folder named {agent}")))
        }
    }

    fn paths(&self, agent: &str) -> Paths {
        let base = self.config.render_dir.join(agent);
        Paths {
            env: self.config.render_dir.join(format!("{agent}.env")),
            mcp_dir: base.join("config"),
            mcp: base.join("config").join("mcp.json"),
            applied: base.join("applied.sha256"),
        }
    }

    /// Makes the agent's files what `servers` and the Beehive settings render to, and restarts
    /// the agent when they changed, or when an earlier restart did not finish. Returns whether
    /// it restarted.
    async fn apply(&self, agent: &str, servers: &[ToolServer]) -> Result<bool, Failure> {
        self.known(agent)?;
        let _one_at_a_time = self.writes.lock().await;
        let paths = self.paths(agent);
        let base = parse_env(&read_env(&paths.env)?);
        let env_text = format_env(&render_env(&self.config.hive_pubkey, &base));
        let mcp_text = render_mcp(servers);
        let hash = digest(&mcp_text, &env_text);

        let files_changed = read_if_there(&paths.mcp).as_deref() != Some(mcp_text.as_str())
            || read_if_there(&paths.env).as_deref() != Some(env_text.as_str());
        if files_changed {
            fs::create_dir_all(&paths.mcp_dir).map_err(Failure::failed)?;
            // The container user reads the mount, so it is world readable and not writable.
            fs::set_permissions(&paths.mcp_dir, fs::Permissions::from_mode(0o755))
                .map_err(Failure::failed)?;
            write_atomic(&paths.mcp, 0o644, &mcp_text)?;
            write_atomic(&paths.env, 0o600, &env_text)?;
        }
        // The mark is written only after a restart worked, so a restart that failed is tried
        // again by the next apply even though the files are already right.
        let unfinished = read_if_there(&paths.applied).as_deref() != Some(hash.as_str());
        if !files_changed && !unfinished {
            return Ok(false);
        }
        self.restart(agent).await?;
        write_atomic(&paths.applied, 0o600, &hash)?;
        Ok(true)
    }

    async fn restart_known(&self, agent: &str) -> Result<(), Failure> {
        self.known(agent)?;
        let _one_at_a_time = self.writes.lock().await;
        self.restart(agent).await
    }

    async fn restart(&self, agent: &str) -> Result<(), Failure> {
        let mut command = match &self.config.restart {
            Restart::Command(program) => {
                let mut command = Command::new(program);
                command.arg(agent);
                command
            }
            Restart::Compose { compose_file } => {
                let mut command = Command::new("docker");
                command
                    .args(["compose", "-p"])
                    .arg(format!("buzz-agent-{agent}"))
                    .arg("-f")
                    .arg(compose_file)
                    .arg("--env-file")
                    .arg(self.config.render_dir.join(format!("{agent}.compose.env")))
                    .args(["up", "-d", "--force-recreate"]);
                command
            }
        };
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let output = tokio::time::timeout(RESTART_TIMEOUT, command.output())
            .await
            .map_err(|_| Failure::failed("the restart took too long"))?
            .map_err(|e| Failure::failed(format!("could not run the restart: {e}")))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: String = stderr
            .chars()
            .rev()
            .take(300)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Err(Failure::failed(format!(
            "the restart exited with {}: {}",
            output.status,
            tail.trim()
        )))
    }

    /// The session policy and respond-to list the agent's env file holds now.
    fn settings(&self, agent: &str) -> Result<serde_json::Value, Failure> {
        self.known(agent)?;
        let env = parse_env(&read_env(&self.paths(agent).env)?);
        let allowlisted = allowlist(&env).contains(&self.config.hive_pubkey);
        Ok(json!({
            "session_policy": env.get("BUZZ_ACP_SESSION_POLICY").map_or("channel", String::as_str),
            "allowlisted": allowlisted,
        }))
    }
}

struct Paths {
    env: PathBuf,
    mcp_dir: PathBuf,
    mcp: PathBuf,
    applied: PathBuf,
}

/// The environment every stage agent needs: one session per thread, answering an allowlist
/// that holds the Beehive account. Entries the list already has stay, and the account is
/// added once.
pub fn render_env(hive_pubkey: &str, base: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut env = base.clone();
    env.insert("BUZZ_ACP_SESSION_POLICY".into(), "thread".into());
    env.insert("BUZZ_ACP_RESPOND_TO".into(), "allowlist".into());
    let mut allowed = allowlist(&env);
    allowed.insert(hive_pubkey.to_owned());
    env.insert(
        "BUZZ_ACP_RESPOND_TO_ALLOWLIST".into(),
        allowed.into_iter().collect::<Vec<_>>().join(","),
    );
    env
}

fn allowlist(env: &BTreeMap<String, String>) -> BTreeSet<String> {
    env.get("BUZZ_ACP_RESPOND_TO_ALLOWLIST")
        .map(|list| {
            list.split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// `KEY=VALUE` lines, as `render-agents-env.sh` writes them: values are unquoted and the value
/// is everything after the first `=`.
pub fn parse_env(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

pub fn format_env(env: &BTreeMap<String, String>) -> String {
    env.iter()
        .map(|(key, value)| format!("{key}={value}\n"))
        .collect()
}

fn digest(mcp: &str, env: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(mcp.as_bytes());
    hash.update([0]);
    hash.update(env.as_bytes());
    hex::encode(hash.finalize())
}

fn read_if_there(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

fn read_env(path: &Path) -> Result<String, Failure> {
    fs::read_to_string(path).map_err(|_| {
        Failure::NotFound("the agent has no rendered environment yet; run a deploy".into())
    })
}

/// Writes a file beside its final name, then renames it, so a reader sees the old file or the
/// whole new one.
fn write_atomic(path: &Path, mode: u32, text: &str) -> Result<(), Failure> {
    let mut name = path
        .file_name()
        .ok_or_else(|| Failure::failed("a path has no file name"))?
        .to_owned();
    name.push(".tmp");
    let tmp = path.with_file_name(name);
    let write = || -> std::io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)?;
        // A leftover file from a crash keeps its old mode, so set it again.
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    };
    write().map_err(|e| {
        let _ = fs::remove_file(&tmp);
        Failure::failed(format!("could not write {}: {e}", path.display()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HIVE: &str = "aa";

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect()
    }

    #[test]
    fn render_env_keeps_existing_allowlist_entries_and_adds_the_account_once() {
        let base = env(&[
            ("BUZZ_ACP_RESPOND_TO_ALLOWLIST", "cc,bb, ,cc"),
            ("BUZZ_PRIVATE_KEY", "k"),
            ("BUZZ_ACP_SESSION_POLICY", "channel"),
        ]);
        let once = render_env(HIVE, &base);
        assert_eq!(once["BUZZ_ACP_RESPOND_TO_ALLOWLIST"], "aa,bb,cc");
        assert_eq!(once["BUZZ_ACP_SESSION_POLICY"], "thread");
        assert_eq!(once["BUZZ_ACP_RESPOND_TO"], "allowlist");
        assert_eq!(once["BUZZ_PRIVATE_KEY"], "k");
        // Rendering what was rendered changes nothing, and the account is not added twice.
        assert_eq!(render_env(HIVE, &once), once);
        assert_eq!(
            render_env(HIVE, &env(&[]))["BUZZ_ACP_RESPOND_TO_ALLOWLIST"],
            "aa"
        );
    }

    #[test]
    fn an_env_file_round_trips_and_keeps_a_value_with_an_equals_sign() {
        let parsed = parse_env("# a comment\nA=1\n\nB=x=y\nbroken line\n");
        assert_eq!(parsed, env(&[("A", "1"), ("B", "x=y")]));
        assert_eq!(format_env(&parsed), "A=1\nB=x=y\n");
    }
}
