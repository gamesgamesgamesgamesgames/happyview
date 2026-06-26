mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;

async fn json_body(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

// ---------------------------------------------------------------------------
// GET /admin/service-identity
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn get_identity_returns_null_when_not_configured() {
    common::require_db!();
    let app = TestApp::new().await;
    let cookie = app.admin_cookie();

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/service-identity")
                .header(cookie.0, cookie.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert!(body.is_null(), "expected null when no identity configured");
}

#[tokio::test]
#[serial]
async fn get_identity_returns_identity_after_setup() {
    common::require_db!();
    let mut app = TestApp::new().await;
    app.setup_did_web().await;
    let cookie = app.admin_cookie();

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/service-identity")
                .header(cookie.0, cookie.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["mode"], "did_web");
    assert!(
        body["did"].is_null(),
        "did:web derives DID from host, not stored"
    );
    assert_eq!(body["setup_complete"], true);
}

// ---------------------------------------------------------------------------
// PUT /admin/service-identity
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn update_identity_changes_mode() {
    common::require_db!();
    let app = TestApp::new().await;
    let cookie = app.admin_cookie();

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/admin/service-identity")
                .header(cookie.0.clone(), cookie.1.clone())
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "mode": "not_exposed"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Verify the mode was persisted
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/service-identity")
                .header(cookie.0, cookie.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = json_body(resp).await;
    assert_eq!(body["mode"], "not_exposed");
}

#[tokio::test]
#[serial]
async fn update_identity_rejects_invalid_mode() {
    common::require_db!();
    let app = TestApp::new().await;
    let cookie = app.admin_cookie();

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/admin/service-identity")
                .header(cookie.0, cookie.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "mode": "invalid_mode"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[serial]
async fn get_identity_requires_auth() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/service-identity")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[serial]
async fn update_identity_requires_auth() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/admin/service-identity")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "mode": "not_exposed"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
