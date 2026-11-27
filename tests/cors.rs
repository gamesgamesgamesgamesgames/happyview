mod common;

use axum::body::Body;
use axum::http::Request;
use serial_test::serial;
use tower::ServiceExt;

/// The origin the TestApp registers in its DomainCache (a trusted first-party
/// domain — where the dashboard / admin UI is served).
const TRUSTED_ORIGIN: &str = "http://127.0.0.1:0";
/// An arbitrary untrusted origin (e.g. a third-party app or a malicious page).
const UNTRUSTED_ORIGIN: &str = "https://evil.example";

fn preflight(origin: &str, request_method: &str) -> Request<Body> {
    Request::builder()
        .method("OPTIONS")
        .uri("/xrpc/com.example.test")
        .header("host", "127.0.0.1")
        .header("origin", origin)
        .header("access-control-request-method", request_method)
        .header(
            "access-control-request-headers",
            "content-type, authorization",
        )
        .body(Body::empty())
        .unwrap()
}

fn get_with_origin(uri: &str, origin: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", "127.0.0.1")
        .header("origin", origin)
        .body(Body::empty())
        .unwrap()
}

fn header<'a>(resp: &'a axum::http::Response<Body>, name: &str) -> Option<&'a str> {
    resp.headers().get(name).and_then(|v| v.to_str().ok())
}

#[tokio::test]
#[serial]
async fn preflight_from_trusted_origin_allows_credentials() {
    common::require_db!();
    let app = common::app::TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(preflight(TRUSTED_ORIGIN, "POST"))
        .await
        .unwrap();

    assert!(resp.status().is_success());
    assert_eq!(
        header(&resp, "access-control-allow-origin"),
        Some(TRUSTED_ORIGIN)
    );
    assert_eq!(
        header(&resp, "access-control-allow-credentials"),
        Some("true"),
        "trusted first-party origins must be allowed to send credentials"
    );
}

#[tokio::test]
#[serial]
async fn preflight_from_untrusted_origin_never_allows_credentials() {
    common::require_db!();
    let app = common::app::TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(preflight(UNTRUSTED_ORIGIN, "POST"))
        .await
        .unwrap();

    // The untrusted origin may still use credential-less (cookieless) CORS —
    // e.g. a third-party DPoP client — but it must NEVER be granted credentials.
    assert_eq!(
        header(&resp, "access-control-allow-credentials"),
        None,
        "untrusted origins must never be allowed to send credentials"
    );
}

#[tokio::test]
#[serial]
async fn actual_request_from_trusted_origin_allows_credentials() {
    common::require_db!();
    let app = common::app::TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(get_with_origin("/health", TRUSTED_ORIGIN))
        .await
        .unwrap();

    assert_eq!(
        header(&resp, "access-control-allow-origin"),
        Some(TRUSTED_ORIGIN)
    );
    assert_eq!(
        header(&resp, "access-control-allow-credentials"),
        Some("true")
    );
}

#[tokio::test]
#[serial]
async fn actual_request_from_untrusted_origin_never_allows_credentials() {
    common::require_db!();
    let app = common::app::TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(get_with_origin("/health", UNTRUSTED_ORIGIN))
        .await
        .unwrap();

    assert_eq!(
        header(&resp, "access-control-allow-credentials"),
        None,
        "untrusted origins must never be allowed to send credentials"
    );
}

#[tokio::test]
#[serial]
async fn request_without_origin_gets_no_cors_headers() {
    common::require_db!();
    let app = common::app::TestApp::new().await;

    let req = Request::builder()
        .method("GET")
        .uri("/health")
        .header("host", "127.0.0.1")
        .body(Body::empty())
        .unwrap();

    let resp = app.router.clone().oneshot(req).await.unwrap();

    assert!(resp.status().is_success());
    assert_eq!(header(&resp, "access-control-allow-origin"), None);
    assert_eq!(header(&resp, "access-control-allow-credentials"), None);
}

#[tokio::test]
#[serial]
async fn preflight_allows_the_admin_api_update_methods() {
    common::require_db!();
    let app = common::app::TestApp::new().await;

    for method in ["PUT", "PATCH"] {
        let resp = app
            .router
            .clone()
            .oneshot(preflight(TRUSTED_ORIGIN, method))
            .await
            .unwrap();
        let allowed = header(&resp, "access-control-allow-methods").unwrap_or_default();
        assert!(
            allowed.split(", ").any(|m| m == method),
            "a browser client must be able to send {method} to the admin API, got {allowed:?}"
        );
    }
}
