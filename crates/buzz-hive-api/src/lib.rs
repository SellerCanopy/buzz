//! HTTP routes and token auth for the Beehive workflow engine.

pub mod app;
pub mod auth;
pub mod cors;
pub mod error;
pub mod replay;
pub mod token;
pub mod unary;

pub use app::{ApiConfig, App};
pub use error::HiveError;

use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use axum::Router;

/// Largest request body any route reads.
pub const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// The routes this crate serves. Later phases add to this router.
pub fn routes() -> Router<App> {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/auth/token", post(auth::token))
}

/// Cap request bodies, add the CORS layer for the configured origins and bind the app state.
pub fn finish(routes: Router<App>, app: App) -> Router {
    let cors = cors::layer(&app.config.allowed_origins);
    routes
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(cors)
        .with_state(app)
}
