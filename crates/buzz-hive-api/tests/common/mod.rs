//! A test app over `MemStore` and a helper that calls a procedure with a minted token.

#![allow(dead_code)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use buzz_hive_api::replay::{ReplayGuard, REPLAY_TTL};
use buzz_hive_api::routes::path;
use buzz_hive_api::token::{SystemClock, TokenMinter};
use buzz_hive_api::{ApiConfig, App};
use buzz_hive_core::access::{ActorKind, Level, RelayRole};
use buzz_hive_core::bus::Bus;
use buzz_hive_core::ports::store::{CommunityId, Store};
use buzz_hive_core::testing::{FakeDirectory, MemStore};
use buzz_hive_core::workflow::builtin::builtin_source;
use buzz_hive_core::workflow::source::Sources;
use tower::ServiceExt;

pub const OWNER: &str = "aa";
pub const MEMBER: &str = "bb";
pub const STRANGER: &str = "cc";
pub const AGENT: &str = "dd";
/// The Beehive account.
pub const HIVE: &str = "ee";

pub fn community() -> CommunityId {
    CommunityId::from_u128(1)
}

/// `OWNER` is a relay owner, `MEMBER` a relay member, and `AGENT` belongs to `OWNER`.
pub fn people() -> FakeDirectory {
    FakeDirectory::new()
        .with_role(OWNER, RelayRole::Owner)
        .with_role(MEMBER, RelayRole::Member)
        .with_agent(AGENT, OWNER)
}

/// An app over `store` and `directory`, with the built-in workflows and the Beehive account
/// `HIVE`. Nothing is stored in it yet.
pub fn app_with(directory: FakeDirectory, store: MemStore) -> App {
    App {
        config: Arc::new(ApiConfig {
            public_url: "http://hive.test".into(),
            allowed_origins: Vec::new(),
        }),
        store: Arc::new(store),
        directory: Arc::new(directory),
        minter: Arc::new(
            TokenMinter::new(b"0123456789abcdef0123456789abcdef", Arc::new(SystemClock)).unwrap(),
        ),
        replay: Arc::new(ReplayGuard::new(REPLAY_TTL)),
        auth_limit: buzz_hive_api::rate::auth_limiter(),
        bus: Bus::new(0),
        catalog: Arc::new(Sources(vec![builtin_source()])),
        community: community(),
        hive_pubkey: HIVE.into(),
        outbox: Arc::new(tokio::sync::Notify::new()),
    }
}

/// [`people`], where `OWNER` is Admin, `MEMBER` is Read, and `AGENT` has been given Run.
pub async fn app(store: MemStore) -> App {
    let mut tx = store.begin(community()).await.unwrap();
    tx.access()
        .set_level(AGENT, Level::Run, ActorKind::Agent, OWNER)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    app_with(people(), store)
}

pub fn router(app: &App) -> Router {
    buzz_hive_api::finish(buzz_hive_api::routes(), app.clone())
}

pub async fn call(
    app: &App,
    method: &str,
    pubkey: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let (token, _) = app.minter.mint(pubkey, app.community, None).unwrap();
    let request = Request::post(path(method))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = router(app).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}
