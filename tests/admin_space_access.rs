mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::db::adapt_sql;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;

async fn json_body(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap_or(json!(null))
}

async fn send(
    app: &TestApp,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> axum::response::Response {
    let (name, value) = app.admin_cookie();
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header(name, value);
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    app.router
        .clone()
        .oneshot(req.body(body).unwrap())
        .await
        .unwrap()
}

async fn put_setting(app: &TestApp, key: &str, value: &str) {
    let resp = send(
        app,
        "PUT",
        &format!("/admin/settings/{key}"),
        Some(json!({ "value": value })),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

async fn events_of(app: &TestApp, event_type: &str) -> Vec<(Option<String>, String)> {
    let sql = adapt_sql(
        "SELECT subject, detail FROM happyview_event_logs WHERE event_type = ? ORDER BY created_at",
        app.state.db_backend,
    );
    happyview::db::query_as(&sql)
        .bind(event_type)
        .fetch_all(&app.state.db)
        .await
        .unwrap()
}

#[tokio::test]
#[serial]
async fn inspector_is_off_by_default() {
    common::require_db!();
    let app = TestApp::new().await;
    let resp = send(&app, "GET", "/admin/spaces/inspector", None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["enabled"], false);
    assert_eq!(body["default_grant_minutes"], 60);
    assert_eq!(body["max_grant_minutes"], 60);
}

#[tokio::test]
#[serial]
async fn toggling_the_inspector_logs_each_change() {
    common::require_db!();
    let app = TestApp::new().await;
    put_setting(&app, "feature.space_inspector_enabled", "true").await;
    put_setting(&app, "feature.space_inspector_enabled", "false").await;
    assert_eq!(events_of(&app, "space_inspector.enabled").await.len(), 1);
    assert_eq!(events_of(&app, "space_inspector.disabled").await.len(), 1);
}

#[tokio::test]
#[serial]
async fn saving_the_same_value_twice_logs_once() {
    common::require_db!();
    let app = TestApp::new().await;
    put_setting(&app, "feature.space_inspector_enabled", "true").await;
    put_setting(&app, "feature.space_inspector_enabled", "true").await;
    put_setting(&app, "event_log_retention_days", "14").await;
    put_setting(&app, "event_log_retention_days", "14").await;
    assert_eq!(events_of(&app, "space_inspector.enabled").await.len(), 1);
    let changes = events_of(&app, "event_logs.retention_changed").await;
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].0.as_deref(), Some("event_log_retention_days"));
    let detail: Value = serde_json::from_str(&changes[0].1).unwrap();
    assert_eq!(detail["to"], "14");
}

#[tokio::test]
#[serial]
async fn deleting_the_setting_counts_as_disabling() {
    common::require_db!();
    let app = TestApp::new().await;
    put_setting(&app, "feature.space_inspector_enabled", "true").await;
    let resp = send(
        &app,
        "DELETE",
        "/admin/settings/feature.space_inspector_enabled",
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(events_of(&app, "space_inspector.disabled").await.len(), 1);
}
