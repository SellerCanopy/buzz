//! Secrets over the API: who may set one, that a value is stored in the secret store and comes
//! back nowhere, and that the supervisor is told names only.

mod common;

use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use buzz_hive_api::App;
use buzz_hive_core::orchestrator::agent_tools;
use buzz_hive_core::ports::secrets::SecretScope;
use buzz_hive_core::testing::{FakeSecretSink, FakeSupervisor, MemStore};
use common::{app, call, community, AGENT, MEMBER, OWNER};
use serde_json::{json, Value};

const VALUE: &str = "ghp_SecretValue123";
const OWN_VALUE: &str = "docs.OwnSecret456";

async fn world() -> (App, FakeSupervisor, FakeSecretSink) {
    let fake = FakeSupervisor::new();
    fake.deploy("coder", "thread", true);
    fake.deploy("researcher", "thread", true);
    let sink = FakeSecretSink::new();
    let app = app(MemStore::new()).await;
    for agent in ["coder", "researcher"] {
        agent_tools::seed(app.store.as_ref(), community(), agent, None)
            .await
            .unwrap();
    }
    let app = App {
        supervisor: Some(Arc::new(fake.clone())),
        secrets: Some(Arc::new(sink.clone())),
        ..app
    };
    (app, fake, sink)
}

fn team(name: &str, value: &str) -> Value {
    json!({"name": name, "value": value})
}

#[tokio::test]
async fn an_admin_sets_grants_lists_and_revokes_and_no_response_holds_a_value() {
    let (app, fake, sink) = world().await;
    let mut seen = Vec::new();

    let (status, body) = call(&app, "SetTeamSecret", OWNER, team("GH_TOKEN", VALUE)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Nobody holds it yet, so nothing was applied.
    assert_eq!(body["applied"].as_array().map_or(0, Vec::len), 0, "{body}");
    assert_eq!(
        sink.value(&SecretScope::Team, "GH_TOKEN").as_deref(),
        Some(VALUE)
    );
    seen.push(body);

    let (status, body) = call(
        &app,
        "GrantTeamSecret",
        OWNER,
        json!({"agent": "coder", "name": "GH_TOKEN"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["applied"][0]["agent"], "coder");
    assert_eq!(body["applied"][0]["applied"], true);
    assert_eq!(
        fake.secrets("coder").map(|n| n.team),
        Some(vec!["GH_TOKEN".to_owned()])
    );
    // The researcher was not granted it and has no team secret.
    assert!(fake.secrets("researcher").is_none_or(|n| n.team.is_empty()));
    seen.push(body);

    let (status, body) = call(
        &app,
        "SetAgentSecret",
        OWNER,
        json!({"agent": "coder", "name": "DOCS_TOKEN", "value": OWN_VALUE}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        sink.value(&SecretScope::Agent("coder".into()), "DOCS_TOKEN")
            .as_deref(),
        Some(OWN_VALUE)
    );
    seen.push(body);

    // Replacing the team secret applies the agents that hold it.
    let (status, body) = call(
        &app,
        "SetTeamSecret",
        OWNER,
        team("GH_TOKEN", "ghp_Rotated"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["applied"][0]["agent"], "coder");
    seen.push(body);

    let (status, listed) = call(&app, "ListSecretNames", OWNER, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed["team"][0]["name"], "GH_TOKEN");
    assert_eq!(listed["team"][0]["updatedBy"], OWNER);
    assert_eq!(listed["agents"][0]["agent"], "coder");
    assert_eq!(listed["agents"][0]["secret"]["name"], "DOCS_TOKEN");
    assert_eq!(
        listed["grants"][0],
        json!({"agent": "coder", "name": "GH_TOKEN"})
    );
    seen.push(listed);

    let (status, body) = call(
        &app,
        "RevokeTeamSecret",
        OWNER,
        json!({"agent": "coder", "name": "GH_TOKEN"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(fake.secrets("coder").unwrap().team.is_empty());
    seen.push(body);
    let (status, body) = call(
        &app,
        "RevokeTeamSecret",
        OWNER,
        json!({"agent": "coder", "name": "GH_TOKEN"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    seen.push(body);

    // No value in any response, and the supervisor was told names only.
    let all = format!("{seen:?}{:?}", fake.applied());
    for value in [VALUE, OWN_VALUE, "ghp_Rotated"] {
        assert!(!all.contains(value), "{value} leaked: {all}");
    }
    // The events name the secret and nothing more.
    let (_, events) = call(&app, "GetRunEvents", OWNER, json!({})).await;
    assert!(!events.to_string().contains(VALUE));
}

#[tokio::test]
async fn only_an_admin_person_may_touch_a_secret() {
    let (app, fake, sink) = world().await;
    for (method, body) in [
        ("ListSecretNames", json!({})),
        ("SetTeamSecret", team("GH_TOKEN", VALUE)),
        (
            "SetAgentSecret",
            json!({"agent": "coder", "name": "X_TOKEN", "value": VALUE}),
        ),
        (
            "GrantTeamSecret",
            json!({"agent": "coder", "name": "GH_TOKEN"}),
        ),
        (
            "RevokeTeamSecret",
            json!({"agent": "coder", "name": "GH_TOKEN"}),
        ),
    ] {
        // A member below Admin.
        let (status, response) = call(&app, method, MEMBER, body.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method}: {response}");
        // An agent, even one acting for an Admin.
        let (status, response) = call(&app, method, AGENT, body).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{method} as an agent: {response}"
        );
    }
    assert!(sink.paths().is_empty());
    assert!(fake.applied().is_empty());
}

#[tokio::test]
async fn a_bad_name_value_or_agent_is_refused_without_repeating_the_value() {
    let (app, fake, sink) = world().await;
    let bad = "has space and $VAR";
    for (method, body, want) in [
        (
            "SetTeamSecret",
            team("BUZZ_PRIVATE_KEY", VALUE),
            StatusCode::BAD_REQUEST,
        ),
        (
            "SetTeamSecret",
            team("lower", VALUE),
            StatusCode::BAD_REQUEST,
        ),
        (
            "SetTeamSecret",
            team("GH_TOKEN", bad),
            StatusCode::BAD_REQUEST,
        ),
        (
            "SetTeamSecret",
            team("GH_TOKEN", ""),
            StatusCode::BAD_REQUEST,
        ),
        (
            "SetAgentSecret",
            json!({"agent": "stranger", "name": "X_TOKEN", "value": VALUE}),
            StatusCode::NOT_FOUND,
        ),
        (
            "GrantTeamSecret",
            json!({"agent": "coder", "name": "NOT_SET"}),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (status, response) = call(&app, method, OWNER, body).await;
        assert_eq!(status, want, "{method}: {response}");
        let text = response.to_string();
        assert!(!text.contains(VALUE) && !text.contains(bad), "{text}");
    }
    // A value of the wrong type is a bad request too, and the refusal does not echo it.
    let (status, response) = call(
        &app,
        "SetTeamSecret",
        OWNER,
        json!({"name": "GH_TOKEN", "value": 987654321}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
    assert!(!response.to_string().contains("987654321"), "{response}");
    assert!(sink.paths().is_empty(), "{:?}", sink.paths());
    assert!(fake.applied().is_empty());
}

#[tokio::test]
async fn with_no_secret_store_nothing_is_saved() {
    let (app, _fake, _sink) = world().await;
    let app = App {
        secrets: None,
        ..app
    };
    let (status, body) = call(&app, "SetTeamSecret", OWNER, team("GH_TOKEN", VALUE)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.to_string().contains("BUZZ_HIVE_SECRETS_DIR"));
    let (_, listed) = call(&app, "ListSecretNames", OWNER, json!({})).await;
    assert_eq!(listed["team"].as_array().map_or(0, Vec::len), 0);
}

#[tokio::test]
async fn a_supervisor_that_is_down_leaves_the_change_saved_and_says_so() {
    let (app, _fake, sink) = world().await;
    let app = App {
        supervisor: None,
        ..app
    };
    call(&app, "SetTeamSecret", OWNER, team("GH_TOKEN", VALUE)).await;
    let (status, body) = call(
        &app,
        "GrantTeamSecret",
        OWNER,
        json!({"agent": "coder", "name": "GH_TOKEN"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The protobuf JSON mapping leaves a false out.
    assert!(body["applied"][0].get("applied").is_none(), "{body}");
    assert!(!body["applied"][0]["applyError"]
        .as_str()
        .unwrap()
        .is_empty());
    assert_eq!(
        sink.value(&SecretScope::Team, "GH_TOKEN").as_deref(),
        Some(VALUE)
    );
    let (_, listed) = call(&app, "ListSecretNames", OWNER, json!({})).await;
    assert_eq!(listed["grants"][0]["name"], "GH_TOKEN");
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
async fn no_value_reaches_a_log_line_at_any_level() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let (app, _fake, _sink) = world().await;
    call(&app, "SetTeamSecret", OWNER, team("GH_TOKEN", VALUE)).await;
    call(
        &app,
        "GrantTeamSecret",
        OWNER,
        json!({"agent": "coder", "name": "GH_TOKEN"}),
    )
    .await;
    call(
        &app,
        "SetAgentSecret",
        OWNER,
        json!({"agent": "coder", "name": "DOCS_TOKEN", "value": OWN_VALUE}),
    )
    .await;
    call(&app, "SetTeamSecret", OWNER, team("GH_TOKEN", "has space")).await;
    call(
        &app,
        "SetTeamSecret",
        OWNER,
        json!({"name": "GH_TOKEN", "value": 987654321}),
    )
    .await;
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("secret changed"),
        "the capture saw nothing: {logs}"
    );
    for value in [VALUE, OWN_VALUE, "has space", "987654321"] {
        assert!(!logs.contains(value), "{value} reached the log:\n{logs}");
    }
}
