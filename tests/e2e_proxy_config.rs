mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;

async fn json_body(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

fn admin_get(
    uri: &str,
    cookie: (axum::http::HeaderName, axum::http::HeaderValue),
) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(cookie.0, cookie.1)
        .body(Body::empty())
        .unwrap()
}

fn admin_put(
    uri: &str,
    cookie: (axum::http::HeaderName, axum::http::HeaderValue),
    body: &Value,
) -> Request<Body> {
    Request::builder()
        .method(Method::PUT)
        .uri(uri)
        .header(cookie.0, cookie.1)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

#[tokio::test]
#[serial]
async fn get_proxy_config_returns_default() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/settings/xrpc-proxy", app.admin_cookie()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = json_body(resp).await;
    assert_eq!(json["mode"], "open");
    assert_eq!(json["nsids"], json!([]));
}

#[tokio::test]
#[serial]
async fn put_and_get_allowlist() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_put(
            "/admin/settings/xrpc-proxy",
            app.admin_cookie(),
            &json!({
                "mode": "allowlist",
                "nsids": ["com.example.feed.*", "com.other.thing.getStuff"]
            }),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/settings/xrpc-proxy", app.admin_cookie()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = json_body(resp).await;
    assert_eq!(json["mode"], "allowlist");
    assert_eq!(
        json["nsids"],
        json!(["com.example.feed.*", "com.other.thing.getStuff"])
    );
}

#[tokio::test]
#[serial]
async fn disabled_mode_clears_nsids() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_put(
            "/admin/settings/xrpc-proxy",
            app.admin_cookie(),
            &json!({
                "mode": "disabled",
                "nsids": ["com.example.*"]
            }),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/settings/xrpc-proxy", app.admin_cookie()))
        .await
        .unwrap();

    let json = json_body(resp).await;
    assert_eq!(json["mode"], "disabled");
    assert_eq!(json["nsids"], json!([]));
}

#[tokio::test]
#[serial]
async fn invalid_mode_rejected() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_put(
            "/admin/settings/xrpc-proxy",
            app.admin_cookie(),
            &json!({
                "mode": "yolo",
                "nsids": []
            }),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
#[serial]
async fn invalid_nsid_rejected() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_put(
            "/admin/settings/xrpc-proxy",
            app.admin_cookie(),
            &json!({
                "mode": "allowlist",
                "nsids": ["*"]
            }),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[serial]
async fn put_and_get_blocklist() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_put(
            "/admin/settings/xrpc-proxy",
            app.admin_cookie(),
            &json!({
                "mode": "blocklist",
                "nsids": ["com.blocked.feed.*"]
            }),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/settings/xrpc-proxy", app.admin_cookie()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = json_body(resp).await;
    assert_eq!(json["mode"], "blocklist");
    assert_eq!(json["nsids"], json!(["com.blocked.feed.*"]));
}

#[tokio::test]
#[serial]
async fn put_keeping_stored_legacy_pattern_succeeds() {
    common::require_db!();
    let app = TestApp::new().await;

    // Seed the in-memory config with a pattern the current validator would
    // reject (a hyphen at the end of a domain-label segment) but an older,
    // laxer validator once accepted. This simulates a legacy stored config.
    app.state
        .proxy_config
        .store(std::sync::Arc::new(happyview::proxy_config::ProxyConfig {
            mode: happyview::proxy_config::ProxyMode::Allowlist,
            nsids: vec!["com.foo-.*".to_string()],
        }));

    // Re-submitting the exact same, already-stored pattern must not 400 —
    // it's grandfathered rather than revalidated.
    let resp = app
        .router
        .clone()
        .oneshot(admin_put(
            "/admin/settings/xrpc-proxy",
            app.admin_cookie(),
            &json!({
                "mode": "allowlist",
                "nsids": ["com.foo-.*"]
            }),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/settings/xrpc-proxy", app.admin_cookie()))
        .await
        .unwrap();
    let json = json_body(resp).await;
    assert_eq!(json["nsids"], json!(["com.foo-.*"]));
}

#[tokio::test]
#[serial]
async fn put_adding_new_invalid_pattern_alongside_stored_legacy_one_is_rejected() {
    common::require_db!();
    let app = TestApp::new().await;

    app.state
        .proxy_config
        .store(std::sync::Arc::new(happyview::proxy_config::ProxyConfig {
            mode: happyview::proxy_config::ProxyMode::Allowlist,
            nsids: vec!["com.foo-.*".to_string()],
        }));

    // The stored legacy pattern is grandfathered, but a brand-new invalid
    // pattern alongside it must still be rejected — the check is narrowed,
    // not removed.
    let resp = app
        .router
        .clone()
        .oneshot(admin_put(
            "/admin/settings/xrpc-proxy",
            app.admin_cookie(),
            &json!({
                "mode": "allowlist",
                "nsids": ["com.foo-.*", "*"]
            }),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[serial]
async fn requires_auth() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/settings/xrpc-proxy")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
