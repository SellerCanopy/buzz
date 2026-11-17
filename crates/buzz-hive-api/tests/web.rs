//! The Beehive web page: served from `BUZZ_HIVE_WEB_DIR` at `/beehive/`, or not at all.

mod common;

use std::path::PathBuf;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use buzz_hive_api::web::CONTENT_SECURITY_POLICY;
use buzz_hive_core::testing::MemStore;
use common::{app_with, people};
use tower::ServiceExt;

/// A folder holding a built page, removed when dropped.
struct Bundle {
    /// The folder that is served, inside `parent`.
    dir: PathBuf,
    /// Holds the folder and a file beside it that must never be served.
    parent: PathBuf,
}

/// A line that appears only in the file beside the served folder.
const SECRET: &str = "secret-beside-the-bundle-7f3a9c";

impl Bundle {
    fn new() -> Self {
        let parent = std::env::temp_dir().join(format!("beehive-web-{}", uuid::Uuid::new_v4()));
        let dir = parent.join("bundle");
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(dir.join("index.html"), "<html>beehive</html>").unwrap();
        std::fs::write(dir.join("assets/app.js"), "console.log('beehive');").unwrap();
        std::fs::write(parent.join("secret.txt"), SECRET).unwrap();
        Self { dir, parent }
    }
}

impl Drop for Bundle {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.parent);
    }
}

fn router(web: Option<&Bundle>) -> Router {
    let app = app_with(people(), MemStore::new());
    let mut routes = buzz_hive_api::routes();
    if let Some(bundle) = web {
        routes = routes.merge(buzz_hive_api::web::from_config(Some(&bundle.dir)).unwrap());
    }
    buzz_hive_api::finish(routes, app)
}

async fn get(router: &Router, path: &str) -> (StatusCode, axum::http::HeaderMap, String) {
    let response = router
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn a_file_in_the_folder_is_served_without_a_token() {
    let bundle = Bundle::new();
    let router = router(Some(&bundle));
    let (status, _, body) = get(&router, "/beehive/assets/app.js").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "console.log('beehive');");
}

#[tokio::test]
async fn the_folder_root_is_the_index_page() {
    let bundle = Bundle::new();
    let router = router(Some(&bundle));
    for path in ["/beehive/", "/beehive"] {
        let (status, _, body) = get(&router, path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(body, "<html>beehive</html>", "{path}");
    }
}

#[tokio::test]
async fn an_unknown_path_gets_the_index_page() {
    let bundle = Bundle::new();
    let router = router(Some(&bundle));
    let (status, _, body) = get(&router, "/beehive/cards/42").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "<html>beehive</html>");
}

#[tokio::test]
async fn every_page_response_carries_the_strict_policy() {
    let bundle = Bundle::new();
    let router = router(Some(&bundle));
    for path in ["/beehive/", "/beehive/assets/app.js", "/beehive/cards/42"] {
        let (_, headers, _) = get(&router, path).await;
        assert_eq!(
            headers.get("content-security-policy").unwrap(),
            CONTENT_SECURITY_POLICY,
            "{path}"
        );
    }
}

#[test]
fn the_policy_allows_nothing_beyond_the_page_itself() {
    // Pinned whole: a new source in any directive is a decision to make on purpose.
    assert_eq!(
        CONTENT_SECURITY_POLICY,
        "default-src 'self'; connect-src 'self'; img-src 'self' data:; \
style-src 'self' 'unsafe-inline'; frame-ancestors 'none'"
    );
}

#[tokio::test]
async fn other_routes_carry_no_policy_and_still_work() {
    let bundle = Bundle::new();
    let router = router(Some(&bundle));
    let (status, headers, body) = get(&router, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "ok");
    assert!(headers.get("content-security-policy").is_none());
}

#[tokio::test]
async fn a_path_outside_the_folder_is_not_served() {
    let bundle = Bundle::new();
    let router = router(Some(&bundle));
    // The file beside the folder is readable on disk, so a leak shows as its marker.
    assert_eq!(
        std::fs::read_to_string(bundle.parent.join("secret.txt")).unwrap(),
        SECRET
    );
    for path in [
        "/beehive/..%2fsecret.txt",
        "/beehive/%2e%2e/secret.txt",
        "/beehive/%2e%2e%2fsecret.txt",
        "/beehive/../secret.txt",
        "/beehive/assets/../../secret.txt",
        "/beehive/..\\secret.txt",
    ] {
        let (_, _, body) = get(&router, path).await;
        assert!(!body.contains(SECRET), "{path} leaked the file");
    }
}

#[tokio::test]
async fn unset_serves_no_page() {
    let router = router(None);
    for path in ["/beehive/", "/beehive", "/beehive/assets/app.js"] {
        let (status, headers, _) = get(&router, path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert!(headers.get("content-security-policy").is_none(), "{path}");
    }
}

#[test]
fn no_setting_means_no_routes() {
    assert!(buzz_hive_api::web::from_config(None).is_none());
}

#[test]
fn a_folder_with_no_index_serves_no_page() {
    let bundle = Bundle::new();
    std::fs::remove_file(bundle.dir.join("index.html")).unwrap();
    assert!(buzz_hive_api::web::from_config(Some(&bundle.dir)).is_none());
}

#[test]
fn a_folder_that_does_not_exist_serves_no_page() {
    let bundle = Bundle::new();
    assert!(buzz_hive_api::web::from_config(Some(&bundle.parent.join("missing"))).is_none());
}

#[test]
fn a_folder_with_an_index_serves_the_page() {
    let bundle = Bundle::new();
    assert!(buzz_hive_api::web::from_config(Some(&bundle.dir)).is_some());
}
