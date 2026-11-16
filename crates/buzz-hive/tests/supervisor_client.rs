//! The engine's supervisor client against a socket that checks the signature as the real
//! supervisor does: what it sends, and how each answer becomes an error.

use std::path::Path;
use std::sync::{Arc, Mutex};

use buzz_hive::supervisor_client::UnixSupervisor;
use buzz_hive_core::domain::agent_login::{CallbackUrl, LoginState};
use buzz_hive_core::domain::secret::SecretNames;
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
    let restarted = client(&socket)
        .apply("coder", &[fs()], &Default::default())
        .await
        .unwrap();
    assert!(restarted);
    assert_eq!(
        *seen.lock().unwrap(),
        [Call::ApplyAgentConfig {
            agent: "coder".into(),
            servers: vec![fs()],
            secrets: Default::default(),
        }]
    );
}

#[tokio::test]
async fn an_apply_carries_secret_names_and_nothing_else_about_them() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.sock");
    let seen = serve(&socket, Reply::done(json!({"restarted": false})));
    let names = SecretNames {
        own: vec!["DOCS_TOKEN".into()],
        team: vec!["GH_TOKEN".into()],
    };
    client(&socket).apply("coder", &[], &names).await.unwrap();
    let calls = seen.lock().unwrap();
    let Call::ApplyAgentConfig { secrets, .. } = &calls[0] else {
        panic!("{calls:?}")
    };
    assert_eq!(secrets, &names);
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
        let error = client(&socket)
            .apply("coder", &[], &Default::default())
            .await
            .unwrap_err();
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
        other
            .apply("coder", &[], &Default::default())
            .await
            .unwrap_err(),
        SupervisorError::Refused(_)
    ));
}

const ADDRESS: &str = "http://localhost:45219/callback?code=TOPSECRETCODE&state=s";

#[tokio::test]
async fn a_sign_in_start_returns_the_link_and_a_paste_and_a_status_send_what_they_were_given() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("login.sock");
    let seen = serve(
        &socket,
        Reply::done(json!({
            "url": "https://auth.example.org/authorize?state=s",
            "signed_in": true,
            "state": "connected",
        })),
    );
    let c = client(&socket);
    let link = c.login_start("coder", "docs").await.unwrap();
    assert_eq!(link.expose(), "https://auth.example.org/authorize?state=s");
    assert!(
        !format!("{link:?}").contains("auth.example.org"),
        "{link:?}"
    );

    let url = CallbackUrl::parse(ADDRESS).unwrap();
    c.login_paste("coder", "docs", &url).await.unwrap();
    assert_eq!(
        c.login_status("coder", "docs").await.unwrap(),
        LoginState::Connected
    );
    assert_eq!(
        *seen.lock().unwrap(),
        [
            Call::LoginStart {
                agent: "coder".into(),
                server: "docs".into()
            },
            Call::LoginPaste {
                agent: "coder".into(),
                server: "docs".into(),
                url
            },
            Call::LoginStatus {
                agent: "coder".into(),
                server: "docs".into()
            },
        ]
    );
}

#[tokio::test]
async fn sign_in_replies_that_are_not_what_was_asked_for_are_unavailable_and_a_rejection_is_refused(
) {
    let dir = tempfile::tempdir().unwrap();
    let url = CallbackUrl::parse(ADDRESS).unwrap();

    let socket = dir.path().join("empty.sock");
    serve(&socket, Reply::done(json!({})));
    let c = client(&socket);
    assert!(matches!(
        c.login_start("coder", "docs").await.unwrap_err(),
        SupervisorError::Unavailable(_)
    ));
    assert!(matches!(
        c.login_paste("coder", "docs", &url).await.unwrap_err(),
        SupervisorError::Unavailable(_)
    ));
    assert!(matches!(
        c.login_status("coder", "docs").await.unwrap_err(),
        SupervisorError::Unavailable(_)
    ));

    // The CLI would not take the address: a refusal of the paste, not a supervisor that is down.
    let socket = dir.path().join("rejected.sock");
    serve(
        &socket,
        Reply::failed("rejected", "the sign-in did not accept that address"),
    );
    let error = client(&socket)
        .login_paste("coder", "docs", &url)
        .await
        .unwrap_err();
    assert!(matches!(error, SupervisorError::Refused(_)), "{error}");
    assert!(!error.to_string().contains("TOPSECRETCODE"), "{error}");

    let socket = dir.path().join("nothing.sock");
    serve(&socket, Reply::failed("not_found", "no sign-in is waiting"));
    assert!(matches!(
        client(&socket)
            .login_paste("coder", "docs", &url)
            .await
            .unwrap_err(),
        SupervisorError::NotFound(_)
    ));
}

/// A socket that reads one line and then does `what` with the connection.
fn serve_raw(
    socket: &Path,
    what: fn(
        tokio::net::UnixStream,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
) {
    let listener = UnixListener::bind(socket).unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::spawn(what(stream));
        }
    });
}

fn quick(socket: &Path) -> UnixSupervisor {
    UnixSupervisor::new(socket.to_owned(), SECRET)
        .unwrap()
        .with_timeouts(buzz_hive::supervisor_client::Timeouts {
            apply: std::time::Duration::from_millis(300),
            read: std::time::Duration::from_millis(300),
            login: std::time::Duration::from_millis(300),
            login_status: std::time::Duration::from_millis(300),
        })
}

fn unavailable(error: SupervisorError) -> String {
    match error {
        SupervisorError::Unavailable(text) => text,
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

#[tokio::test]
async fn a_supervisor_that_never_answers_or_answers_something_else_is_unavailable_with_its_reason()
{
    let dir = tempfile::tempdir().unwrap();
    let url = CallbackUrl::parse(ADDRESS).unwrap();

    // Accepts, reads the line and says nothing: every kind of call gives up after its own time.
    let silent = dir.path().join("silent.sock");
    serve_raw(&silent, |stream| {
        Box::pin(async move {
            let (read, _write) = stream.into_split();
            let mut line = String::new();
            let _ = BufReader::new(read).read_line(&mut line).await;
            std::future::pending::<()>().await;
        })
    });
    let c = quick(&silent);
    for text in [
        unavailable(
            c.apply("coder", &[], &Default::default())
                .await
                .unwrap_err(),
        ),
        unavailable(c.settings("coder").await.unwrap_err()),
        unavailable(c.login_start("coder", "docs").await.unwrap_err()),
        unavailable(c.login_paste("coder", "docs", &url).await.unwrap_err()),
        unavailable(c.login_status("coder", "docs").await.unwrap_err()),
    ] {
        assert_eq!(text, "the supervisor did not answer in time");
    }

    // Closes without a word: the line is empty, which is not a reply.
    let closed = dir.path().join("closed.sock");
    serve_raw(&closed, |stream| {
        Box::pin(async move {
            let (read, _write) = stream.into_split();
            let mut line = String::new();
            let _ = BufReader::new(read).read_line(&mut line).await;
        })
    });
    assert_eq!(
        unavailable(
            quick(&closed)
                .apply("coder", &[], &Default::default())
                .await
                .unwrap_err()
        ),
        "the supervisor's answer is not a reply"
    );

    // Answers with something that is not JSON, or with a line longer than a reply may be.
    for (name, body) in [
        ("garbage.sock", b"not json at all\n".to_vec()),
        ("long.sock", vec![b'x'; (1 << 20) + 100]),
    ] {
        let socket = dir.path().join(name);
        let body: &'static [u8] = Box::leak(body.into_boxed_slice());
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let (read, mut write) = stream.into_split();
                    let mut line = String::new();
                    let _ = BufReader::new(read).read_line(&mut line).await;
                    let _ = write.write_all(body).await;
                });
            }
        });
        assert_eq!(
            unavailable(quick(&socket).settings("coder").await.unwrap_err()),
            "the supervisor's answer is not a reply",
            "{name}"
        );
    }
}

#[tokio::test]
async fn a_reply_that_leaves_out_what_the_call_needs_is_unavailable_and_says_which() {
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("empty.sock");
    serve(&empty, Reply::done(json!({})));
    let c = client(&empty);
    assert_eq!(
        unavailable(
            c.apply("coder", &[], &Default::default())
                .await
                .unwrap_err()
        ),
        "the reply has no restarted"
    );
    assert_eq!(
        unavailable(c.settings("coder").await.unwrap_err()),
        "the reply does not carry the agent's settings"
    );
    assert_eq!(
        unavailable(c.login_status("coder", "docs").await.unwrap_err()),
        "the reply has no sign-in state"
    );

    // Half of the settings is not the settings.
    for (name, body) in [
        ("policy.sock", json!({"session_policy": "thread"})),
        ("allow.sock", json!({"allowlisted": true})),
    ] {
        let socket = dir.path().join(name);
        serve(&socket, Reply::done(body));
        assert_eq!(
            unavailable(client(&socket).settings("coder").await.unwrap_err()),
            "the reply does not carry the agent's settings",
            "{name}"
        );
    }

    // A state the engine does not know is not guessed at.
    let odd = dir.path().join("odd.sock");
    serve(&odd, Reply::done(json!({"state": "half-signed-in"})));
    assert_eq!(
        unavailable(
            client(&odd)
                .login_status("coder", "docs")
                .await
                .unwrap_err()
        ),
        "the reply has no sign-in state"
    );
}
