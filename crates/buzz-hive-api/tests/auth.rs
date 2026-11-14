use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use base64::Engine as _;
use buzz_hive_api::replay::{ReplayGuard, REPLAY_TTL};
use buzz_hive_api::token::{SystemClock, TokenMinter};
use buzz_hive_api::unary::unary;
use buzz_hive_api::{ApiConfig, App};
use buzz_hive_core::access::{ActorKind, Level, RelayRole};
use buzz_hive_core::bus::Bus;
use buzz_hive_core::ports::store::{CommunityId, Store};
use buzz_hive_core::testing::{FakeDirectory, MemStore};
use buzz_hive_core::workflow::builtin::builtin_source;
use buzz_hive_core::workflow::source::Sources;
use nostr::nips::nip98::{HttpData, HttpMethod};
use nostr::{EventBuilder, JsonUtil, Keys, Timestamp, Url};
use tower::ServiceExt;

const PUBLIC_URL: &str = "http://hive.test";
const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";

fn community() -> CommunityId {
    CommunityId::from_u128(1)
}

fn app(directory: FakeDirectory, store: MemStore) -> App {
    App {
        config: Arc::new(ApiConfig {
            public_url: PUBLIC_URL.into(),
            allowed_origins: Vec::new(),
        }),
        store: Arc::new(store),
        directory: Arc::new(directory),
        minter: Arc::new(TokenMinter::new(SECRET, Arc::new(SystemClock)).unwrap()),
        replay: Arc::new(ReplayGuard::new(REPLAY_TTL)),
        bus: Bus::new(0),
        catalog: Arc::new(Sources(vec![builtin_source()])),
        community: community(),
    }
}

fn sign(keys: &Keys, url: &str, age_secs: u64) -> String {
    let event = EventBuilder::http_auth(HttpData::new(Url::parse(url).unwrap(), HttpMethod::POST))
        .custom_created_at(Timestamp::from_secs(Timestamp::now().as_secs() - age_secs))
        .sign_with_keys(keys)
        .unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(event.as_json());
    format!("Nostr {encoded}")
}

fn token_request(authorization: &str) -> Request<Body> {
    Request::post("/auth/token")
        .header("authorization", authorization)
        .body(Body::empty())
        .unwrap()
}

async fn send(router: &Router, request: Request<Body>) -> (StatusCode, serde_json::Value) {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

fn router(app: App) -> Router {
    buzz_hive_api::finish(buzz_hive_api::routes(&app), app)
}

fn member(keys: &Keys) -> FakeDirectory {
    FakeDirectory::new().with_role(&keys.public_key().to_hex(), RelayRole::Member)
}

#[tokio::test]
async fn a_valid_event_returns_a_token_for_that_pubkey() {
    let keys = Keys::generate();
    let app = app(member(&keys), MemStore::new());
    let router = router(app.clone());
    let (status, body) = send(
        &router,
        token_request(&sign(&keys, "http://hive.test/auth/token", 0)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let claims = app.minter.verify(body["token"].as_str().unwrap()).unwrap();
    assert_eq!(claims.sub, keys.public_key().to_hex());
    assert_eq!(claims.community, community());
    assert_eq!(claims.owner, None);
    assert_eq!(body["expires_at"].as_i64().unwrap(), claims.exp);
}

#[tokio::test]
async fn a_trailing_slash_on_the_public_url_is_ignored() {
    let keys = Keys::generate();
    let mut app = app(member(&keys), MemStore::new());
    app.config = Arc::new(ApiConfig {
        public_url: "http://hive.test/".into(),
        allowed_origins: Vec::new(),
    });
    let (status, _) = send(
        &router(app),
        token_request(&sign(&keys, "http://hive.test/auth/token", 0)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn an_agent_gets_a_token_that_names_its_owner() {
    let owner = Keys::generate();
    let agent = Keys::generate();
    let directory = FakeDirectory::new()
        .with_role(&owner.public_key().to_hex(), RelayRole::Member)
        .with_agent(&agent.public_key().to_hex(), &owner.public_key().to_hex());
    let app = app(directory, MemStore::new());
    let (status, body) = send(
        &router(app.clone()),
        token_request(&sign(&agent, "http://hive.test/auth/token", 0)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let claims = app.minter.verify(body["token"].as_str().unwrap()).unwrap();
    assert_eq!(claims.owner, Some(owner.public_key().to_hex()));
}

#[tokio::test]
async fn an_event_for_another_url_is_unauthenticated() {
    let keys = Keys::generate();
    let router = router(app(member(&keys), MemStore::new()));
    let (status, body) = send(
        &router,
        token_request(&sign(&keys, "http://hive.test/other", 0)),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "unauthenticated");
}

#[tokio::test]
async fn a_stale_event_is_unauthenticated() {
    let keys = Keys::generate();
    let router = router(app(member(&keys), MemStore::new()));
    let (status, body) = send(
        &router,
        token_request(&sign(&keys, "http://hive.test/auth/token", 120)),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "unauthenticated");
}

#[tokio::test]
async fn a_replayed_event_is_unauthenticated() {
    let keys = Keys::generate();
    let router = router(app(member(&keys), MemStore::new()));
    let header = sign(&keys, "http://hive.test/auth/token", 0);
    let (first, _) = send(&router, token_request(&header)).await;
    assert_eq!(first, StatusCode::OK);
    let (second, body) = send(&router, token_request(&header)).await;
    assert_eq!(second, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "unauthenticated");
    assert_eq!(body["message"], "authorization already used");
}

#[tokio::test]
async fn a_missing_or_malformed_header_is_unauthenticated() {
    let keys = Keys::generate();
    let router = router(app(member(&keys), MemStore::new()));
    for header in [
        "",
        "Bearer abc",
        "Nostr !!!not-base64",
        "Nostr bm90IGpzb24=",
    ] {
        let (status, body) = send(&router, token_request(header)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{header}");
        assert_eq!(body["code"], "unauthenticated");
    }
}

#[tokio::test]
async fn a_stranger_is_permission_denied() {
    let stranger = Keys::generate();
    let router = router(app(FakeDirectory::new(), MemStore::new()));
    let (status, body) = send(
        &router,
        token_request(&sign(&stranger, "http://hive.test/auth/token", 0)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "permission_denied");
}

#[tokio::test]
async fn a_forged_event_does_not_burn_the_real_ones_id() {
    // Verify first, mark second: a tampered copy fails and the real event still works.
    let keys = Keys::generate();
    let router = router(app(member(&keys), MemStore::new()));
    let header = sign(&keys, "http://hive.test/auth/token", 0);
    let json = String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(header.strip_prefix("Nostr ").unwrap())
            .unwrap(),
    )
    .unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    value["sig"] = serde_json::json!("0".repeat(128));
    let forged = format!(
        "Nostr {}",
        base64::engine::general_purpose::STANDARD.encode(value.to_string())
    );
    let (status, _) = send(&router, token_request(&forged)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = send(&router, token_request(&header)).await;
    assert_eq!(status, StatusCode::OK);
}

// ---- the unary wrapper ----

fn echo_router(app: App, needs: Level) -> Router {
    let route = unary(
        app.clone(),
        needs,
        |_app, ctx, req: serde_json::Value| async move {
            Ok(serde_json::json!({ "who": ctx.actor.pubkey, "got": req }))
        },
    );
    buzz_hive_api::finish(buzz_hive_api::routes(&app).route("/test/echo", route), app)
}

fn bearer(app: &App, pubkey: &str) -> String {
    let (token, _) = app.minter.mint(pubkey, app.community, None).unwrap();
    format!("Bearer {token}")
}

fn echo_request(authorization: Option<&str>, body: Body) -> Request<Body> {
    let mut request = Request::post("/test/echo");
    if let Some(value) = authorization {
        request = request.header("authorization", value);
    }
    request.body(body).unwrap()
}

#[tokio::test]
async fn unary_runs_the_handler_for_an_allowed_actor() {
    let admin = Keys::generate().public_key().to_hex();
    let app = app(
        FakeDirectory::new().with_role(&admin, RelayRole::Admin),
        MemStore::new(),
    );
    let router = echo_router(app.clone(), Level::Run);
    let (status, body) = send(
        &router,
        echo_request(Some(&bearer(&app, &admin)), Body::from(r#"{"a":1}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["who"], admin);
    assert_eq!(body["got"], serde_json::json!({"a": 1}));

    // An empty body means `{}`.
    let (status, body) = send(
        &router,
        echo_request(Some(&bearer(&app, &admin)), Body::empty()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["got"], serde_json::json!({}));
}

#[tokio::test]
async fn unary_names_both_levels_when_it_denies() {
    let reader = Keys::generate().public_key().to_hex();
    let app = app(
        FakeDirectory::new().with_role(&reader, RelayRole::Member),
        MemStore::new(),
    );
    let router = echo_router(app.clone(), Level::Run);
    let (status, body) = send(
        &router,
        echo_request(Some(&bearer(&app, &reader)), Body::empty()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "permission_denied");
    assert_eq!(body["message"], "needs Run, you have Read");
}

#[tokio::test]
async fn unary_uses_the_level_stored_now_not_the_one_at_mint_time() {
    let person = Keys::generate().public_key().to_hex();
    let store = MemStore::new();
    let app = app(
        FakeDirectory::new().with_role(&person, RelayRole::Admin),
        store.clone(),
    );
    let router = echo_router(app.clone(), Level::Run);
    let header = bearer(&app, &person);
    let (status, _) = send(&router, echo_request(Some(&header), Body::empty())).await;
    assert_eq!(status, StatusCode::OK);

    let mut tx = store.begin(community()).await.unwrap();
    tx.access()
        .set_level(&person, Level::Read, ActorKind::Person, "admin")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let (status, _) = send(&router, echo_request(Some(&header), Body::empty())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn unary_refuses_a_missing_bad_or_foreign_token() {
    let admin = Keys::generate().public_key().to_hex();
    let app = app(
        FakeDirectory::new().with_role(&admin, RelayRole::Admin),
        MemStore::new(),
    );
    let router = echo_router(app.clone(), Level::Read);
    for header in [None, Some("Bearer nope"), Some("Basic abc")] {
        let (status, body) = send(&router, echo_request(header, Body::empty())).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{header:?}");
        assert_eq!(body["code"], "unauthenticated");
    }
    let (other, _) = app
        .minter
        .mint(&admin, CommunityId::from_u128(2), None)
        .unwrap();
    let (status, _) = send(
        &router,
        echo_request(Some(&format!("Bearer {other}")), Body::empty()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn unary_rejects_invalid_json_as_invalid_argument() {
    let admin = Keys::generate().public_key().to_hex();
    let app = app(
        FakeDirectory::new().with_role(&admin, RelayRole::Admin),
        MemStore::new(),
    );
    let router = echo_router(app.clone(), Level::Read);
    let (status, body) = send(
        &router,
        echo_request(Some(&bearer(&app, &admin)), Body::from("{not json")),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_argument");
}

#[tokio::test]
async fn a_body_over_16_mib_is_refused_and_one_at_the_limit_is_read() {
    let admin = Keys::generate().public_key().to_hex();
    let app = app(
        FakeDirectory::new().with_role(&admin, RelayRole::Admin),
        MemStore::new(),
    );
    let router = echo_router(app.clone(), Level::Read);
    let limit = 16 * 1024 * 1024;
    let header = bearer(&app, &admin);

    let (status, _) = send(
        &router,
        echo_request(Some(&header), Body::from(vec![b' '; limit + 1])),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

    // Exactly at the limit is read; it is whitespace, so it fails as JSON, not as size.
    let (status, body) = send(
        &router,
        echo_request(Some(&header), Body::from(vec![b' '; limit])),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn healthz_answers_without_auth() {
    let router = router(app(FakeDirectory::new(), MemStore::new()));
    let response = router
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_finished_router_answers_a_preflight_only_for_a_configured_origin() {
    let mut app = app(FakeDirectory::new(), MemStore::new());
    app.config = Arc::new(ApiConfig {
        public_url: PUBLIC_URL.into(),
        allowed_origins: vec!["https://app.example.org".into()],
    });
    let router = router(app);
    let preflight = |origin: &str| {
        Request::builder()
            .method("OPTIONS")
            .uri("/auth/token")
            .header("origin", origin)
            .header("access-control-request-method", "POST")
            .body(Body::empty())
            .unwrap()
    };
    let allowed = router
        .clone()
        .oneshot(preflight("https://app.example.org"))
        .await
        .unwrap();
    assert_eq!(
        allowed.headers()["access-control-allow-origin"],
        "https://app.example.org"
    );
    let other = router
        .oneshot(preflight("https://other.example.org"))
        .await
        .unwrap();
    assert!(other.headers().get("access-control-allow-origin").is_none());
}
