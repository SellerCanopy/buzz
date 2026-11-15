//! Cross-origin access for the web build.
//!
//! The list comes from `BUZZ_HIVE_ALLOWED_ORIGINS`. An origin that is not in it gets
//! no CORS headers, and an empty list allows no cross-origin access. The desktop
//! app calls through Rust, so it needs no entry.

use axum::http::{header, HeaderName, Method};
use tower_http::cors::{AllowOrigin, CorsLayer};

/// The CORS layer for `origins`. An entry that is not a valid header value is skipped.
pub fn layer(origins: &[String]) -> CorsLayer {
    let list: Vec<_> = origins.iter().filter_map(|o| o.parse().ok()).collect();
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(list))
        .allow_methods([Method::POST, Method::OPTIONS])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            HeaderName::from_static("connect-protocol-version"),
        ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::post;
    use axum::Router;
    use tower::ServiceExt;

    fn router(origins: &[&str]) -> Router {
        let origins: Vec<String> = origins.iter().map(|o| (*o).to_owned()).collect();
        Router::new()
            .route("/auth/token", post(|| async { "ok" }))
            .layer(layer(&origins))
    }

    fn preflight(origin: &str) -> Request<Body> {
        Request::builder()
            .method("OPTIONS")
            .uri("/auth/token")
            .header("origin", origin)
            .header("access-control-request-method", "POST")
            .header(
                "access-control-request-headers",
                "authorization,content-type",
            )
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn a_preflight_from_an_allowed_origin_is_answered() {
        let response = router(&["https://app.example.org"])
            .oneshot(preflight("https://app.example.org"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let h = response.headers();
        assert_eq!(h["access-control-allow-origin"], "https://app.example.org");
        let methods = h["access-control-allow-methods"].to_str().unwrap();
        assert!(
            methods.contains("POST") && methods.contains("OPTIONS"),
            "{methods}"
        );
        let headers = h["access-control-allow-headers"].to_str().unwrap();
        for name in ["authorization", "content-type", "connect-protocol-version"] {
            assert!(headers.contains(name), "{headers}");
        }
    }

    #[tokio::test]
    async fn a_preflight_from_another_origin_gets_no_allow_origin() {
        let response = router(&["https://app.example.org"])
            .oneshot(preflight("https://evil.example.org"))
            .await
            .unwrap();
        assert!(response
            .headers()
            .get("access-control-allow-origin")
            .is_none());
    }

    #[tokio::test]
    async fn an_empty_list_allows_no_origin() {
        let response = router(&[])
            .oneshot(preflight("https://app.example.org"))
            .await
            .unwrap();
        assert!(response
            .headers()
            .get("access-control-allow-origin")
            .is_none());
    }

    #[tokio::test]
    async fn an_actual_response_carries_the_header_only_for_an_allowed_origin() {
        let request = |origin: &str| {
            Request::post("/auth/token")
                .header("origin", origin)
                .body(Body::empty())
                .unwrap()
        };
        let app = router(&["https://app.example.org"]);
        let allowed = app
            .clone()
            .oneshot(request("https://app.example.org"))
            .await
            .unwrap();
        assert_eq!(
            allowed.headers()["access-control-allow-origin"],
            "https://app.example.org"
        );
        let other = app
            .oneshot(request("https://evil.example.org"))
            .await
            .unwrap();
        assert!(other.headers().get("access-control-allow-origin").is_none());
    }
}
