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

fn admin_delete(
    uri: &str,
    cookie: (axum::http::HeaderName, axum::http::HeaderValue),
) -> Request<Body> {
    Request::builder()
        .method(Method::DELETE)
        .uri(uri)
        .header(cookie.0, cookie.1)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
#[serial]
async fn space_routes_blocked_when_flag_disabled() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/xrpc/com.atproto.space.listSpaces")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = json_body(resp).await;
    assert_eq!(body["error"], "FeatureDisabled");
}

#[tokio::test]
#[serial]
async fn space_routes_allowed_after_enabling_flag() {
    common::require_db!();
    let app = TestApp::new().await;

    // Enable the feature flag
    let resp = app
        .router
        .clone()
        .oneshot(admin_put(
            "/admin/settings/feature.spaces_enabled",
            app.admin_cookie(),
            &json!({ "value": "true" }),
        ))
        .await
        .unwrap();
    assert!(resp.status().is_success());

    // Space routes should now pass through (will get auth error, not FeatureDisabled)
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/xrpc/com.atproto.space.listSpaces")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = json_body(resp).await;
    assert_ne!(
        body["error"].as_str().unwrap_or(""),
        "FeatureDisabled",
        "expected request to pass through feature gate"
    );
}

#[tokio::test]
#[serial]
async fn space_routes_blocked_again_after_disabling_flag() {
    common::require_db!();
    let app = TestApp::new().await;

    // Enable
    let resp = app
        .router
        .clone()
        .oneshot(admin_put(
            "/admin/settings/feature.spaces_enabled",
            app.admin_cookie(),
            &json!({ "value": "true" }),
        ))
        .await
        .unwrap();
    assert!(resp.status().is_success());

    // Disable
    let resp = app
        .router
        .clone()
        .oneshot(admin_delete(
            "/admin/settings/feature.spaces_enabled",
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert!(resp.status().is_success());

    // Space routes should be blocked again
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/xrpc/com.atproto.space.listSpaces")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = json_body(resp).await;
    assert_eq!(body["error"], "FeatureDisabled");
}

#[tokio::test]
#[serial]
async fn admin_feature_flags_lists_flags() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/feature-flags", app.admin_cookie()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    let flags = body.as_array().expect("expected array");
    assert!(!flags.is_empty());

    let spaces_flag = flags
        .iter()
        .find(|f| f["key"] == "feature.spaces_enabled")
        .expect("spaces flag not found");
    assert_eq!(spaces_flag["enabled"], false);
    assert!(spaces_flag["name"].as_str().is_some());
    assert!(spaces_flag["description"].as_str().is_some());
}

#[tokio::test]
#[serial]
async fn admin_feature_flags_reflects_enabled_state() {
    common::require_db!();
    let app = TestApp::new().await;

    // Enable the flag
    let resp = app
        .router
        .clone()
        .oneshot(admin_put(
            "/admin/settings/feature.spaces_enabled",
            app.admin_cookie(),
            &json!({ "value": "true" }),
        ))
        .await
        .unwrap();
    assert!(resp.status().is_success());

    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/feature-flags", app.admin_cookie()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    let flags = body.as_array().unwrap();
    let spaces_flag = flags
        .iter()
        .find(|f| f["key"] == "feature.spaces_enabled")
        .unwrap();
    assert_eq!(spaces_flag["enabled"], true);
}

#[tokio::test]
#[serial]
async fn config_endpoint_includes_features() {
    common::require_db!();
    let app = TestApp::new().await;

    // Default: spaces disabled
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/config")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["features"]["spaces"], false);

    // Enable the flag
    let resp = app
        .router
        .clone()
        .oneshot(admin_put(
            "/admin/settings/feature.spaces_enabled",
            app.admin_cookie(),
            &json!({ "value": "true" }),
        ))
        .await
        .unwrap();
    assert!(resp.status().is_success());

    // Now config should reflect enabled
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/config")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["features"]["spaces"], true);
}

#[tokio::test]
#[serial]
async fn admin_feature_flags_requires_auth() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/feature-flags")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
