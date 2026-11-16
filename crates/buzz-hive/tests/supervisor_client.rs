//! The engine's supervisor client against a socket that checks the signature as the real
//! supervisor does: what it sends, and how each answer becomes an error.

use std::path::Path;
use std::sync::{Arc, Mutex};

use buzz_hive::supervisor_client::UnixSupervisor;
use buzz_hive_core::domain::tool_server::{ToolServer, Transport};
use buzz_hive_core::ports::supervisor::{Supervisor, SupervisorError};
use buzz_hive_core::supervisor_wire::{Call, Reply, Verifier};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";

/// A socket that opens each line with the engine's public key and answers with `answer`.
/// Returns the calls it opened.
fn serve(socket: &Path, answer: Reply) -> Arc<Mutex<Vec<Call>>> {
    let client = UnixSupervisor::new(socket.to_owned(), SECRET).unwrap();
    let verifier = Arc::new(Verifier::new(&client.public_hex()).unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let listener = UnixListener::bind(socket).unwrap();
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut line = String::new();
            BufReader::new(read).read_line(&mut line).await.unwrap();
            let now = chrono::Utc::now().timestamp_millis();
            let reply = match verifier.open(line.trim_end(), now) {
                Ok(call) => {
                    log.lock().unwrap().push(call);
                    answer.clone()
                }
                Err(refusal) => Reply::failed(refusal.code(), refusal),
            };
            let mut text = serde_json::to_string(&reply).unwrap();
            text.push('\n');
            write.write_all(text.as_bytes()).await.unwrap();
        }
    });
    seen
}

fn client(socket: &Path) -> UnixSupervisor {
    UnixSupervisor::new(socket.to_owned(), SECRET).unwrap()
}

fn fs() -> ToolServer {
    ToolServer::new("fs", Transport::Stdio, &json!({"command": "npx"})).unwrap()
}

#[tokio::test]
async fn an_apply_is_signed_with_the_secrets_key_and_returns_whether_the_agent_restarted() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.sock");
    let seen = serve(&socket, Reply::done(json!({"restarted": true})));
    let restarted = client(&socket).apply("coder", &[fs()]).await.unwrap();
    assert!(restarted);
    assert_eq!(
        *seen.lock().unwrap(),
        [Call::ApplyAgentConfig {
            agent: "coder".into(),
            servers: vec![fs()]
        }]
    );
}

#[tokio::test]
async fn settings_are_read_from_the_reply() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.sock");
    serve(
        &socket,
        Reply::done(json!({"session_policy": "channel", "allowlisted": false})),
    );
    let settings = client(&socket).settings("coder").await.unwrap();
    assert_eq!(settings.session_policy, "channel");
    assert!(!settings.allowlisted);
}

#[tokio::test]
async fn each_reply_code_becomes_its_error() {
    let dir = tempfile::tempdir().unwrap();
    for (code, check) in [
        (
            "not_found",
            (|e: &SupervisorError| matches!(e, SupervisorError::NotFound(_))) as fn(&_) -> bool,
        ),
        ("failed", |e| matches!(e, SupervisorError::Unavailable(_))),
        ("bad_request", |e| matches!(e, SupervisorError::Refused(_))),
        ("unauthenticated", |e| {
            matches!(e, SupervisorError::Refused(_))
        }),
    ] {
        let socket = dir.path().join(format!("{code}.sock"));
        serve(&socket, Reply::failed(code, "why"));
        let error = client(&socket).apply("coder", &[]).await.unwrap_err();
        assert!(check(&error), "{code} gave {error}");
        assert!(error.to_string().contains("why"), "{error}");
    }
}

#[tokio::test]
async fn a_supervisor_that_is_not_there_is_unavailable_and_a_wrong_key_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let missing = client(&dir.path().join("none.sock"));
    assert!(matches!(
        missing.settings("coder").await.unwrap_err(),
        SupervisorError::Unavailable(_)
    ));

    // A supervisor configured with another engine's key refuses the signature.
    let socket = dir.path().join("other.sock");
    serve(&socket, Reply::done(json!({"restarted": false})));
    let other = UnixSupervisor::new(socket, b"another secret of enough length!!").unwrap();
    assert!(matches!(
        other.apply("coder", &[]).await.unwrap_err(),
        SupervisorError::Refused(_)
    ));
}
