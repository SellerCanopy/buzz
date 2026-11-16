//! Signing an agent in to an OAuth tool server through the Claude CLI in its container.
//!
//! `LoginStart` runs `claude mcp login <server> --no-browser` and keeps the process waiting, with
//! its stdin held open, in a table keyed by `(agent, server)` for ten minutes. The CLI prints a
//! link, the admin signs in at the provider, and `LoginPaste` writes the address the browser
//! ended on to the process and waits for it to exit. `LoginStatus` runs `claude mcp list` and
//! reads the server's line.
//!
//! Nothing the process prints is ever logged, returned or put in an error, apart from the one
//! link `LoginStart` returns: after a paste the CLI may echo the address, which can carry the
//! authorization code. Every message here is a fixed sentence.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use buzz_hive_core::domain::agent_login::{CallbackUrl, LoginState};
use buzz_hive_core::ports::secrets::SecretString;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::calls::compose;
use crate::config::{Config, Exec};

/// How long a sign-in may wait for its paste.
pub const SESSION_TTL: Duration = Duration::from_secs(10 * 60);
/// How long `LoginStart` waits for the CLI to print a link.
pub const START_WAIT: Duration = Duration::from_secs(30);
/// How long `LoginPaste` waits for the CLI to take the address and exit.
pub const PASTE_WAIT: Duration = Duration::from_secs(30);
/// How long `LoginStatus` waits for `claude mcp list`, which checks every server.
pub const LIST_WAIT: Duration = Duration::from_secs(60);
/// The longest link returned.
const MAX_LINK_CHARS: usize = 4096;

/// The times the login calls use. A test shortens them; the defaults are the ones above.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub ttl: Duration,
    pub start_wait: Duration,
    pub paste_wait: Duration,
    pub list_wait: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            ttl: SESSION_TTL,
            start_wait: START_WAIT,
            paste_wait: PASTE_WAIT,
            list_wait: LIST_WAIT,
        }
    }
}

/// Why a login call did not finish. Each message is a fixed sentence.
#[derive(Debug, PartialEq, Eq)]
pub enum LoginError {
    /// Nothing is waiting, or it expired.
    NoSession,
    /// The CLI took the address and did not sign in.
    Rejected,
    /// The process could not be run, or did not finish in time.
    Failed(&'static str),
}

struct Session {
    id: u64,
    started: Instant,
    child: Child,
    stdin: ChildStdin,
    /// The tasks that keep draining the process's output, so it never blocks on a full pipe.
    readers: Vec<JoinHandle<()>>,
}

impl Session {
    /// Ends the process and waits for it, so it is not left as a zombie.
    async fn end(mut self) {
        for reader in &self.readers {
            reader.abort();
        }
        let _ = self.child.kill().await;
    }
}

type Key = (String, String);
type Table = Arc<Mutex<HashMap<Key, Session>>>;

pub struct Logins {
    table: Table,
    next: AtomicU64,
    limits: Limits,
}

impl Logins {
    pub fn new(limits: Limits) -> Self {
        Self {
            table: Table::default(),
            next: AtomicU64::new(1),
            limits,
        }
    }

    /// Runs the CLI's login for `server` in `agent`'s container and returns the link it
    /// prints. A sign-in already waiting for the same agent and server is ended first.
    pub async fn start(
        &self,
        config: &Config,
        agent: &str,
        server: &str,
    ) -> Result<SecretString, LoginError> {
        let key = (agent.to_owned(), server.to_owned());
        self.drop_expired().await;
        if let Some(old) = self.take(&key) {
            old.end().await;
        }
        let mut command = exec_command(
            config,
            agent,
            &["claude", "mcp", "login", server, "--no-browser"],
        );
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|_| {
            LoginError::Failed("could not run the sign-in in the agent's container")
        })?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill().await;
            return Err(LoginError::Failed("the sign-in process has no pipes"));
        };
        let (lines, mut seen) = mpsc::channel::<String>(64);
        let readers = vec![
            tokio::spawn(forward(BufReader::new(stdout), lines.clone())),
            tokio::spawn(forward(BufReader::new(stderr), lines)),
        ];
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let session = Session {
            id,
            started: Instant::now(),
            child,
            stdin,
            readers,
        };
        // The first link that is a sign-in address. One that is not (a link in an MCP server's
        // own output, say) is never returned, since the admin would trust it as the provider's.
        let mut refused = false;
        let link = tokio::time::timeout(self.limits.start_wait, async {
            while let Some(line) = seen.recv().await {
                if let Some(link) = link_in(&line) {
                    if is_sign_in_link(&link) {
                        return Some(link);
                    }
                    refused = true;
                }
            }
            None
        })
        .await;
        let link = match link {
            Ok(Some(link)) => link,
            Ok(None) => {
                session.end().await;
                return Err(LoginError::Failed(if refused {
                    "the sign-in printed a link that is not a sign-in address"
                } else {
                    "the sign-in ended before it printed a link"
                }));
            }
            Err(_) => {
                session.end().await;
                return Err(LoginError::Failed(
                    "the sign-in did not print a link in time",
                ));
            }
        };
        drop(seen);
        // A start that ran at the same time and finished first is replaced, and its process is
        // ended here rather than left to be dropped: the last start wins.
        let replaced = self
            .table
            .lock()
            .expect("the session table is never poisoned")
            .insert(key.clone(), session);
        if let Some(old) = replaced {
            old.end().await;
        }
        // A session nobody pastes into is ended when it expires, so no process waits forever.
        let table = self.table.clone();
        let ttl = self.limits.ttl;
        tokio::spawn(async move {
            tokio::time::sleep(ttl).await;
            let expired = {
                let mut table = table.lock().expect("the session table is never poisoned");
                match table.get(&key) {
                    Some(session) if session.id == id => table.remove(&key),
                    _ => None,
                }
            };
            if let Some(session) = expired {
                session.end().await;
            }
        });
        Ok(SecretString::new(link))
    }

    /// Writes `url` to the process waiting for `(agent, server)` and waits for it to exit. The
    /// session is used up whatever happens: an address the CLI did not accept needs a new
    /// start, since the provider's code works once.
    pub async fn paste(
        &self,
        agent: &str,
        server: &str,
        url: &CallbackUrl,
    ) -> Result<(), LoginError> {
        self.drop_expired().await;
        let key = (agent.to_owned(), server.to_owned());
        let Some(mut session) = self.take(&key) else {
            return Err(LoginError::NoSession);
        };
        if session.started.elapsed() > self.limits.ttl {
            session.end().await;
            return Err(LoginError::NoSession);
        }
        let written = async {
            session
                .stdin
                .write_all(format!("{}\n", url.expose()).as_bytes())
                .await?;
            session.stdin.flush().await?;
            session.stdin.shutdown().await
        }
        .await;
        if written.is_err() {
            session.end().await;
            return Err(LoginError::Failed(
                "the sign-in process is no longer waiting",
            ));
        }
        match tokio::time::timeout(self.limits.paste_wait, session.child.wait()).await {
            Ok(Ok(status)) if status.success() => {
                session.end().await;
                Ok(())
            }
            Ok(_) => {
                session.end().await;
                Err(LoginError::Rejected)
            }
            Err(_) => {
                session.end().await;
                Err(LoginError::Failed("the sign-in did not finish in time"))
            }
        }
    }

    /// Runs `claude mcp list` in the agent's container and reads `server`'s state. `None` when
    /// there is no line for the server. `Some(Err(()))` when the line is in a state that is
    /// neither sign-in state.
    pub async fn status(
        &self,
        config: &Config,
        agent: &str,
        server: &str,
    ) -> Result<Option<Result<LoginState, ()>>, LoginError> {
        let mut command = exec_command(config, agent, &["claude", "mcp", "list"]);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let output = tokio::time::timeout(self.limits.list_wait, command.output())
            .await
            .map_err(|_| LoginError::Failed("the agent did not list its tool servers in time"))?
            .map_err(|_| {
                LoginError::Failed("could not run the listing in the agent's container")
            })?;
        if !output.status.success() {
            return Err(LoginError::Failed(
                "the agent could not list its tool servers",
            ));
        }
        Ok(parse_list(&String::from_utf8_lossy(&output.stdout), server))
    }

    /// Ends every sign-in waiting in `agent`'s container. A restart recreates the container, so
    /// each one is already lost, and a later paste then says to start again.
    pub async fn end_agent(&self, agent: &str) {
        let ended: Vec<Session> = {
            let mut table = self
                .table
                .lock()
                .expect("the session table is never poisoned");
            let keys: Vec<Key> = table.keys().filter(|k| k.0 == agent).cloned().collect();
            keys.iter().filter_map(|k| table.remove(k)).collect()
        };
        for session in ended {
            session.end().await;
        }
    }

    fn take(&self, key: &Key) -> Option<Session> {
        self.table
            .lock()
            .expect("the session table is never poisoned")
            .remove(key)
    }

    /// Ends every session older than the time to live.
    async fn drop_expired(&self) {
        let expired: Vec<Session> = {
            let mut table = self
                .table
                .lock()
                .expect("the session table is never poisoned");
            let keys: Vec<Key> = table
                .iter()
                .filter(|(_, s)| s.started.elapsed() > self.limits.ttl)
                .map(|(k, _)| k.clone())
                .collect();
            keys.iter().filter_map(|k| table.remove(k)).collect()
        };
        for session in expired {
            session.end().await;
        }
    }
}

/// The command that runs `argv` inside the agent's container: `docker compose exec -T agent`
/// on its Compose project, or the configured program standing in for it.
fn exec_command(config: &Config, agent: &str, argv: &[&str]) -> Command {
    match &config.exec {
        Exec::Compose { compose_file } => {
            let mut command = compose(config, compose_file, agent);
            command.args(["exec", "-T", "agent"]).args(argv);
            command
        }
        Exec::Command(program) => {
            let mut command = Command::new(program);
            command.arg(agent).args(argv);
            command
        }
    }
}

/// Reads lines until the process closes the stream, handing each to `lines` while there is
/// room and dropping it otherwise. The reader must keep going so the process never blocks on
/// a full pipe.
async fn forward<R: tokio::io::AsyncRead + Unpin>(
    mut reader: BufReader<R>,
    lines: mpsc::Sender<String>,
) {
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                let _ = lines.try_send(line.clone());
            }
        }
    }
}

/// The first `https://` address on a line, without the punctuation a sentence puts after it.
fn link_in(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let candidate = line[start..]
        .split_whitespace()
        .next()?
        .trim_end_matches(['.', ',', ';', ')', '"', '\'', '>']);
    let usable = candidate.len() > "https://".len()
        && candidate.len() <= MAX_LINK_CHARS
        && candidate.chars().all(|c| c.is_ascii_graphic());
    usable.then(|| candidate.to_owned())
}

/// Whether `link` is the authorization address of a sign-in this supervisor started: `https`,
/// a host with no user in front of it, and a `redirect_uri` that is a localhost callback
/// (`http://localhost:PORT/...`, or `127.0.0.1`), which is where the CLI waits for the browser.
///
/// The shape is the one the stub prints and the OAuth authorization-code flow defines. What the
/// real `claude mcp login --no-browser` prints is not verified (see PORTING.md, spike 8).
fn is_sign_in_link(link: &str) -> bool {
    let Some(rest) = link.strip_prefix("https://") else {
        return false;
    };
    let (authority, tail) = rest.split_at(rest.find(['/', '?', '#']).unwrap_or(rest.len()));
    if authority.is_empty() || authority.contains(['@', '\\']) || authority.starts_with(':') {
        return false;
    }
    let Some((_, query)) = tail.split_once('?') else {
        return false;
    };
    let query = query.split('#').next().unwrap_or_default();
    let mut redirects = query
        .split('&')
        .filter_map(|pair| pair.strip_prefix("redirect_uri="));
    let (Some(redirect), None) = (redirects.next(), redirects.next()) else {
        return false;
    };
    let Some(redirect) = percent_decode(redirect) else {
        return false;
    };
    let Some(after) = redirect.strip_prefix("http://") else {
        return false;
    };
    let Some((host, rest)) = after.split_once(':') else {
        return false;
    };
    let port = rest.split(['/', '?']).next().unwrap_or_default();
    (host == "localhost" || host == "127.0.0.1")
        && !port.is_empty()
        && port.len() <= 5
        && port.bytes().all(|b| b.is_ascii_digit())
}

/// `text` with its `%XX` escapes decoded, or `None` for a bad escape or non-UTF-8 result.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let pair = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(pair, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The state the Claude CLI's `mcp list` prints on a server's line (the text after the last
/// ` - `): `Needs authentication` or `Connected`, with or without a mark in front. Anything
/// else (`Failed to connect`, a state this does not know) is `None`. The format is the CLI's,
/// so it is read here beside the rest of it and the engine sees only the wire form.
fn state_from_cli(status: &str) -> Option<LoginState> {
    let status = status.to_ascii_lowercase();
    if status.contains("needs auth") || status.contains("needs sign") {
        Some(LoginState::NeedsSignIn)
    } else if status.contains("connected") && !status.contains("not connected") {
        Some(LoginState::Connected)
    } else {
        None
    }
}

/// The state `claude mcp list` prints for `server`, whose line reads
/// `<server>: <command or url> - <mark> <state>`. `None` when no line is for the server, and
/// `Some(Err(()))` when it is in a state that is neither of the two (it failed to connect).
pub fn parse_list(output: &str, server: &str) -> Option<Result<LoginState, ()>> {
    output.lines().find_map(|line| {
        let rest = line.strip_prefix(server)?.strip_prefix(": ")?;
        let status = rest.rsplit_once(" - ").map_or(rest, |(_, status)| status);
        Some(state_from_cli(status).ok_or(()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cli_status_text_maps_to_a_state() {
        assert_eq!(
            state_from_cli("⚠ Needs authentication"),
            Some(LoginState::NeedsSignIn)
        );
        assert_eq!(state_from_cli("✓ Connected"), Some(LoginState::Connected));
        assert_eq!(state_from_cli("✗ Failed to connect"), None);
        assert_eq!(state_from_cli("Not connected"), None);
    }

    #[test]
    fn the_production_exec_runs_the_cli_in_the_agents_container_through_compose() {
        let config = Config::from_lookup(|name| match name {
            "BUZZ_HIVE_SUPERVISOR_SOCKET" => Some("/run/s.sock".into()),
            "BUZZ_HIVE_SUPERVISOR_PUBKEY" | "BUZZ_HIVE_SUPERVISOR_HIVE_PUBKEY" => {
                Some("ab".repeat(32))
            }
            "BUZZ_HIVE_SUPERVISOR_AGENTS_SRC" => Some("/opt/buzz/deploy/aws/agents".into()),
            "BUZZ_HIVE_SUPERVISOR_RENDER_DIR" => Some("/opt/buzz/agents".into()),
            _ => None,
        })
        .unwrap();
        let command = exec_command(
            &config,
            "coder",
            &["claude", "mcp", "login", "docs", "--no-browser"],
        );
        let command = command.as_std();
        assert_eq!(command.get_program(), "docker");
        let args: Vec<_> = command.get_args().map(|a| a.to_string_lossy()).collect();
        assert_eq!(
            args,
            [
                "compose",
                "-p",
                "buzz-agent-coder",
                "-f",
                "/opt/buzz/deploy/aws/compose.agents.yml",
                "--env-file",
                "/opt/buzz/agents/coder.compose.env",
                "exec",
                "-T",
                "agent",
                "claude",
                "mcp",
                "login",
                "docs",
                "--no-browser",
            ]
        );
        // The stand-in program gets the agent's name first and then the whole argv.
        let mut config = config;
        config.exec = Exec::Command("/usr/bin/stub".into());
        let command = exec_command(&config, "coder", &["claude", "mcp", "list"]);
        let args: Vec<_> = command
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy())
            .collect();
        assert_eq!(command.as_std().get_program(), "/usr/bin/stub");
        assert_eq!(args, ["coder", "claude", "mcp", "list"]);
    }

    #[test]
    fn a_link_is_the_first_https_address_on_a_line() {
        assert_eq!(
            link_in("  https://auth.example.org/authorize?a=1&b=2"),
            Some("https://auth.example.org/authorize?a=1&b=2".into())
        );
        assert_eq!(
            link_in("Open this link: https://auth.example.org/a?x=1."),
            Some("https://auth.example.org/a?x=1".into())
        );
        assert_eq!(link_in("redirect http://localhost:1/cb"), None);
        assert_eq!(link_in("https://"), None);
        assert_eq!(link_in("Paste the final address here:"), None);
    }

    #[test]
    fn only_an_authorization_link_with_a_localhost_callback_is_a_sign_in() {
        let good = "https://auth.example.org/oauth/authorize?client_id=stub&state=S&redirect_uri=http%3A%2F%2Flocalhost%3A45219%2Fcallback";
        assert!(is_sign_in_link(good));
        assert!(is_sign_in_link(
            "https://auth.example.org/a?redirect_uri=http%3A%2F%2F127.0.0.1%3A1%2Fcb#frag"
        ));
        for bad in [
            // Not https, or no address at all.
            "http://auth.example.org/a?redirect_uri=http%3A%2F%2Flocalhost%3A1%2Fcb",
            "https://",
            // A user in front of the host, or a backslash that some parsers read as a slash.
            "https://auth.example.org@evil.example.org/a?redirect_uri=http%3A%2F%2Flocalhost%3A1%2Fcb",
            "https://evil.example.org\\@auth.example.org/a?redirect_uri=http%3A%2F%2Flocalhost%3A1%2Fcb",
            // No redirect, or one that is not a localhost callback.
            "https://auth.example.org/a?client_id=x",
            "https://auth.example.org/a?redirect_uri=https%3A%2F%2Fevil.example.org%2Fcb",
            "https://auth.example.org/a?redirect_uri=http%3A%2F%2Flocalhost.evil.example.org%3A1%2Fcb",
            "https://auth.example.org/a?redirect_uri=http%3A%2F%2Flocalhost%2Fcb",
            "https://auth.example.org/a?redirect_uri=http%3A%2F%2Flocalhost%3A1x%2Fcb",
            "https://auth.example.org/a?redirect_uri=http%3A%2F%2Flocalhost%3A1%2Fcb&redirect_uri=http%3A%2F%2Fevil.example.org",
            // The callback sits in another parameter's value, not in redirect_uri.
            "https://auth.example.org/a?next=redirect_uri%3Dhttp%3A%2F%2Flocalhost%3A1%2Fcb",
            // A bad escape.
            "https://auth.example.org/a?redirect_uri=http%3A%2F%2Flocalhost%3A1%2Fcb%zz",
        ] {
            assert!(!is_sign_in_link(bad), "{bad}");
        }
    }

    #[test]
    fn the_list_line_for_a_server_names_its_state() {
        let out = "Checking MCP server health...\n\n\
            filesystem: npx -y pkg /home/agent - ✓ Connected\n\
            docs: https://mcp.example.org/mcp (HTTP) - ⚠ Needs authentication\n\
            docs-2: https://mcp.example.org/two (HTTP) - ✓ Connected\n\
            broken: https://mcp.example.org/b (HTTP) - ✗ Failed to connect\n";
        assert_eq!(parse_list(out, "docs"), Some(Ok(LoginState::NeedsSignIn)));
        assert_eq!(parse_list(out, "docs-2"), Some(Ok(LoginState::Connected)));
        assert_eq!(
            parse_list(out, "filesystem"),
            Some(Ok(LoginState::Connected))
        );
        assert_eq!(parse_list(out, "broken"), Some(Err(())));
        assert_eq!(parse_list(out, "missing"), None);
        // A server whose name starts another's is not that server.
        assert_eq!(parse_list(out, "doc"), None);
    }
}
