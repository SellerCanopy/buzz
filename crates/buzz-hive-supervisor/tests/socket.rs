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
        let huge = exchange(
            &socket,
            &"x".repeat(buzz_hive_core::supervisor_wire::MAX_LINE_BYTES + 10),
        );
        (ok, unsigned, huge)
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
