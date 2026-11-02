mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;

async fn send(app: &TestApp, method: &str) -> (StatusCode, Value) {
    let (name, value) = app.admin_cookie();
    let req = Request::builder()
        .method(method)
        .uri("/admin/encryption/rotation")
        .header(name, value)
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap_or(json!(null)))
}

/// With one key and nothing sealed under another, there is nothing to do,
/// and the previous key is never reported as safe to drop when there isn't
/// one.
#[tokio::test]
#[serial]
async fn the_status_reports_each_store_and_no_rotation_under_way() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;

    let (status, body) = send(&app, "GET").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["key_configured"], json!(true));
    assert_eq!(body["previous_key_configured"], json!(false));
    assert_eq!(body["previous_key_can_go"], json!(false));
    assert_eq!(body["remaining"], json!(0));
    let stores: Vec<&str> = body["stores"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["store"].as_str().unwrap())
        .collect();
    assert!(stores.contains(&"oauth_sessions"), "{stores:?}");
    assert!(stores.contains(&"plugin_secrets"), "{stores:?}");
}

/// Starting a re-seal twice answers the same job.
#[tokio::test]
#[serial]
async fn starting_a_reseal_twice_answers_the_same_job() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;

    let (status, first) = send(&app, "POST").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{first}");
    let (_, second) = send(&app, "POST").await;
    assert_eq!(first["job_id"], second["job_id"]);

    let (_, body) = send(&app, "GET").await;
    assert_eq!(body["job"]["id"], first["job_id"]);
}

/// Without a key there is nothing to re-seal with.
#[tokio::test]
#[serial]
async fn a_reseal_needs_a_key() {
    common::require_db!();
    let mut app = TestApp::new().await;
    app.state.config.token_encryption_key = None;
    app.rebuild_router();

    let (status, body) = send(&app, "GET").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["key_configured"], json!(false));
    let (status, _) = send(&app, "POST").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
