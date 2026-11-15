//! The access page's routes: who is listed, who may set a level, and what a change leaves behind.

mod common;

use axum::http::StatusCode;
use buzz_hive_api::App;
use buzz_hive_core::access::{ActorKind, Level, RelayRole};
use buzz_hive_core::testing::{FakeDirectory, MemStore};
use common::{app, app_with, call, community, AGENT, MEMBER, OWNER};
use serde_json::json;

async fn who_am_i(app: &App, pubkey: &str) -> String {
    let (_, body) = call(app, "WhoAmI", pubkey, json!({})).await;
    body["role"].as_str().unwrap().to_owned()
}

async fn set(app: &App, by: &str, pubkey: &str, level: &str) -> (StatusCode, serde_json::Value) {
    call(
        app,
        "SetAccessLevel",
        by,
        json!({"pubkey": pubkey, "level": level}),
    )
    .await
}

fn named() -> FakeDirectory {
    FakeDirectory::new()
        .with_role(OWNER, RelayRole::Owner)
        .with_role(MEMBER, RelayRole::Member)
        .with_name(OWNER, "Ada")
        .with_name(MEMBER, "Ben")
        .with_named_agent("builder", AGENT, MEMBER)
}

#[tokio::test]
async fn a_reader_lists_everyone_with_the_level_each_acts_at() {
    let app = app_with(named(), MemStore::new());
    let (status, body) = call(&app, "ListAccess", MEMBER, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["entries"],
        json!([
            {"pubkey": OWNER, "name": "Ada", "kind": "person", "level": "admin"},
            {"pubkey": MEMBER, "name": "Ben", "kind": "person", "level": "read"},
            {"pubkey": AGENT, "name": "builder", "kind": "agent", "ownerPubkey": MEMBER, "level": "none"},
        ])
    );

    let (status, body) = call(&app, "ListPeople", MEMBER, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["people"],
        json!([
            {"userId": OWNER, "name": "Ada", "level": "admin"},
            {"userId": MEMBER, "name": "Ben", "level": "read"},
        ])
    );
}

#[tokio::test]
async fn a_person_at_none_cannot_read_the_list() {
    let app = app_with(named(), MemStore::new());
    assert_eq!(set(&app, OWNER, MEMBER, "none").await.0, StatusCode::OK);
    let (status, body) = call(&app, "ListAccess", MEMBER, json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, _) = call(&app, "ListPeople", MEMBER, json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn an_admin_sets_a_level_and_it_takes_effect_at_once() {
    let app = app_with(named(), MemStore::new());
    assert_eq!(who_am_i(&app, MEMBER).await, "read");
    let (status, body) = set(&app, OWNER, MEMBER, "run").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["level"], "run");
    assert_eq!(body["name"], "Ben");
    assert_eq!(who_am_i(&app, MEMBER).await, "run");
    let (status, body) = call(&app, "CreateCard", MEMBER, json!({"title": "now allowed"})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn each_change_appends_access_changed_with_the_actor() {
    let app = app_with(named(), MemStore::new());
    set(&app, OWNER, MEMBER, "run").await;
    let (status, polled) = call(&app, "PollEvents", MEMBER, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{polled}");
    let events = polled["events"].as_array().unwrap();
    assert_eq!(events.len(), 1, "{polled}");
    assert_eq!(events[0]["type"], "access.changed");
    assert_eq!(events[0]["actorPubkey"], OWNER);
    assert_eq!(events[0]["actorKind"], "person");
    let payload: serde_json::Value =
        serde_json::from_str(events[0]["payloadJson"].as_str().unwrap()).unwrap();
    assert_eq!(
        payload,
        json!({"pubkey": MEMBER, "kind": "person", "level": "run", "previous": "read"})
    );
}

#[tokio::test]
async fn a_person_at_run_cannot_set_a_level() {
    let app = app_with(named(), MemStore::new());
    set(&app, OWNER, MEMBER, "run").await;
    let (status, body) = set(&app, MEMBER, OWNER, "read").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(who_am_i(&app, OWNER).await, "admin");
}

#[tokio::test]
async fn an_agent_above_its_owner_is_refused_naming_both_levels() {
    let app = app_with(named(), MemStore::new());
    let (status, body) = set(&app, OWNER, AGENT, "admin").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "failed_precondition");
    assert_eq!(
        body["message"],
        "agent builder is capped at read by its owner Ben"
    );
    assert_eq!(who_am_i(&app, AGENT).await, "none", "nothing was stored");

    // At its owner's level it is allowed, and raising the owner then raises the cap.
    assert_eq!(set(&app, OWNER, AGENT, "read").await.0, StatusCode::OK);
    assert_eq!(who_am_i(&app, AGENT).await, "read");
    assert_eq!(set(&app, OWNER, MEMBER, "admin").await.0, StatusCode::OK);
    assert_eq!(set(&app, OWNER, AGENT, "admin").await.0, StatusCode::OK);
    assert_eq!(who_am_i(&app, AGENT).await, "admin");
}

#[tokio::test]
async fn an_admin_may_not_lower_their_own_level() {
    let app = app_with(named(), MemStore::new());
    let (status, body) = set(&app, OWNER, OWNER, "read").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(who_am_i(&app, OWNER).await, "admin");
}

#[tokio::test]
async fn a_pubkey_that_is_not_in_the_community_is_not_found() {
    let app = app_with(named(), MemStore::new());
    let stranger = "cc";
    let (status, body) = set(&app, OWNER, stranger, "read").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn a_malformed_request_is_invalid() {
    let app = app_with(named(), MemStore::new());
    let (status, _) = set(&app, OWNER, "not hex", "read").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, body) = set(&app, OWNER, MEMBER, "owner").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(who_am_i(&app, MEMBER).await, "read");
}

#[tokio::test]
async fn an_agent_at_admin_with_no_person_behind_it_cannot_set_a_level() {
    let app = app(MemStore::new()).await;
    // The agent's owner is the relay owner, so Admin is within its cap.
    let mut tx = app.store.begin(community()).await.unwrap();
    tx.access()
        .set_level(AGENT, Level::Admin, ActorKind::Agent, OWNER)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(who_am_i(&app, AGENT).await, "admin");

    let (status, body) = set(&app, AGENT, MEMBER, "run").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("at Admin"),
        "{body}"
    );
    assert_eq!(who_am_i(&app, MEMBER).await, "read");
}
