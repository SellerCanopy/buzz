//! A scripted agent that stands in for an LLM: it holds its own key, listens on the relay
//! for a message that mentions it, and plays a list of tool calls through the real
//! `buzz-hive-mcp` binary over stdio.
//!
//! Like the real harness, it records the id of the message that starts each turn in the file
//! the shim was told about (`BUZZ_REQUESTER_FILE`), so a call acts for the person that
//! message names. [`ScriptAgent::ask`] is a person starting a turn; a mention the agent
//! waits for starts one too.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use buzz_ws_client::{NostrWsConnection, RelayMessage};
use nostr::Keys;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use uuid::Uuid;

const REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// One step of a script.
pub enum Step {
    /// Call a tool. A string argument `"$card"` is replaced by the id of the first card a
    /// reply returned.
    Call(&'static str, Value),
    /// Wait for the next message that mentions the agent.
    AwaitMention,
}

/// What a tool call returned.
#[derive(Debug, Clone)]
pub struct Reply {
    pub is_error: bool,
    pub text: String,
}

impl Reply {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or_default()
    }
}

pub struct ScriptAgent {
    keys: Keys,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    relay: NostrWsConnection,
    next_id: u64,
    card: Option<String>,
    /// What each thread has told the agent, by the id of the thread's root event. A real
    /// agent opens one session per thread, so this is the history that session would hold.
    sessions: BTreeMap<String, Vec<String>>,
    channel: Uuid,
    /// Where the id of the message that started the turn is kept. Beside the shim's own
    /// directory, so it goes when the agent does.
    requester_dir: tempfile::TempDir,
}

/// The id of the thread `event` belongs to: the root it replies in, or its own id when it
/// opens a thread.
pub fn thread_of(event: &nostr::Event) -> String {
    event
        .tags
        .iter()
        .map(|t| t.clone().to_vec())
        .find(|t| t[0] == "e")
        .map(|t| t[1].clone())
        .unwrap_or_else(|| event.id.to_hex())
}

/// The built `buzz-hive-mcp`, built now if it is not there. `CARGO_BIN_EXE_*` only covers
/// binaries of the package under test, so this looks beside the test binary.
fn shim_binary() -> PathBuf {
    let status =
        std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args(["build", "-p", "buzz-hive-mcp"])
            .status()
            .expect("run cargo to build buzz-hive-mcp");
    assert!(status.success(), "cargo could not build buzz-hive-mcp");
    let test_binary = std::env::current_exe().unwrap();
    // target/<profile>/deps/<test> -> target/<profile>/buzz-hive-mcp
    let profile_dir = test_binary.parent().unwrap().parent().unwrap();
    let binary = profile_dir.join(format!("buzz-hive-mcp{}", std::env::consts::EXE_SUFFIX));
    assert!(binary.exists(), "{} was not built", binary.display());
    binary
}

impl ScriptAgent {
    /// Connects to the relay as `keys`, subscribes to the messages in `channel` that mention
    /// it, and starts the shim against Beehive at `hive_url`.
    pub async fn start(keys: Keys, relay_url: &str, hive_url: &str, channel: Uuid) -> Self {
        let mut relay = NostrWsConnection::connect_authenticated(relay_url, &keys, None)
            .await
            .expect("connect to the relay");
        relay
            .send_raw(&json!([
                "REQ",
                "agent",
                {"kinds": [9], "#h": [channel.to_string()], "#p": [keys.public_key().to_hex()]}
            ]))
            .await
            .unwrap();
        // Everything before the end-of-stored-events marker is old.
        loop {
            match relay.next_event(Duration::from_secs(15)).await.unwrap() {
                RelayMessage::Eose { .. } => break,
                RelayMessage::Closed { message, .. } => {
                    panic!("the relay closed the subscription: {message}")
                }
                _ => {}
            }
        }

        let requester_dir = tempfile::tempdir().expect("a directory for the requester file");
        let mut child = tokio::process::Command::new(shim_binary())
            .env("BUZZ_PRIVATE_KEY", keys.secret_key().to_secret_hex())
            .env("BUZZ_RELAY_URL", relay_url)
            .env("BUZZ_HIVE_URL", hive_url)
            .env("BUZZ_REQUESTER_FILE", requester_dir.path().join("turn"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .expect("start buzz-hive-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut agent = Self {
            keys,
            child,
            stdin,
            stdout,
            relay,
            next_id: 0,
            card: None,
            sessions: BTreeMap::new(),
            channel,
            requester_dir,
        };
        let init = agent
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "script-agent", "version": "0"}
                }),
            )
            .await;
        assert!(
            init["result"]["serverInfo"]["name"] == "buzz-hive-mcp",
            "{init}"
        );
        agent.notify("notifications/initialized").await;
        agent
    }

    async fn send(&mut self, message: Value) {
        let mut line = message.to_string();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await.unwrap();
        self.stdin.flush().await.unwrap();
    }

    async fn notify(&mut self, method: &str) {
        self.send(json!({"jsonrpc": "2.0", "method": method})).await;
    }

    /// Sends a request and returns the response with its id, skipping any notification.
    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        loop {
            let mut line = String::new();
            let read = tokio::time::timeout(REPLY_TIMEOUT, self.stdout.read_line(&mut line))
                .await
                .unwrap_or_else(|_| panic!("buzz-hive-mcp did not answer {method}"))
                .unwrap();
            assert!(read > 0, "buzz-hive-mcp closed its output during {method}");
            let message: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("not JSON from buzz-hive-mcp: {e}: {line}"));
            if message["id"] == id {
                return message;
            }
        }
    }

    /// The names of the tools the shim lists.
    pub async fn tool_names(&mut self) -> Vec<String> {
        let response = self.request("tools/list", json!({})).await;
        response["result"]["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("{response}"))
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect()
    }

    pub async fn call(&mut self, tool: &str, arguments: Value) -> Reply {
        let arguments = self.fill(arguments);
        let response = self
            .request("tools/call", json!({"name": tool, "arguments": arguments}))
            .await;
        assert!(response.get("error").is_none(), "{tool}: {response}");
        let result = &response["result"];
        let reply = Reply {
            is_error: result["isError"] == true,
            text: result["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        };
        if self.card.is_none() && !reply.is_error {
            self.card = reply.json()["id"].as_str().map(str::to_owned);
        }
        reply
    }

    fn fill(&self, value: Value) -> Value {
        match value {
            Value::String(s) if s == "$card" => {
                Value::String(self.card.clone().expect("no card has been returned yet"))
            }
            Value::Object(map) => {
                Value::Object(map.into_iter().map(|(k, v)| (k, self.fill(v))).collect())
            }
            other => other,
        }
    }

    fn requester_path(&self) -> PathBuf {
        self.requester_dir.path().join("turn")
    }

    /// Notes `id` as the message that started the turn, the way the harness does: written
    /// whole and moved into place, so the shim never reads half of it.
    fn record_requester(&self, id: &str) {
        let tmp = self.requester_dir.path().join("turn.tmp");
        std::fs::write(&tmp, id).expect("write the requester file");
        std::fs::rename(&tmp, self.requester_path()).expect("move the requester file into place");
    }

    /// Leaves the agent with no turn to act in, as one started by a heartbeat has.
    pub fn forget_requester(&self) {
        let _ = std::fs::remove_file(self.requester_path());
    }

    /// `person` posts `text` in the channel, mentioning this agent, and the agent's turn
    /// starts from it. Returns the message.
    pub async fn ask(&mut self, person: &Keys, relay_url: &str, text: &str) -> nostr::Event {
        let me = self.keys.public_key().to_hex();
        let event = buzz_sdk::builders::build_message(
            self.channel,
            text,
            None,
            &[me.as_str()],
            false,
            &[],
            &[],
        )
        .expect("build the message")
        .sign_with_keys(person)
        .expect("sign the message");
        let id = event.id.to_hex();
        let mut conn = NostrWsConnection::connect_authenticated(relay_url, person, None)
            .await
            .expect("connect to the relay as the person");
        let ok = conn.send_event(event).await.expect("post the message");
        assert!(ok.accepted, "the relay refused the message: {}", ok.message);
        let _ = conn.disconnect().await;
        loop {
            match self.relay.next_event(Duration::from_secs(60)).await {
                Ok(RelayMessage::Event { event, .. }) if event.id.to_hex() == id => {
                    self.record_requester(&id);
                    return *event;
                }
                Ok(_) => {}
                Err(e) => panic!("the agent never saw the message: {e}"),
            }
        }
    }

    /// The next message that mentions this agent. It starts a turn, so it is the one the
    /// agent's calls act for until the next.
    pub async fn await_mention(&mut self) -> nostr::Event {
        let me = self.keys.public_key().to_hex();
        loop {
            match self.relay.next_event(Duration::from_secs(60)).await {
                Ok(RelayMessage::Event { event, .. }) => {
                    let mentions_me = event
                        .tags
                        .iter()
                        .map(|t| t.clone().to_vec())
                        .any(|t| t[0] == "p" && t.get(1) == Some(&me));
                    if mentions_me {
                        self.sessions
                            .entry(thread_of(&event))
                            .or_default()
                            .push(event.content.clone());
                        self.record_requester(&event.id.to_hex());
                        return *event;
                    }
                }
                Ok(_) => {}
                Err(e) => panic!("no message mentioned the agent: {e}"),
            }
        }
    }

    /// The messages the session of the thread rooted at `root` has been told, oldest first.
    pub fn session(&self, root: &str) -> &[String] {
        self.sessions.get(root).map_or(&[], Vec::as_slice)
    }

    /// How many sessions the agent has opened.
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Plays the steps in order and returns the reply of each call. A mention is not a reply,
    /// so it is not in the list.
    pub async fn play(&mut self, steps: Vec<Step>) -> (Vec<Reply>, Vec<nostr::Event>) {
        let (mut replies, mut mentions) = (Vec::new(), Vec::new());
        for step in steps {
            match step {
                Step::Call(tool, arguments) => replies.push(self.call(tool, arguments).await),
                Step::AwaitMention => mentions.push(self.await_mention().await),
            }
        }
        (replies, mentions)
    }

    pub async fn finish(mut self) {
        drop(self.stdin);
        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
        let _ = self.relay.disconnect().await;
    }
}
