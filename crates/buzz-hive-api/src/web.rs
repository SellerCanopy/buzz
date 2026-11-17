//! The Beehive-only web page the phone's web view loads.
//!
//! The service serves a built bundle at [`WEB_PATH`], on the same origin as
//! [`crate::AUTH_TOKEN_PATH`], so the page needs no cross-origin access. Nothing here is
//! served unless the operator points `BUZZ_HIVE_WEB_DIR` at a folder. The files are public
//! and carry no data: every call the page makes is authenticated as usual.

use std::path::Path;

use axum::extract::Request;
use axum::http::{header, HeaderValue};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::Router;
use tower_http::services::{ServeDir, ServeFile};

use crate::app::App;

/// Where the bundle is served, with no trailing slash.
pub const WEB_PATH: &str = "/beehive";

/// What the page may load and call: itself only. Styles may be inline, images may be inlined.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; connect-src 'self'; \
img-src 'self' data:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'";

/// Serves the bundle in `dir` at [`WEB_PATH`]. A path that names no file gets `index.html`, so
/// the page's own routes load. `None` when `dir` holds no `index.html`: the image carries an
/// empty folder when it was built without a bundle, and then there is no page to serve.
pub fn router(dir: &Path) -> Option<Router<App>> {
    let index = dir.join("index.html");
    if !index.is_file() {
        return None;
    }
    let files = ServeDir::new(dir).fallback(ServeFile::new(index));
    Some(
        Router::new()
            .nest_service(WEB_PATH, files)
            .layer(middleware::from_fn(content_security_policy)),
    )
}

async fn content_security_policy(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CONTENT_SECURITY_POLICY),
    );
    response
}
