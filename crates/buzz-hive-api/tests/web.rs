//! The Beehive web page: served from `BUZZ_HIVE_WEB_DIR` at `/beehive/`, or not at all.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use buzz_hive_api::web::CONTENT_SECURITY_POLICY;
use buzz_hive_core::testing::MemStore;
use common::{app_with, people};
use tower::ServiceExt;

/// A folder holding a built page, removed when dropped.
struct Bundle(std::path::PathBuf);

impl Bundle {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("beehive-web-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(dir.join("index.html"), "<html>beehive</html>").unwrap();
        std::fs::write(dir.join("assets/app.js"), "console.log('beehive');").unwrap();
        Self(dir)
    }
}

impl Drop for Bundle {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn router(web: Option<&Bundle>) -> Router {
    let app = app_with(people(), MemStore::new());
    let mut routes = buzz_hive_api::routes();
    if let Some(bundle) = web {
        routes = routes.merge(buzz_hive_api::web::router(&bundle.0).unwrap());
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
    assert!(CONTENT_SECURITY_POLICY.contains("default-src 'self'"));
    assert!(CONTENT_SECURITY_POLICY.contains("connect-src 'self'"));
    assert!(CONTENT_SECURITY_POLICY.contains("frame-ancestors 'none'"));
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
    let (_, _, body) = get(&router, "/beehive/..%2f..%2fetc%2fpasswd").await;
    assert!(!body.contains("root:"), "{body}");
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
fn a_folder_with_no_index_serves_no_page() {
    let dir = std::env::temp_dir().join(format!("beehive-web-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let router = buzz_hive_api::web::router(&dir);
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(router.is_none());
}
