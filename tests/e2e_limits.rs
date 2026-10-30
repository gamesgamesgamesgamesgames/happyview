mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview_provider::{FixedLimits, Limits};
use serde_json::json;
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;

#[tokio::test]
#[serial]
async fn an_instance_with_no_limits_set_reports_every_one_as_unlimited() {
    common::require_db!();
    let app = TestApp::new().await;
    let body = app.get_json("/admin/limits").await;
    assert_eq!(body["seats"]["max"], json!(null));
    assert_eq!(body["api_clients"]["max"], json!(null));
    assert_eq!(body["log_retention_days"]["max"], json!(null));
    // The test's own admin is a seat.
    assert!(body["seats"]["used"].as_u64().unwrap() >= 1, "{body}");
    assert_eq!(body["log_retention_days"]["configured"], 30);
}

#[tokio::test]
#[serial]
async fn the_plans_limits_are_reported_with_what_is_in_use() {
    common::require_db!();
    let mut app = TestApp::new().await;
    app.state.limits = Arc::new(FixedLimits(Limits {
        seats: Some(5),
        api_clients: Some(3),
        log_retention_days: Some(7),
    }));
    app.rebuild_router();

    let before = app.get_json("/admin/limits").await;
    let seats = before["seats"]["used"].as_u64().unwrap();
    let clients = before["api_clients"]["used"].as_u64().unwrap();
    app.create_api_client("confidential", None).await;
    // The configured retention is reported as set, above the limit too:
    // capping it is the retention limit's own change.
    let cookie = app.admin_cookie();
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/admin/settings/event_log_retention_days")
                .header(cookie.0, cookie.1)
                .header("content-type", "application/json")
                .body(Body::from(json!({ "value": "90" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "setting refused: {}",
        resp.status()
    );

    let after = app.get_json("/admin/limits").await;
    assert_eq!(
        after,
        json!({
            "seats": { "max": 5, "used": seats },
            "api_clients": { "max": 3, "used": clients + 1 },
            "log_retention_days": { "max": 7, "configured": 90 },
        })
    );
}

#[tokio::test]
#[serial]
async fn the_limits_need_a_signed_in_admin() {
    common::require_db!();
    let app = TestApp::new().await;
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/limits")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
