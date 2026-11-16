//! The socket and the real binary, the way the engine uses them.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use buzz_hive_core::supervisor_wire::{Call, Reply};
use buzz_hive_supervisor::server;
use common::{base_env, servers, Rig, HIVE};

fn exchange(socket: &std::path::Path, line: &str) -> Reply {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    stream.write_all(line.as_bytes()).unwrap();
    stream.write_all(b"\n").unwrap();
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).unwrap();
    serde_json::from_str(&reply).unwrap()
}

/// Sends `bytes` as they are, optionally ends the sending side, and reads until the server
/// closes. `None` when no reply came, for a connection the server dropped. Transport errors are
/// not unwrapped: what the server said is what is asserted.
fn send_raw(socket: &std::path::Path, bytes: &[u8], shut_write: bool) -> Option<Reply> {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let _ = stream.write_all(bytes);
    if shut_write {
        let _ = stream.shutdown(std::net::Shutdown::Write);
    }
    let mut reply = String::new();
    let _ = BufReader::new(stream).read_line(&mut reply);
    serde_json::from_str(&reply).ok()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[tokio::test]
async fn the_socket_is_mode_0600_answers_one_line_and_refuses_a_line_that_is_too_long() {
    let rig = Rig::new();
    let service =
        std::sync::Arc::new(buzz_hive_supervisor::calls::Service::new(rig.config.clone()).unwrap());
    let listener = server::bind(&service).unwrap();
    let mode = std::fs::metadata(&rig.config.socket)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    tokio::spawn(server::serve(service, listener));
    let socket = rig.config.socket.clone();
    let signer = rig.signer;
    let reply = tokio::task::spawn_blocking(move || {
        let ok = exchange(
            &socket,
            &signer.sign(
                &Call::AgentSettings {
                    agent: "coder".into(),
                },
                now_ms(),
            ),
        );
        let unsigned = exchange(
            &socket,
            r#"{"method":"AgentSettings","body":"{}","timestamp":1}"#,
        );
        // Exactly one byte past the limit and no newline: the server reads all of it before it
        // answers, so no unread tail turns the close into a reset.
        let huge = send_raw(
            &socket,
            "x".repeat(buzz_hive_core::supervisor_wire::MAX_LINE_BYTES + 1)
                .as_bytes(),
            false,
        );
        (
            ok,
            unsigned,
            huge.expect("the server answers an oversize line"),
        )
    })
    .await
    .unwrap();
    assert!(reply.0.ok, "{:?}", reply.0);
    assert_eq!(reply.1.error.as_deref(), Some("unauthenticated"));
    assert_eq!(reply.2.error.as_deref(), Some("unauthenticated"));
}

#[tokio::test]
async fn a_path_that_is_not_a_socket_is_not_replaced_and_a_stale_socket_is() {
    let rig = Rig::new();
    let service = buzz_hive_supervisor::calls::Service::new(rig.config.clone()).unwrap();
    std::fs::write(&rig.config.socket, "precious").unwrap();
    assert!(server::bind(&service).is_err());
    assert_eq!(
        std::fs::read_to_string(&rig.config.socket).unwrap(),
        "precious"
    );
    std::fs::remove_file(&rig.config.socket).unwrap();
    drop(server::bind(&service).unwrap());
    // The socket file stays after the listener is gone, as it does after a crash.
    assert!(server::bind(&service).is_ok());
}

struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The real binary against temporary folders and a stub restart script: one signed
/// ApplyAgentConfig applies and restarts, the same call again changes nothing, and a changed
/// list restarts once more.
#[test]
fn the_binary_applies_a_signed_call_and_restarts_only_when_the_output_changed() {
    let rig = Rig::new();
    let socket = rig.config.socket.clone();
    let child = Command::new(env!("CARGO_BIN_EXE_buzz-hive-supervisor"))
        .env("BUZZ_HIVE_SUPERVISOR_SOCKET", &socket)
        .env("BUZZ_HIVE_SUPERVISOR_PUBKEY", &rig.config.engine_key)
        .env("BUZZ_HIVE_SUPERVISOR_HIVE_PUBKEY", HIVE)
        .env("BUZZ_HIVE_SUPERVISOR_AGENTS_SRC", &rig.config.agents_src)
        .env("BUZZ_HIVE_SUPERVISOR_RENDER_DIR", &rig.config.render_dir)
        .env(
            "BUZZ_HIVE_SUPERVISOR_RESTART_CMD",
            rig.root().join("restart.sh"),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _running = Running(child);
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(socket.exists(), "the supervisor did not open its socket");
    assert_eq!(
        std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let apply = |servers| {
        let call = Call::ApplyAgentConfig {
            agent: "coder".into(),
            servers,
            secrets: Default::default(),
        };
        exchange(&socket, &rig.signer.sign(&call, now_ms()))
    };
    let first = apply(servers());
    assert_eq!(
        first.result,
        Some(serde_json::json!({"restarted": true})),
        "{first:?}"
    );
    assert_eq!(rig.restarts(), ["coder"]);
    let env = std::fs::read_to_string(rig.render("coder.env")).unwrap();
    assert!(env.contains("BUZZ_ACP_SESSION_POLICY=thread\n"));
    assert_ne!(env, base_env());

    let again = apply(servers());
    assert_eq!(
        again.result,
        Some(serde_json::json!({"restarted": false})),
        "{again:?}"
    );
    assert_eq!(rig.restarts().len(), 1);

    let mut fewer = servers();
    fewer.pop();
    let changed = apply(fewer);
    assert_eq!(
        changed.result,
        Some(serde_json::json!({"restarted": true})),
        "{changed:?}"
    );
    assert_eq!(rig.restarts(), ["coder", "coder"]);

    let missing = exchange(
        &socket,
        &rig.signer.sign(
            &Call::RestartAgent {
                agent: "stranger".into(),
            },
            now_ms(),
        ),
    );
    assert_eq!(missing.error.as_deref(), Some("not_found"));
}

/// A running socket on a rig, answering until the test ends.
async fn running(
    rig: &Rig,
    shorten: impl FnOnce(buzz_hive_supervisor::calls::Service) -> buzz_hive_supervisor::calls::Service,
) -> (
    std::sync::Arc<buzz_hive_supervisor::calls::Service>,
    std::path::PathBuf,
) {
    let service = std::sync::Arc::new(shorten(
        buzz_hive_supervisor::calls::Service::new(rig.config.clone()).unwrap(),
    ));
    let listener = server::bind(&service).unwrap();
    tokio::spawn(server::serve(service.clone(), listener));
    (service, rig.config.socket.clone())
}

fn settings_line(rig: &Rig) -> String {
    rig.signer.sign(
        &Call::AgentSettings {
            agent: "coder".into(),
        },
        now_ms(),
    )
}

#[tokio::test]
async fn a_line_that_is_cut_short_or_never_sent_is_refused_and_the_server_keeps_answering() {
    let rig = Rig::new();
    let (_service, socket) =
        running(&rig, |s| s.with_read_timeout(Duration::from_millis(300))).await;
    let line = settings_line(&rig);
    let signed = line.clone();
    let socket2 = socket.clone();
    let (cut, nothing, stalled_then_ok, empty) = tokio::task::spawn_blocking(move || {
        // Bytes with no newline, then the end of the sending side.
        let cut = send_raw(&socket2, line.as_bytes(), true);
        // A client that connects, sends nothing and closes.
        let empty = send_raw(&socket2, b"", true);
        // A client that connects and sends nothing is let go when its time is up, and does
        // not hold up the next one.
        let stalled = UnixStream::connect(&socket2).unwrap();
        let ok = exchange(&socket2, &signed);
        let mut stalled_reply = String::new();
        stalled
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        let _ = BufReader::new(stalled).read_line(&mut stalled_reply);
        (cut, stalled_reply, ok, empty)
    })
    .await
    .unwrap();
    let cut = cut.expect("a cut line is answered");
    assert_eq!(cut.error.as_deref(), Some("unauthenticated"), "{cut:?}");
    assert!(
        cut.message.as_deref().unwrap().contains("complete line"),
        "{cut:?}"
    );
    assert!(empty.is_some_and(|r| r.error.as_deref() == Some("unauthenticated")));
    assert!(stalled_then_ok.ok, "{stalled_then_ok:?}");
    assert!(nothing.contains("complete line"), "{nothing}");
}

#[tokio::test]
async fn a_line_of_exactly_the_limit_is_read_and_one_byte_more_is_not() {
    let rig = Rig::new();
    let (_service, socket) = running(&rig, |s| s).await;
    let max = buzz_hive_core::supervisor_wire::MAX_LINE_BYTES;
    let (at_limit, over) = tokio::task::spawn_blocking(move || {
        // The newline is the last of MAX_LINE_BYTES: the line is read and then judged, which
        // is not the same refusal as a line that is cut off.
        let mut at = "x".repeat(max - 1).into_bytes();
        at.push(b'\n');
        let at_limit = send_raw(&socket, &at, false);
        let mut more = "x".repeat(max).into_bytes();
        more.push(b'\n');
        let over = send_raw(&socket, &more, false);
        (at_limit, over)
    })
    .await
    .unwrap();
    let at_limit = at_limit.expect("answered");
    assert_eq!(at_limit.error.as_deref(), Some("unauthenticated"));
    assert!(
        !at_limit
            .message
            .as_deref()
            .unwrap()
            .contains("complete line"),
        "a line that fits reaches the check of the call: {at_limit:?}"
    );
    // One byte more: refused as not a complete line, or the connection is reset first.
    if let Some(over) = over {
        assert!(
            over.message.as_deref().unwrap().contains("complete line"),
            "{over:?}"
        );
    }
}

#[tokio::test]
async fn the_socket_can_be_given_to_the_user_the_engine_runs_as() {
    use std::os::unix::fs::MetadataExt;
    let rig = Rig::new();
    let me = std::fs::metadata(rig.root()).unwrap().uid();
    let mut config = rig.config.clone();
    config.socket_uid = Some(me);
    let service = buzz_hive_supervisor::calls::Service::new(config).unwrap();
    drop(server::bind(&service).unwrap());
    assert_eq!(std::fs::metadata(&rig.config.socket).unwrap().uid(), me);

    // A user that is not the caller's cannot be given a socket by a caller that is not root.
    if me != 0 {
        let mut config = rig.config.clone();
        config.socket_uid = Some(me + 1);
        let service = buzz_hive_supervisor::calls::Service::new(config).unwrap();
        let error = server::bind(&service).unwrap_err();
        assert!(error.contains("chown"), "{error}");
    }
}

#[tokio::test]
async fn a_call_whose_client_has_gone_is_dropped_before_it_takes_its_turn() {
    let rig = Rig::new();
    let (service, socket) = running(&rig, |s| s).await;
    let apply = |servers, offset: i64| {
        rig.signer.sign(
            &Call::ApplyAgentConfig {
                agent: "coder".into(),
                servers,
                secrets: Default::default(),
            },
            now_ms() + offset,
        )
    };
    // The first apply is in its restart, which is held, so it has the turn.
    rig.hold_restarts(true);
    let first_line = apply(servers(), 0);
    let first = {
        let service = service.clone();
        let at = now_ms();
        tokio::spawn(async move { service.handle(&first_line, at).await })
    };
    while !rig.restart_started() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // The second arrives over the socket, waits for its turn, and its client goes away. The
    // server closes the connection without a word, which is what the read below waits for.
    let mut fewer = servers();
    fewer.pop();
    let second_line = apply(fewer, 1000);
    let socket2 = socket.clone();
    let answer = tokio::task::spawn_blocking(move || {
        send_raw(&socket2, format!("{second_line}\n").as_bytes(), true)
    })
    .await
    .unwrap();
    assert!(
        answer.is_none(),
        "a dropped call is not answered: {answer:?}"
    );
    rig.hold_restarts(false);
    let first = first.await.unwrap();
    assert!(first.ok, "{first:?}");
    // Only the first apply ran: its list is the one on disk, and the agent restarted once.
    common::golden(
        "apply/mcp.json",
        &std::fs::read_to_string(rig.render("coder/config/mcp.json")).unwrap(),
    );
    assert_eq!(rig.restarts(), ["coder"]);
}
