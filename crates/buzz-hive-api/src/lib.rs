//! HTTP routes and token auth for the Beehive workflow engine.

pub mod app;
pub mod auth;
pub mod claims;
pub mod cors;
pub mod error;
pub mod proto;
pub mod rate;
pub mod replay;
pub mod routes;
pub mod token;
pub mod tools;
pub mod unary;
pub mod web;

pub use app::{ApiConfig, App};
pub use error::HiveError;

use axum::body::Body;
use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderValue, Request};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use tower_http::trace::{DefaultOnResponse, TraceLayer};

/// Where a signed NIP-98 event is traded for a token. Signed events name it, so the server's
/// route and every client read it from here.
pub const AUTH_TOKEN_PATH: &str = "/auth/token";

/// Where an agent trades a chat event or a run for a requester claim.
pub const CLAIM_PATH: &str = "/hive/v1/claim";

/// The public key claims are signed with, as a JWK set, for servers that check them.
pub const JWKS_PATH: &str = "/.well-known/jwks.json";

/// Largest request body any route reads.
pub const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// The routes this crate serves: health, sign-in and every procedure in [`routes::ROUTES`].
pub fn routes() -> Router<App> {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route(AUTH_TOKEN_PATH, post(auth::token))
        .route(CLAIM_PATH, post(claims::claim))
        .route(JWKS_PATH, get(claims::jwks))
        .merge(routes::router())
}

/// Header that carries the id of a request, taken from the caller when it sends one.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// Cap request bodies, trace each request, add the CORS layer for the configured origins and
/// bind the app state.
///
/// Every request gets an id, kept from the caller's `x-request-id` when that is short and
/// plain, and returned in the response header. The trace span carries the method, path and
/// id, so a log line from a handler, the error that refused it and the client's report all
/// line up. The finished request logs its status and latency.
pub fn finish(routes: Router<App>, app: App) -> Router {
    let cors = cors::layer(&app.config.allowed_origins);
    let trace = TraceLayer::new_for_http()
        .make_span_with(|request: &Request<Body>| {
            let id = request
                .extensions()
                .get::<RequestId>()
                .map(|id| id.0.as_str())
                .unwrap_or_default();
            tracing::info_span!(
                "request",
                method = %request.method(),
                path = %request.uri().path(),
                request_id = %id,
            )
        })
        .on_response(DefaultOnResponse::new().level(tracing::Level::INFO));
    routes
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(cors)
        .layer(trace)
        .layer(middleware::from_fn(request_id))
        .with_state(app)
}

#[derive(Clone)]
struct RequestId(String);

async fn request_id(mut request: Request<Body>, next: Next) -> Response {
    let id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|v| {
            !v.is_empty()
                && v.len() <= 64
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    request.extensions_mut().insert(RequestId(id.clone()));
    let mut response = next.run(request).await;
    if let Ok(value) = HeaderValue::from_str(&id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    response
}
