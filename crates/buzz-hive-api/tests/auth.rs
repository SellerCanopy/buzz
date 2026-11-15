mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use base64::Engine as _;
use buzz_hive_api::rate::RateLimiter;
use buzz_hive_api::replay::{ReplayGuard, REPLAY_TTL};
use buzz_hive_api::unary::unary;
use buzz_hive_api::{ApiConfig, App};
use buzz_hive_core::access::{ActorKind, Level, RelayRole};
use buzz_hive_core::ports::store::{CommunityId, Store};
use buzz_hive_core::testing::{FakeDirectory, MemStore};
use nostr::nips::nip98::{HttpData, HttpMethod};
use nostr::{EventBuilder, JsonUtil, Keys, Tag, TagKind, Timestamp, Url};
use tower::ServiceExt;

use common::{app_with as app, community, router};

const PUBLIC_URL: &str = "http://hive.test";

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

fn member(keys: &Keys) -> FakeDirectory {
    FakeDirectory::new().with_role(&keys.public_key().to_hex(), RelayRole::Member)
}

#[tokio::test]
async fn a_valid_event_returns_a_token_for_that_pubkey() {
    let keys = Keys::generate();
    let app = app(member(&keys), MemStore::new());
    let router = router(&app);
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
        &router(&app),
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
        &router(&app),
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
    let router = router(&app(member(&keys), MemStore::new()));
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
    let router = router(&app(member(&keys), MemStore::new()));
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
    let router = router(&app(member(&keys), MemStore::new()));
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
    let router = router(&app(member(&keys), MemStore::new()));
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
    let router = router(&app(FakeDirectory::new(), MemStore::new()));
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
    let router = router(&app(member(&keys), MemStore::new()));
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
    let route = unary(needs, |_app, ctx, req: serde_json::Value| async move {
        Ok(serde_json::json!({ "who": ctx.actor.pubkey, "got": req }))
    });
    buzz_hive_api::finish(buzz_hive_api::routes().route("/test/echo", route), app)
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
    let router = router(&app(FakeDirectory::new(), MemStore::new()));
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
    let router = router(&app);
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

// ---- who may sign in, and how fast ----

fn url() -> String {
    format!("{PUBLIC_URL}/auth/token")
}

#[tokio::test]
async fn a_person_the_relay_no_longer_lists_gets_no_token_whatever_level_is_stored() {
    let keys = Keys::generate();
    let hex = keys.public_key().to_hex();
    let store = MemStore::new();
    let mut tx = store.begin(community()).await.unwrap();
    tx.access()
        .set_level(&hex, Level::Admin, ActorKind::Person, "aa")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    // The relay lists no role for this key: the person was removed.
    let app = app(FakeDirectory::new(), store);
    let (status, body) = send(&router(&app), token_request(&sign(&keys, &url(), 0))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "permission_denied");
}

#[tokio::test]
async fn a_deactivated_agent_gets_no_token_even_with_a_stored_level() {
    let owner = Keys::generate().public_key().to_hex();
    let agent = Keys::generate();
    let hex = agent.public_key().to_hex();
    let store = MemStore::new();
    let mut tx = store.begin(community()).await.unwrap();
    tx.access()
        .set_level(&hex, Level::Run, ActorKind::Agent, &owner)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let directory = FakeDirectory::new()
        .with_role(&owner, RelayRole::Member)
        .with_agent(&hex, &owner)
        .with_deactivated(&hex);
    let app = app(directory, store);
    let (status, body) = send(&router(&app), token_request(&sign(&agent, &url(), 0))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn a_token_minted_before_removal_stops_working_once_the_relay_drops_the_member() {
    let keys = Keys::generate();
    let hex = keys.public_key().to_hex();
    let app = app(member(&keys), MemStore::new());
    let bearer = bearer(&app, &hex);
    let (status, _) = send(
        &echo_router(app.clone(), Level::Read),
        echo_request(Some(&bearer), Body::from("{}")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // The same token, after the relay has dropped the member.
    let gone = App {
        directory: Arc::new(FakeDirectory::new()),
        ..app
    };
    let (status, body) = send(
        &echo_router(gone, Level::Read),
        echo_request(Some(&bearer), Body::from("{}")),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn a_stranger_does_not_take_a_slot_in_the_replay_guard() {
    let member_keys = Keys::generate();
    let mut app = app(member(&member_keys), MemStore::new());
    // Room for a single id.
    app.replay = Arc::new(ReplayGuard::with_capacity(REPLAY_TTL, 1));
    let router = router(&app);
    for _ in 0..3 {
        let stranger = Keys::generate();
        let (status, _) = send(&router, token_request(&sign(&stranger, &url(), 0))).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    let (status, body) = send(&router, token_request(&sign(&member_keys, &url(), 0))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn a_full_replay_guard_refuses_new_ids_instead_of_growing() {
    let first = Keys::generate();
    let second = Keys::generate();
    let directory = member(&first).with_role(&second.public_key().to_hex(), RelayRole::Member);
    let mut app = app(directory, MemStore::new());
    app.replay = Arc::new(ReplayGuard::with_capacity(REPLAY_TTL, 1));
    let router = router(&app);
    let (status, _) = send(&router, token_request(&sign(&first, &url(), 0))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = send(&router, token_request(&sign(&second, &url(), 0))).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], "unavailable");
}

#[tokio::test]
async fn sign_ins_beyond_the_rate_limit_get_429_before_any_lookup() {
    let keys = Keys::generate();
    let mut app = app(member(&keys), MemStore::new());
    // Two at once, and none refilled during the test.
    app.auth_limit = Arc::new(RateLimiter::new(0, 2));
    let router = router(&app);
    for age in 0..2 {
        let (status, _) = send(&router, token_request(&sign(&keys, &url(), age))).await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, body) = send(&router, token_request(&sign(&keys, &url(), 2))).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["code"], "resource_exhausted");
}

#[tokio::test]
async fn a_forged_event_does_not_spend_the_rate_limit() {
    let keys = Keys::generate();
    let mut app = app(member(&keys), MemStore::new());
    app.auth_limit = Arc::new(RateLimiter::new(0, 1));
    let router = router(&app);
    let (status, _) = send(&router, token_request("Nostr bm90IGpzb24=")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = send(&router, token_request(&sign(&keys, &url(), 0))).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn every_response_carries_a_request_id_and_keeps_a_plain_one_from_the_caller() {
    let router = router(&app(FakeDirectory::new(), MemStore::new()));
    let response = router
        .clone()
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(!response.headers()["x-request-id"].is_empty());
    let kept = router
        .clone()
        .oneshot(
            Request::get("/healthz")
                .header("x-request-id", "trace-123")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(kept.headers()["x-request-id"], "trace-123");
    let replaced = router
        .oneshot(
            Request::get("/healthz")
                .header("x-request-id", "has spaces and \"quotes\"")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        replaced.headers()["x-request-id"],
        "has spaces and \"quotes\""
    );
}

#[tokio::test]
async fn an_event_with_a_nonce_tag_is_accepted_and_two_nonces_are_two_events() {
    let keys = Keys::generate();
    let router = router(&app(member(&keys), MemStore::new()));
    let signed = |nonce: &str| {
        let event =
            EventBuilder::http_auth(HttpData::new(Url::parse(&url()).unwrap(), HttpMethod::POST))
                .tag(Tag::custom(TagKind::custom("nonce"), [nonce]))
                .sign_with_keys(&keys)
                .unwrap();
        format!(
            "Nostr {}",
            base64::engine::general_purpose::STANDARD.encode(event.as_json())
        )
    };
    // Both are signed in the same second, and neither is a replay of the other.
    let (first, body) = send(&router, token_request(&signed("one"))).await;
    assert_eq!(first, StatusCode::OK, "{body}");
    let (second, body) = send(&router, token_request(&signed("two"))).await;
    assert_eq!(second, StatusCode::OK, "{body}");
}
