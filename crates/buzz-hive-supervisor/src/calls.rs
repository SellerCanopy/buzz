//! What the calls do. A call names an agent (and, for a sign-in, one of its tool servers) and
//! nothing else about the host: the files it writes and the programs it runs are fixed by the
//! supervisor's own settings.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use buzz_hive_core::domain::agent_login::CallbackUrl;
use buzz_hive_core::domain::secret::{check_value, check_var, SecretNames};
use buzz_hive_core::domain::tool_server::{
    check_agent, check_name, parse_mcp, render_mcp, ToolServer, Transport,
};
use buzz_hive_core::ports::secrets::SecretScope;
use buzz_hive_core::supervisor_wire::{Call, Reply, Verifier};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::config::{Config, Restart, Secrets};
use crate::login::{Limits, LoginError, Logins};
use crate::secrets::{FileSecretSource, NoSecrets, SecretSource, SsmSecretSource};

/// The comment line that lists the variables an apply filled in, so the next apply can take
/// out the ones no longer granted. It holds names only.
const INJECTED: &str = "# buzz-hive-injected: ";

/// The longest a restart may take. Compose waits up to the container's 60 second stop grace.
const RESTART_TIMEOUT: Duration = Duration::from_secs(180);

pub struct Service {
    config: Config,
    verifier: Verifier,
    /// The only reader of secret values.
    secrets: Arc<dyn SecretSource>,
    /// One change at a time, so two applies never interleave their files and restarts.
    writes: Mutex<()>,
    /// The sign-ins waiting for a paste.
    logins: Logins,
}

enum Failure {
    NotFound(String),
    /// The call was understood and the other side said no, for example a sign-in the CLI did
    /// not accept. Starting over is the way on.
    Rejected(String),
    Failed(String),
}

impl Failure {
    fn failed(message: impl ToString) -> Self {
        Self::Failed(message.to_string())
    }
}

impl Service {
    pub fn new(config: Config) -> Result<Self, String> {
        let secrets: Arc<dyn SecretSource> = match &config.secrets {
            Secrets::None => Arc::new(NoSecrets),
            Secrets::Dir(dir) => Arc::new(FileSecretSource { dir: dir.clone() }),
            Secrets::Ssm { cli } => Arc::new(SsmSecretSource { cli: cli.clone() }),
        };
        Self::with_secrets(config, secrets)
    }

    /// A service that reads secret values from `secrets`, whatever the config says.
    pub fn with_secrets(config: Config, secrets: Arc<dyn SecretSource>) -> Result<Self, String> {
        let verifier = Verifier::new(&config.engine_key)?;
        Ok(Self {
            config,
            verifier,
            secrets,
            writes: Mutex::new(()),
            logins: Logins::new(Limits::default()),
        })
    }

    /// The same service with other login times, which a test shortens.
    pub fn with_login_limits(mut self, limits: Limits) -> Self {
        self.logins = Logins::new(limits);
        self
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
        // Of a sign-in call only the agent is logged: the pasted address can carry a code, and
        // the link is the sign-in itself.
        let (agent, outcome) = match call {
            Call::ApplyAgentConfig {
                agent,
                servers,
                secrets,
            } => {
                let outcome = self.apply(&agent, &servers, &secrets).await;
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
            Call::LoginStart { agent, server } => {
                let outcome = self.login_start(&agent, &server).await;
                (agent, outcome)
            }
            Call::LoginPaste { agent, server, url } => {
                let outcome = self.login_paste(&agent, &server, &url).await;
                (agent, outcome)
            }
            Call::LoginStatus { agent, server } => {
                let outcome = self.login_status(&agent, &server).await;
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
            Err(Failure::Rejected(message)) => {
                tracing::warn!(method, %agent, %message, "call rejected");
                Reply::failed("rejected", message)
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
    async fn apply(
        &self,
        agent: &str,
        servers: &[ToolServer],
        secrets: &SecretNames,
    ) -> Result<bool, Failure> {
        self.known(agent)?;
        let _one_at_a_time = self.writes.lock().await;
        let paths = self.paths(agent);
        let before = read_env(&paths.env)?;
        let mut base = parse_env(&before);
        // What an earlier apply filled in comes out first, so a secret no longer granted is
        // gone from the file and one still granted is read again (a rotated value restarts).
        for var in injected(&before) {
            base.remove(&var);
        }
        let mut env = render_env(&self.config.hive_pubkey, &base);
        let filled = self.fill(agent, secrets, &mut env).await?;
        let env_text = format_env_with(&env, &filled);
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

    /// Reads each secret `secrets` names and puts it in `env`. An agent's own secret wins over
    /// a team secret of the same name. A secret that is not in the store, or whose value the
    /// env file could not hold, fails the apply and says which name, never the value, so no
    /// agent starts half filled.
    async fn fill(
        &self,
        agent: &str,
        secrets: &SecretNames,
        env: &mut BTreeMap<String, String>,
    ) -> Result<BTreeSet<String>, Failure> {
        let wanted = secrets
            .team
            .iter()
            .filter(|var| !secrets.own.contains(var))
            .map(|var| (SecretScope::Team, var))
            .chain(
                secrets
                    .own
                    .iter()
                    .map(|var| (SecretScope::Agent(agent.to_owned()), var)),
            );
        let mut filled = BTreeSet::new();
        for (scope, var) in wanted {
            check_var(var).map_err(Failure::failed)?;
            let value = self
                .secrets
                .read(&scope, var)
                .await
                .map_err(Failure::failed)?
                .ok_or_else(|| {
                    Failure::failed(format!(
                        "the secret {var} is granted but not set in the secret store"
                    ))
                })?;
            check_value(value.expose())
                .map_err(|e| Failure::failed(format!("the secret {var} cannot be written: {e}")))?;
            env.insert(var.clone(), value.expose().to_owned());
            filled.insert(var.clone());
        }
        Ok(filled)
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
                let mut command = compose(&self.config, compose_file, agent);
                command.args(["up", "-d", "--force-recreate"]);
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

    /// The agent's HTTP tool servers are the only ones a sign-in may name: the server is one the
    /// admin set and the supervisor wrote, never a name the call made up.
    fn known_http_server(&self, agent: &str, server: &str) -> Result<(), Failure> {
        self.known(agent)?;
        check_name(server).map_err(Failure::failed)?;
        let listed = read_if_there(&self.paths(agent).mcp)
            .and_then(|text| parse_mcp(&text).ok())
            .unwrap_or_default();
        if listed
            .iter()
            .any(|s| s.name == server && s.transport == Transport::Http)
        {
            Ok(())
        } else {
            Err(Failure::NotFound(format!(
                "agent {agent} has no HTTP tool server named {server}"
            )))
        }
    }

    async fn login_start(&self, agent: &str, server: &str) -> Result<serde_json::Value, Failure> {
        self.known_http_server(agent, server)?;
        let link = self
            .logins
            .start(&self.config, agent, server)
            .await
            .map_err(login_failure)?;
        Ok(json!({ "url": link.expose() }))
    }

    async fn login_paste(
        &self,
        agent: &str,
        server: &str,
        url: &CallbackUrl,
    ) -> Result<serde_json::Value, Failure> {
        self.known_http_server(agent, server)?;
        self.logins
            .paste(agent, server, url)
            .await
            .map_err(login_failure)?;
        Ok(json!({ "signed_in": true }))
    }

    async fn login_status(&self, agent: &str, server: &str) -> Result<serde_json::Value, Failure> {
        self.known_http_server(agent, server)?;
        let state = self
            .logins
            .status(&self.config, agent, server)
            .await
            .map_err(login_failure)?;
        match state {
            Some(Ok(state)) => Ok(json!({ "state": state.as_str() })),
            Some(Err(())) => Err(Failure::failed(format!(
                "the agent reports {server} as neither signed in nor waiting for a sign-in; see the agent's log"
            ))),
            None => Err(Failure::NotFound(format!(
                "the agent does not list a tool server named {server}"
            ))),
        }
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

/// `docker compose` on the agent's own Compose project, with the env file the deploy rendered
/// for it. The restart and the sign-in's `exec` both start from this.
pub(crate) fn compose(config: &Config, compose_file: &Path, agent: &str) -> Command {
    let mut command = Command::new("docker");
    command
        .args(["compose", "-p"])
        .arg(format!("buzz-agent-{agent}"))
        .arg("-f")
        .arg(compose_file)
        .arg("--env-file")
        .arg(config.render_dir.join(format!("{agent}.compose.env")));
    command
}

fn login_failure(error: LoginError) -> Failure {
    match error {
        LoginError::NoSession => Failure::NotFound(
            "no sign-in is waiting for this tool server (it may have expired); start it again"
                .into(),
        ),
        LoginError::Rejected => Failure::Rejected(
            "the sign-in did not accept that address; start the sign-in again".into(),
        ),
        LoginError::Failed(message) => Failure::failed(message),
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

/// [`format_env`], with a first line naming the variables that were filled in from the secret
/// store. No line when none were. A comment line is valid in an env file.
pub fn format_env_with(env: &BTreeMap<String, String>, filled: &BTreeSet<String>) -> String {
    let mut text = String::new();
    if !filled.is_empty() {
        let names: Vec<_> = filled.iter().map(String::as_str).collect();
        text = format!("{INJECTED}{}\n", names.join(","));
    }
    text.push_str(&format_env(env));
    text
}

/// The variables the file's marker line says an earlier apply filled in.
pub fn injected(text: &str) -> BTreeSet<String> {
    text.lines()
        .find_map(|line| line.strip_prefix(INJECTED))
        .map(|names| {
            names
                .split(',')
                .map(str::trim)
                .filter(|name| check_var(name).is_ok())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
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
    fn the_marker_line_names_what_was_filled_in_and_the_file_still_parses() {
        let env = env(&[("A", "1"), ("GH_TOKEN", "x")]);
        let filled: BTreeSet<String> = ["GH_TOKEN".to_owned(), "DOCS_TOKEN".to_owned()].into();
        let text = format_env_with(&env, &filled);
        assert_eq!(
            text,
            "# buzz-hive-injected: DOCS_TOKEN,GH_TOKEN\nA=1\nGH_TOKEN=x\n"
        );
        assert_eq!(injected(&text), filled);
        assert_eq!(parse_env(&text), env);
        assert_eq!(format_env_with(&env, &BTreeSet::new()), format_env(&env));
        assert!(injected("A=1\n").is_empty());
        // A marker naming a reserved variable is not trusted.
        assert!(injected("# buzz-hive-injected: BUZZ_PRIVATE_KEY,lower\n").is_empty());
    }

    #[test]
    fn an_env_file_round_trips_and_keeps_a_value_with_an_equals_sign() {
        let parsed = parse_env("# a comment\nA=1\n\nB=x=y\nbroken line\n");
        assert_eq!(parsed, env(&[("A", "1"), ("B", "x=y")]));
        assert_eq!(format_env(&parsed), "A=1\nB=x=y\n");
    }
}
