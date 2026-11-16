//! Signing an agent in to an OAuth tool server over the API: who may, that the link comes back,
//! that the pasted address reaches the supervisor and is echoed nowhere.

mod common;

use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use buzz_hive_api::App;
use buzz_hive_core::orchestrator::agent_tools;
use buzz_hive_core::testing::{FakeSupervisor, MemStore};
use common::{app, call, community, AGENT, MEMBER, OWNER};
use serde_json::json;

const CODE: &str = "TOPSECRETCODE";
const ADDRESS: &str = "http://localhost:45219/callback?code=TOPSECRETCODE&state=fake";

async fn world() -> (App, FakeSupervisor) {
    let fake = FakeSupervisor::new();
    fake.deploy("coder", "thread", true);
    let app = app(MemStore::new()).await;
    agent_tools::seed(app.store.as_ref(), community(), "coder", None)
        .await
        .unwrap();
    let app = App {
        supervisor: Some(Arc::new(fake.clone())),
        secrets: None,
        ..app
    };
    (app, fake)
}

fn target() -> serde_json::Value {
    json!({"agent": "coder", "server": "docs"})
}

fn with_url(url: &str) -> serde_json::Value {
    json!({"agent": "coder", "server": "docs", "url": url})
}

#[tokio::test]
async fn an_admin_gets_the_link_pastes_the_address_and_reads_the_state() {
    let (app, fake) = world().await;

    let (status, body) = call(&app, "GetAgentLoginStatus", OWNER, target()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "needs-sign-in", "{body}");
    assert_eq!(body["agent"], "coder");
    assert_eq!(body["server"], "docs");

    let (status, body) = call(&app, "StartAgentLogin", OWNER, target()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["url"], FakeSupervisor::link("coder", "docs"), "{body}");

    // The paste a person makes often has space around it.
    let (status, body) = call(
        &app,
        "PasteAgentLogin",
        OWNER,
        with_url(&format!("  {ADDRESS}\n")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["signedIn"], true, "{body}");
    assert_eq!(fake.pasted(), [ADDRESS]);

    let (_, body) = call(&app, "GetAgentLoginStatus", OWNER, target()).await;
    assert_eq!(body["state"], "connected", "{body}");

    // Nothing is waiting any more.
    let (status, body) = call(&app, "PasteAgentLogin", OWNER, with_url(ADDRESS)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(!body.to_string().contains(CODE), "{body}");
}

#[tokio::test]
async fn only_an_admin_signs_an_agent_in_and_an_agent_never_does() {
    let (app, fake) = world().await;
    for (method, body) in [
        ("StartAgentLogin", target()),
        ("PasteAgentLogin", with_url(ADDRESS)),
        ("GetAgentLoginStatus", target()),
    ] {
        let (status, reply) = call(&app, method, MEMBER, body.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method}: {reply}");
        // An agent acting for an Admin is refused as well.
        let (status, reply) = call(&app, method, AGENT, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method}: {reply}");
        assert!(!reply.to_string().contains(CODE), "{reply}");
    }
    assert!(fake.login_calls().is_empty(), "{:?}", fake.login_calls());
}

#[tokio::test]
async fn an_unknown_agent_a_bad_name_and_a_missing_supervisor_are_refused() {
    let (app, fake) = world().await;
    let (status, body) = call(
        &app,
        "StartAgentLogin",
        OWNER,
        json!({"agent": "ghost", "server": "docs"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, body) = call(
        &app,
        "StartAgentLogin",
        OWNER,
        json!({"agent": "coder", "server": "--help"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(fake.login_calls().is_empty());

    // No supervisor configured: the sign-in cannot run, which is a precondition, not a typo.
    let bare = App {
        supervisor: None,
        ..app
    };
    let (status, body) = call(&bare, "StartAgentLogin", OWNER, target()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[tokio::test]
async fn an_address_of_the_wrong_shape_is_refused_without_being_shown_or_sent() {
    let (app, fake) = world().await;
    for url in [
        format!("https://localhost:45219/callback?code={CODE}"),
        format!("http://evil.example.org:45219/callback?code={CODE}"),
        format!("http://localhost:45219/callback?state=x&token={CODE}"),
        format!("the code is {CODE}"),
    ] {
        let (status, body) = call(&app, "PasteAgentLogin", OWNER, with_url(&url)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{url}: {body}");
        assert!(!body.to_string().contains(CODE), "{body}");
    }
    // A body that does not decode is refused without echoing what was in it.
    let (status, body) = call(
        &app,
        "PasteAgentLogin",
        OWNER,
        json!({"agent": "coder", "server": "docs", "url": 987654321}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(!body.to_string().contains("987654321"), "{body}");
    assert!(fake.pasted().is_empty());
}

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
async fn neither_the_link_nor_the_address_reaches_a_log_line_at_any_level() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let (app, _fake) = world().await;

    call(&app, "StartAgentLogin", OWNER, target()).await;
    call(&app, "PasteAgentLogin", OWNER, with_url(ADDRESS)).await;
    call(&app, "PasteAgentLogin", OWNER, with_url(ADDRESS)).await;
    call(
        &app,
        "PasteAgentLogin",
        OWNER,
        with_url(&format!("https://evil.example.org/?code={CODE}")),
    )
    .await;
    call(
        &app,
        "PasteAgentLogin",
        OWNER,
        json!({"agent": "coder", "server": "docs", "url": 987654321}),
    )
    .await;
    call(&app, "PasteAgentLogin", MEMBER, with_url(ADDRESS)).await;
    call(&app, "GetAgentLoginStatus", OWNER, target()).await;

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("agent sign-in"),
        "the capture saw nothing: {logs}"
    );
    for forbidden in [CODE, "oauth", "auth.example.org", "state=fake", "987654321"] {
        assert!(
            !logs.contains(forbidden),
            "{forbidden} reached the log:\n{logs}"
        );
    }
}
