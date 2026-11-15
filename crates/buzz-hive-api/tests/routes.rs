//! The procedures served in Phase 3: `GetStatus`, `WhoAmI` and `ListCards`.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use buzz_hive_api::routes::{path, ROUTES};
use buzz_hive_api::App;
use buzz_hive_core::access::RelayRole;
use buzz_hive_core::domain::card::{Card, CardState};
use buzz_hive_core::ports::store::CommunityId;
use buzz_hive_core::testing::{FakeDirectory, MemStore};
use common::{app_with, community, router, AGENT, MEMBER, OWNER, STRANGER};
use tower::ServiceExt;
use uuid::Uuid;

/// Here the agent belongs to `MEMBER`, who can only read, so the agent starts at None.
fn app(store: MemStore) -> App {
    let directory = FakeDirectory::new()
        .with_role(OWNER, RelayRole::Owner)
        .with_role(MEMBER, RelayRole::Member)
        .with_agent(AGENT, MEMBER);
    app_with(directory, store)
}

async fn call(
    app: &App,
    method: &str,
    pubkey: Option<&str>,
    body: &str,
) -> (StatusCode, serde_json::Value) {
    let mut request = Request::post(path(method));
    if let Some(pubkey) = pubkey {
        let (token, _) = app.minter.mint(pubkey, app.community, None).unwrap();
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = router(app)
        .oneshot(request.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

fn card(n: u128, title: &str, archived: bool) -> Card {
    Card {
        id: Uuid::from_u128(n),
        title: title.into(),
        description: String::new(),
        state: CardState::Idle,
        workflow: String::new(),
        archived,
        created_by: OWNER.into(),
    }
}

#[tokio::test]
async fn every_served_procedure_refuses_a_call_with_no_token() {
    let app = app(MemStore::new());
    for (method, _) in ROUTES {
        let (status, body) = call(&app, method, None, "{}").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method}");
        assert_eq!(body["code"], "unauthenticated", "{method}");
    }
}

#[tokio::test]
async fn list_cards_with_no_token_is_unauthenticated() {
    let (status, body) = call(&app(MemStore::new()), "ListCards", None, "{}").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "unauthenticated");
}

#[tokio::test]
async fn list_cards_on_an_empty_board_is_an_empty_object() {
    let app = app(MemStore::new());
    let (status, body) = call(&app, "ListCards", Some(MEMBER), "{}").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Protobuf JSON leaves out empty and zero fields.
    assert_eq!(body, serde_json::json!({}));
}

#[tokio::test]
async fn list_cards_returns_the_board_newest_first_or_the_archive() {
    let store = MemStore::new();
    store.seed_card(community(), card(1, "older", false));
    store.seed_card(community(), card(2, "newer", false));
    store.seed_card(community(), card(3, "gone", true));
    store.seed_card(CommunityId::from_u128(2), card(4, "elsewhere", false));
    let app = app(store);

    let (_, body) = call(&app, "ListCards", Some(MEMBER), "{}").await;
    let titles: Vec<_> = body["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["title"].clone())
        .collect();
    assert_eq!(titles, ["newer", "older"]);
    assert_eq!(body["cards"][0]["state"], "idle");

    let (_, body) = call(&app, "ListCards", Some(MEMBER), r#"{"archived":true}"#).await;
    assert_eq!(body["cards"].as_array().unwrap().len(), 1);
    assert_eq!(body["cards"][0]["archived"], true);
}

#[tokio::test]
async fn list_cards_needs_read() {
    let app = app(MemStore::new());
    let (status, body) = call(&app, "ListCards", Some(STRANGER), "{}").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["message"], "needs Read, you have None");
}

#[tokio::test]
async fn who_am_i_reports_owner_as_admin_and_member_as_read() {
    let app = app(MemStore::new());
    let (status, body) = call(&app, "WhoAmI", Some(OWNER), "{}").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["me"]["userId"], OWNER);
    assert_eq!(body["role"], "admin");
    assert_eq!(body["kind"], "person");
    assert_eq!(body["companyId"], community().to_string());

    let (_, body) = call(&app, "WhoAmI", Some(MEMBER), "{}").await;
    assert_eq!(body["role"], "read");
    assert!(body.get("owner").is_none());
}

#[tokio::test]
async fn who_am_i_answers_a_person_at_none_and_names_an_agents_owner() {
    let app = app(MemStore::new());
    let (status, body) = call(&app, "WhoAmI", Some(STRANGER), "{}").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a person at None still learns their level"
    );
    assert_eq!(body["role"], "none");

    let (_, body) = call(&app, "WhoAmI", Some(AGENT), "{}").await;
    assert_eq!(body["kind"], "agent");
    assert_eq!(body["owner"], MEMBER);
    assert_eq!(body["role"], "none", "an agent starts at None");
}

#[tokio::test]
async fn get_status_reports_version_and_community() {
    let app = app(MemStore::new());
    let (status, body) = call(&app, "GetStatus", Some(MEMBER), "{}").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(body["communityId"], community().to_string());
}
