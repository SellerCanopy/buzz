//! An agent never acts above the level of the person it acts for, on any tool.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use buzz_hive_api::tools::TOOLS;
use buzz_hive_api::App;
use buzz_hive_core::access::{ActorKind, Level, RelayRole};
use buzz_hive_core::testing::{FakeDirectory, MemStore};
use common::{app, call, community, AGENT, MEMBER, OWNER};

async fn set_agent_level(app: &App, level: Level) {
    let mut tx = app.store.begin(community()).await.unwrap();
    tx.access()
        .set_level(AGENT, level, ActorKind::Agent, OWNER)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

/// Calls every tool's route as the agent and checks which are refused for level. The body is
/// empty on purpose: the level is checked before the body is read, so a tool the agent may
/// call answers with something other than a refusal for level.
async fn assert_refused_exactly_above(app: &App, ceiling: Level) {
    for tool in TOOLS {
        let (status, body) = call(app, tool.route, AGENT, serde_json::json!({})).await;
        if tool.needs > ceiling {
            assert_eq!(status, StatusCode::FORBIDDEN, "{}: {body}", tool.name);
            assert_eq!(body["code"], "permission_denied", "{}", tool.name);
        } else {
            assert_ne!(status, StatusCode::FORBIDDEN, "{}: {body}", tool.name);
        }
    }
}

#[tokio::test]
async fn an_agent_at_read_is_refused_every_tool_that_needs_more() {
    let app = app(MemStore::new()).await;
    set_agent_level(&app, Level::Read).await;
    assert_refused_exactly_above(&app, Level::Read).await;
}

#[tokio::test]
async fn an_agent_at_none_is_refused_every_tool() {
    let app = app(MemStore::new()).await;
    set_agent_level(&app, Level::None).await;
    assert_refused_exactly_above(&app, Level::None).await;
}

#[tokio::test]
async fn an_agent_at_run_is_refused_only_what_needs_admin() {
    let app = app(MemStore::new()).await;
    assert_refused_exactly_above(&app, Level::Run).await;
}

#[tokio::test]
async fn an_agent_whose_owner_can_only_read_cannot_act_above_read() {
    // The agent is stored at Run, but its owner is a Read member, so it acts at Read.
    let base = app(MemStore::new()).await;
    let directory = FakeDirectory::new()
        .with_role(OWNER, RelayRole::Owner)
        .with_role(MEMBER, RelayRole::Member)
        .with_agent(AGENT, MEMBER);
    let app = App {
        directory: Arc::new(directory),
        ..base
    };
    assert_refused_exactly_above(&app, Level::Read).await;
}
