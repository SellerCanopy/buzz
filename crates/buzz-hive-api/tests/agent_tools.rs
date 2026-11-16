//! An agent's tool servers over the API: who may change them, that a change reaches the
//! supervisor, and that the health read names an agent the supervisor says is set wrong.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use buzz_hive_api::App;
use buzz_hive_core::orchestrator::agent_tools;
use buzz_hive_core::ports::supervisor::SupervisorError;
use buzz_hive_core::testing::{FakeSupervisor, MemStore};
use common::{app, call, call_unclaimed, community, AGENT, MEMBER, OWNER};
use serde_json::{json, Value};

const MCP: &str = r#"{"mcpServers":{"filesystem":{"command":"npx","args":["-y","fs"]}}}"#;

async fn world() -> (App, FakeSupervisor) {
    let fake = FakeSupervisor::new();
    fake.deploy("coder", "thread", true);
    fake.deploy("researcher", "thread", true);
    let app = app(MemStore::new()).await;
    agent_tools::seed(app.store.as_ref(), community(), "coder", Some(MCP))
        .await
        .unwrap();
    agent_tools::seed(app.store.as_ref(), community(), "researcher", None)
        .await
        .unwrap();
    let app = App {
        supervisor: Some(Arc::new(fake.clone())),
        ..app
    };
    (app, fake)
}

fn docs(token: &str) -> Value {
    json!({
        "name": "docs",
        "transport": "http",
        "specJson": json!({"url": "https://mcp.example.org/mcp", "headers": {"Authorization": token}}).to_string(),
    })
}

fn names(agent: &Value) -> Vec<&str> {
    agent["servers"]
        .as_array()
        .map(|servers| {
            servers
                .iter()
                .map(|s| s["name"].as_str().unwrap())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn a_person_lists_and_an_admin_sets_and_removes() {
    let (app, fake) = world().await;

    let (status, body) = call(&app, "ListAgentToolServers", MEMBER, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let agents = body["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 2, "{body}");
    assert_eq!(agents[0]["agent"], "coder");
    assert_eq!(names(&agents[0]), ["filesystem"]);
    assert_eq!(agents[1]["agent"], "researcher");
    assert!(names(&agents[1]).is_empty());

    let set = json!({"agent": "coder", "server": docs("Bearer ${DOCS_TOKEN}")});
    let (status, body) = call(&app, "SetAgentToolServer", MEMBER, set.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(fake.applied().is_empty());

    let (status, body) = call(&app, "SetAgentToolServer", OWNER, set.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["applied"], true, "{body}");
    assert_eq!(body["restarted"], true, "{body}");
    assert_eq!(names(&body["agent"]), ["docs", "filesystem"]);
    assert_eq!(
        fake.applied(),
        [(
            "coder".to_owned(),
            vec!["docs".to_owned(), "filesystem".to_owned()]
        )]
    );

    // The same call again changes nothing, so nothing restarts.
    let (_, again) = call(&app, "SetAgentToolServer", OWNER, set).await;
    assert_eq!(again["applied"], true, "{again}");
    assert!(
        again["restarted"].is_null() || again["restarted"] == false,
        "{again}"
    );

    let remove = json!({"agent": "coder", "name": "docs"});
    let (status, body) = call(&app, "RemoveAgentToolServer", MEMBER, remove.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = call(&app, "RemoveAgentToolServer", OWNER, remove.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(names(&body["agent"]), ["filesystem"]);
    let (status, body) = call(&app, "RemoveAgentToolServer", OWNER, remove).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn an_agent_may_read_but_never_change_tool_servers() {
    let (app, fake) = world().await;
    let (status, body) = call_unclaimed(&app, "ListAgentToolServers", AGENT, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The agent acts for an Admin and has Run itself: still refused.
    let set = json!({"agent": "coder", "server": docs("Bearer ${DOCS_TOKEN}")});
    let (status, body) = call(&app, "SetAgentToolServer", AGENT, set).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let remove = json!({"agent": "coder", "name": "filesystem"});
    let (status, body) = call(&app, "RemoveAgentToolServer", AGENT, remove).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(fake.applied().is_empty());
}

#[tokio::test]
async fn a_bad_server_or_an_unknown_agent_is_refused_and_nothing_is_saved() {
    let (app, fake) = world().await;
    // A secret value in a header, not a ${NAME} reference.
    let (status, body) = call(
        &app,
        "SetAgentToolServer",
        OWNER,
        json!({"agent": "coder", "server": docs("Bearer sk-live-1234")}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = call(
        &app,
        "SetAgentToolServer",
        OWNER,
        json!({"agent": "ghost", "server": docs("Bearer ${DOCS_TOKEN}")}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, body) = call(
        &app,
        "SetAgentToolServer",
        OWNER,
        json!({"agent": "../etc", "server": docs("Bearer ${DOCS_TOKEN}")}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = call(
        &app,
        "SetAgentToolServer",
        OWNER,
        json!({"agent": "coder", "server": {"name": "x", "transport": "websocket", "specJson": "{}"}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = call(&app, "SetAgentToolServer", OWNER, json!({"agent": "coder"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(fake.applied().is_empty());
    let (_, listed) = call(&app, "ListAgentToolServers", OWNER, json!({})).await;
    assert_eq!(names(&listed["agents"][0]), ["filesystem"]);
}

#[tokio::test]
async fn a_supervisor_that_is_down_leaves_the_change_saved_and_says_so() {
    let (app, fake) = world().await;
    fake.fail_next_apply(SupervisorError::Unavailable("socket closed".into()));
    let (status, body) = call(
        &app,
        "SetAgentToolServer",
        OWNER,
        json!({"agent": "coder", "server": docs("Bearer ${DOCS_TOKEN}")}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["applied"].is_null() || body["applied"] == false,
        "{body}"
    );
    assert!(
        body["applyError"]
            .as_str()
            .unwrap()
            .contains("socket closed"),
        "{body}"
    );
    assert_eq!(names(&body["agent"]), ["docs", "filesystem"]);

    // No supervisor at all is the same: saved, not applied.
    let bare = App {
        supervisor: None,
        ..app
    };
    let (status, body) = call(
        &bare,
        "RemoveAgentToolServer",
        OWNER,
        json!({"agent": "coder", "name": "docs"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["applyError"]
            .as_str()
            .unwrap()
            .contains("BUZZ_HIVE_SUPERVISOR_SOCKET"),
        "{body}"
    );
    assert_eq!(names(&body["agent"]), ["filesystem"]);
}

#[tokio::test]
async fn health_names_an_agent_on_a_channel_session_or_missing_the_allowlist_entry() {
    let (app, fake) = world().await;
    let (_, health) = call(&app, "GetHiveHealth", OWNER, json!({})).await;
    assert_eq!(health["allowlistApplied"], "yes", "{health}");
    assert!(health["agentIssues"].as_array().is_none_or(Vec::is_empty));

    fake.deploy("coder", "channel", true);
    fake.deploy("researcher", "thread", false);
    let (_, health) = call(&app, "GetHiveHealth", OWNER, json!({})).await;
    assert_eq!(health["allowlistApplied"], "no", "{health}");
    let issues = health["agentIssues"].as_array().unwrap();
    assert_eq!(issues.len(), 2, "{health}");
    assert_eq!(issues[0]["agent"], "coder");
    assert_eq!(issues[0]["sessionPolicy"], "channel");
    assert!(issues[0]["problem"].as_str().unwrap().contains("thread"));
    assert_eq!(issues[1]["agent"], "researcher");
    assert!(issues[1]["allowlisted"].is_null() || issues[1]["allowlisted"] == false);

    // Applying an agent puts it right.
    call(
        &app,
        "SetAgentToolServer",
        OWNER,
        json!({"agent": "coder", "server": docs("Bearer ${DOCS_TOKEN}")}),
    )
    .await;
    let (_, health) = call(&app, "GetHiveHealth", OWNER, json!({})).await;
    assert_eq!(
        health["agentIssues"].as_array().unwrap().len(),
        1,
        "{health}"
    );

    // With no supervisor the answer stays unknown, which is no fault of the agents.
    let bare = App {
        supervisor: None,
        ..app
    };
    let (_, health) = call(&bare, "GetHiveHealth", OWNER, json!({})).await;
    assert_eq!(health["allowlistApplied"], "unknown", "{health}");
}
