//! Signing an agent in to an OAuth tool server: the three calls, with a stub CLI that is run
//! the way the supervisor runs the real one, through the exec setting.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use buzz_hive_core::domain::agent_login::CallbackUrl;
use buzz_hive_core::supervisor_wire::{Call, Reply, Signer};
use buzz_hive_supervisor::login::Limits;
use common::{servers, Rig, NOW};
use serde_json::json;

const CODE: &str = "TOPSECRETCODE";
const ADDRESS: &str = "http://localhost:45219/callback?code=TOPSECRETCODE&state=STUBSTATE";
const LINK: &str = "https://auth.example.org/oauth/authorize?client_id=stub&state=STUBSTATE&redirect_uri=http%3A%2F%2Flocalhost%3A45219%2Fcallback";

fn start(agent: &str, server: &str) -> Call {
    Call::LoginStart {
        agent: agent.into(),
        server: server.into(),
    }
}

fn status(agent: &str, server: &str) -> Call {
    Call::LoginStatus {
        agent: agent.into(),
        server: server.into(),
    }
}

fn paste(agent: &str, server: &str, address: &str) -> Call {
    Call::LoginPaste {
        agent: agent.into(),
        server: server.into(),
        url: CallbackUrl::parse(address).unwrap(),
    }
}

/// A rig whose `coder` has the `docs` HTTP server and the `filesystem` stdio one applied.
async fn rig_with(limits: Limits) -> Rig {
    let rig = Rig::with_limits(limits);
    assert!(rig.apply("coder", servers()).await.ok);
    rig
}

fn limits(f: impl FnOnce(&mut Limits)) -> Limits {
    let mut limits = Limits::default();
    f(&mut limits);
    limits
}

fn pid(rig: &Rig) -> String {
    std::fs::read_to_string(rig.stub("login.pid"))
        .unwrap()
        .trim()
        .to_owned()
}

fn alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// Waits for a process to be gone. `end` waits for the child, so this is quick.
async fn gone(pid: &str) -> bool {
    for _ in 0..100 {
        if !alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test]
async fn the_link_comes_back_the_paste_reaches_the_process_and_the_status_flips() {
    let rig = rig_with(Limits::default()).await;

    let before = rig.call(&status("coder", "docs")).await;
    assert_eq!(
        before.result,
        Some(json!({"state": "needs-sign-in"})),
        "{before:?}"
    );

    let started = rig.call(&start("coder", "docs")).await;
    assert_eq!(started.result, Some(json!({"url": LINK})), "{started:?}");
    assert_eq!(
        rig.stub_calls(),
        ["coder mcp list", "coder mcp login docs --no-browser"]
    );
    // Still waiting: nothing is signed in yet.
    let waiting = rig.call(&status("coder", "docs")).await;
    assert_eq!(waiting.result, Some(json!({"state": "needs-sign-in"})));

    let pasted = rig.call(&paste("coder", "docs", ADDRESS)).await;
    assert_eq!(
        pasted.result,
        Some(json!({"signed_in": true})),
        "{pasted:?}"
    );
    assert_eq!(
        std::fs::read_to_string(rig.stub("last-paste")).unwrap(),
        format!("{ADDRESS}\n"),
        "the process was handed exactly the pasted address"
    );
    let after = rig.call(&status("coder", "docs")).await;
    assert_eq!(
        after.result,
        Some(json!({"state": "connected"})),
        "{after:?}"
    );
    assert!(gone(&pid(&rig)).await, "the process ended");

    // The session was used up.
    let again = rig.call(&paste("coder", "docs", ADDRESS)).await;
    assert_eq!(again.error.as_deref(), Some("not_found"), "{again:?}");
}

#[tokio::test]
async fn a_paste_with_the_wrong_shape_is_refused_before_the_process_and_keeps_the_session() {
    let rig = rig_with(Limits::default()).await;
    assert!(rig.call(&start("coder", "docs")).await.ok);
    for url in [
        format!("https://localhost:45219/callback?code={CODE}"),
        format!("http://evil.example.org:45219/callback?code={CODE}"),
        format!("http://localhost:80@evil.example.org/x?code={CODE}"),
        format!("http://localhost:45219/callback?state=s&token={CODE}"),
        format!("http://localhost:45219/callback?code={CODE}\nhttp://localhost:1/x?code=2"),
        format!("the code is {CODE}"),
    ] {
        let body = json!({"agent": "coder", "server": "docs", "url": url}).to_string();
        let reply = rig.raw("LoginPaste", &body).await;
        assert_eq!(
            reply.error.as_deref(),
            Some("bad_request"),
            "{url}: {reply:?}"
        );
        assert!(!format!("{reply:?}").contains(CODE), "{reply:?}");
    }
    // A url of the wrong type is refused too, and not echoed.
    let reply = rig
        .raw(
            "LoginPaste",
            r#"{"agent":"coder","server":"docs","url":987654321}"#,
        )
        .await;
    assert_eq!(reply.error.as_deref(), Some("bad_request"));
    assert!(!format!("{reply:?}").contains("987654321"), "{reply:?}");
    assert!(
        !rig.stub("last-paste").exists(),
        "nothing reached the process"
    );
    // The session is still waiting, so the right address still works.
    let ok = rig.call(&paste("coder", "docs", ADDRESS)).await;
    assert!(ok.ok, "{ok:?}");
}

#[tokio::test]
async fn a_session_older_than_its_time_is_dropped_and_its_process_ended() {
    let rig = rig_with(limits(|l| l.ttl = Duration::from_millis(300))).await;
    assert!(rig.call(&start("coder", "docs")).await.ok);
    let waiting = pid(&rig);
    assert!(alive(&waiting));
    // Nothing calls the supervisor from here until the process is gone: it is the expiry
    // timer that ends it, not a later call sweeping the table.
    assert!(gone(&waiting).await, "the process was ended by the timer");
    let reply = rig.call(&paste("coder", "docs", ADDRESS)).await;
    assert_eq!(reply.error.as_deref(), Some("not_found"), "{reply:?}");
    assert!(
        !rig.stub("last-paste").exists(),
        "an expired session is never written to"
    );
}

#[tokio::test]
async fn a_second_start_replaces_the_session_and_ends_the_first_process() {
    let rig = rig_with(Limits::default()).await;
    assert!(rig.call(&start("coder", "docs")).await.ok);
    let first = pid(&rig);
    assert!(rig.call(&start("coder", "docs")).await.ok);
    let second = pid(&rig);
    assert_ne!(first, second);
    assert!(gone(&first).await);
    assert!(alive(&second));
    assert!(rig.call(&paste("coder", "docs", ADDRESS)).await.ok);
}

#[tokio::test]
async fn two_starts_at_once_leave_one_sign_in_and_the_other_process_ended() {
    let rig = rig_with(Limits::default()).await;
    let call = start("coder", "docs");
    let (a, b) = tokio::join!(rig.call(&call), rig.call(&call));
    assert!(a.ok && b.ok, "{a:?} {b:?}");
    let pids: Vec<String> = std::fs::read_to_string(rig.stub("login.pids"))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(pids.len(), 2, "{pids:?}");
    for pid in &pids[..1] {
        assert!(gone(pid).await, "the sign-in that was replaced is ended");
    }
    assert!(alive(&pids[1]), "the last start is the one that is waiting");
    // The address goes to the process that is waiting and the sign-in finishes.
    assert!(rig.call(&paste("coder", "docs", ADDRESS)).await.ok);
}

#[tokio::test]
async fn a_restart_ends_the_sign_ins_waiting_in_that_agents_container() {
    let rig = rig_with(Limits::default()).await;
    assert!(rig.call(&start("coder", "docs")).await.ok);
    let waiting = pid(&rig);
    assert!(alive(&waiting));
    assert!(
        rig.call(&Call::RestartAgent {
            agent: "coder".into()
        })
        .await
        .ok
    );
    assert!(
        gone(&waiting).await,
        "the sign-in did not outlive its container"
    );
    let reply = rig.call(&paste("coder", "docs", ADDRESS)).await;
    assert_eq!(reply.error.as_deref(), Some("not_found"), "{reply:?}");
}

#[tokio::test]
async fn unsigned_stale_forged_and_unknown_calls_are_refused_and_run_nothing() {
    let rig = rig_with(Limits::default()).await;
    let line = rig.signer.sign(&start("coder", "docs"), NOW);

    let mut unsigned: serde_json::Value = serde_json::from_str(&line).unwrap();
    unsigned.as_object_mut().unwrap().remove("signature");
    let reply = rig.service.handle(&unsigned.to_string(), NOW).await;
    assert_eq!(reply.error.as_deref(), Some("unauthenticated"));

    let other = Signer::from_secret(b"another secret entirely").unwrap();
    let forged = rig
        .service
        .handle(&other.sign(&start("coder", "docs"), NOW), NOW)
        .await;
    assert_eq!(forged.error.as_deref(), Some("unauthenticated"));

    let stale = rig.service.handle(&line, NOW + 120_000).await;
    assert_eq!(stale.error.as_deref(), Some("stale"));

    let unknown = rig
        .raw("LoginRun", r#"{"agent":"coder","server":"docs"}"#)
        .await;
    assert_eq!(unknown.error.as_deref(), Some("unknown_method"));

    // An agent with no folder, a name that is a path, an agent not rendered yet, a server the
    // agent does not have and a stdio server (no OAuth) are all not found.
    for call in [
        start("stranger", "docs"),
        start("../etc", "docs"),
        status("researcher", "docs"),
        start("coder", "ghost"),
        paste("coder", "ghost", ADDRESS),
        status("coder", "filesystem"),
    ] {
        let reply = rig.call(&call).await;
        assert_eq!(
            reply.error.as_deref(),
            Some("not_found"),
            "{call:?}: {reply:?}"
        );
    }
    // A server name that is a flag never reaches the wire.
    let flag = rig
        .raw("LoginStart", r#"{"agent":"coder","server":"--help"}"#)
        .await;
    assert_eq!(flag.error.as_deref(), Some("bad_request"));
    // A paste with nothing waiting.
    let none = rig.call(&paste("coder", "docs", ADDRESS)).await;
    assert_eq!(none.error.as_deref(), Some("not_found"));

    assert!(rig.stub_calls().is_empty(), "{:?}", rig.stub_calls());
}

#[tokio::test]
async fn a_cli_that_does_not_accept_the_address_is_rejected_and_the_session_is_used_up() {
    let rig = rig_with(Limits::default()).await;
    // The shape is right but the state is not the one the CLI sent.
    assert!(rig.call(&start("coder", "docs")).await.ok);
    let wrong = "http://localhost:45219/callback?code=TOPSECRETCODE&state=WRONG";
    let reply = rig.call(&paste("coder", "docs", wrong)).await;
    assert_eq!(reply.error.as_deref(), Some("rejected"), "{reply:?}");
    assert!(!format!("{reply:?}").contains(CODE), "{reply:?}");
    let status_now = rig.call(&status("coder", "docs")).await;
    assert_eq!(status_now.result, Some(json!({"state": "needs-sign-in"})));
    let again = rig.call(&paste("coder", "docs", ADDRESS)).await;
    assert_eq!(again.error.as_deref(), Some("not_found"));

    // The CLI exits non-zero whatever it is given.
    rig.set_stub_mode("reject");
    assert!(rig.call(&start("coder", "docs")).await.ok);
    let reply = rig.call(&paste("coder", "docs", ADDRESS)).await;
    assert_eq!(reply.error.as_deref(), Some("rejected"), "{reply:?}");
}

#[tokio::test]
async fn a_process_that_never_prints_a_link_or_never_exits_fails_and_is_ended() {
    let rig = rig_with(limits(|l| {
        l.start_wait = Duration::from_millis(400);
        l.paste_wait = Duration::from_millis(400);
    }))
    .await;

    rig.set_stub_mode("silent");
    let silent = rig.call(&start("coder", "docs")).await;
    assert_eq!(silent.error.as_deref(), Some("failed"), "{silent:?}");
    assert!(gone(&pid(&rig)).await);

    rig.set_stub_mode("hang");
    assert!(rig.call(&start("coder", "docs")).await.ok);
    let hung = pid(&rig);
    let reply = rig.call(&paste("coder", "docs", ADDRESS)).await;
    assert_eq!(reply.error.as_deref(), Some("failed"), "{reply:?}");
    assert!(!format!("{reply:?}").contains(CODE));
    assert!(gone(&hung).await, "the hung process was ended");
}

#[tokio::test]
async fn a_link_that_is_not_a_sign_in_address_is_never_returned() {
    let rig = rig_with(limits(|l| l.start_wait = Duration::from_millis(600))).await;
    rig.set_stub_mode("bad-link");
    let reply = rig.call(&start("coder", "docs")).await;
    assert_eq!(reply.error.as_deref(), Some("failed"), "{reply:?}");
    let text = format!("{reply:?}");
    assert!(!text.contains("phish.example.net"), "{text}");
    assert!(gone(&pid(&rig)).await, "the process was ended");
    // Nothing is waiting, so a paste has nowhere to go.
    let again = rig.call(&paste("coder", "docs", ADDRESS)).await;
    assert_eq!(again.error.as_deref(), Some("not_found"), "{again:?}");
}

#[tokio::test]
async fn a_server_that_fails_to_connect_is_reported_and_not_called_signed_in() {
    let rig = rig_with(Limits::default()).await;
    rig.set_stub_mode("list-failed");
    let reply = rig.call(&status("coder", "docs")).await;
    assert_eq!(reply.error.as_deref(), Some("failed"), "{reply:?}");
}

/// A subscriber that keeps everything it is told, at every level.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Capture {
        self.clone()
    }
}

#[tokio::test]
async fn neither_the_link_nor_the_pasted_address_reaches_a_log_line_or_an_error() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let rig = rig_with(limits(|l| l.paste_wait = Duration::from_millis(400))).await;

    let mut replies: Vec<Reply> = Vec::new();
    // A good sign-in.
    replies.push(rig.call(&start("coder", "docs")).await);
    replies.push(
        rig.raw(
            "LoginPaste",
            &json!({"agent": "coder", "server": "docs", "url": format!("https://x.example.org/?code={CODE}")}).to_string(),
        )
        .await,
    );
    replies.push(rig.call(&paste("coder", "docs", ADDRESS)).await);
    // One the CLI rejects, which echoes the address on both of its streams.
    rig.set_stub_mode("reject");
    replies.push(rig.call(&start("coder", "docs")).await);
    replies.push(rig.call(&paste("coder", "docs", ADDRESS)).await);
    // One that hangs.
    rig.set_stub_mode("hang");
    replies.push(rig.call(&start("coder", "docs")).await);
    replies.push(rig.call(&paste("coder", "docs", ADDRESS)).await);
    // One with nothing waiting, and a status.
    replies.push(rig.call(&paste("coder", "docs", ADDRESS)).await);
    replies.push(rig.call(&status("coder", "docs")).await);

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("call done"),
        "the capture saw nothing: {logs}"
    );
    assert!(logs.contains("call rejected"), "{logs}");
    // The link is returned to the caller by LoginStart, once, and is nowhere else.
    for forbidden in [
        CODE,
        "STUBSTATE",
        "oauth/authorize",
        "localhost%3A45219",
        "received:",
    ] {
        assert!(
            !logs.contains(forbidden),
            "{forbidden} reached the log:\n{logs}"
        );
    }
    for (i, reply) in replies.iter().enumerate() {
        let text = format!("{reply:?}");
        assert!(
            !text.contains(CODE) && !text.contains("received:"),
            "{i}: {text}"
        );
        // Only a successful LoginStart carries the link.
        if !reply.ok {
            assert!(!text.contains("STUBSTATE"), "{i}: {text}");
        }
    }
}
