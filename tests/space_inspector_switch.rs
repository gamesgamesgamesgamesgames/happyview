//! The space inspector follows its setting: off unless a stored row, or the
//! environment with no row, says `true`.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::db::adapt_sql;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;

const KEY: &str = "feature.space_inspector_enabled";

async fn send(app: &TestApp, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let (name, value) = app.admin_cookie();
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header(name, value);
    let body = match body {
        Some(body) => {
            req = req.header("content-type", "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let resp = app
        .router
        .clone()
        .oneshot(req.body(body).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(json!(null)),
    )
}

/// The switch as every reader of it sees it: the status route, `/config`, and
/// whether the space listing opens.
async fn assert_inspector(app: &TestApp, on: bool) {
    let (_, body) = send(app, "GET", "/admin/spaces/inspector", None).await;
    assert_eq!(body["enabled"], on, "inspector status");
    let (_, body) = send(app, "GET", "/config", None).await;
    assert_eq!(body["features"]["space_inspector"], on, "/config");
    let (status, body) = send(app, "GET", "/admin/spaces", None).await;
    if on {
        assert_eq!(status, StatusCode::OK, "{body}");
    } else {
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"], "SpaceInspectorDisabled");
    }
}

async fn inspector_events(app: &TestApp) -> Vec<String> {
    let sql = adapt_sql(
        "SELECT event_type FROM happyview_event_logs WHERE event_type LIKE ? ORDER BY created_at",
        app.state.db_backend,
    );
    let rows: Vec<(String,)> = happyview::db::query_as(&sql)
        .bind("space_inspector.%")
        .fetch_all(&app.state.db)
        .await
        .unwrap();
    rows.into_iter().map(|(t,)| t).collect()
}

/// Restores `SPACE_INSPECTOR_ENABLED` to unset even when the test panics.
struct InspectorEnv;

impl Drop for InspectorEnv {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("SPACE_INSPECTOR_ENABLED") };
    }
}

#[tokio::test]
#[serial]
async fn the_inspector_is_off_by_default() {
    common::require_db!();
    let app = TestApp::new().await;
    let _env = InspectorEnv;
    unsafe { std::env::remove_var("SPACE_INSPECTOR_ENABLED") };

    assert_inspector(&app, false).await;
}

/// A row stored by any means, as a database that ran beta holds it.
#[tokio::test]
#[serial]
async fn a_stored_true_turns_the_inspector_on() {
    common::require_db!();
    let app = TestApp::new().await;
    let _env = InspectorEnv;
    unsafe { std::env::remove_var("SPACE_INSPECTOR_ENABLED") };
    let sql = adapt_sql(
        "INSERT INTO happyview_instance_settings (key, value, updated_at) VALUES (?, 'true', ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(KEY)
        .bind(happyview::db::now_rfc3339())
        .execute(&app.state.db)
        .await
        .unwrap();

    assert_inspector(&app, true).await;
}

#[tokio::test]
#[serial]
async fn the_environment_turns_the_inspector_on() {
    common::require_db!();
    let app = TestApp::new().await;
    let _env = InspectorEnv;
    unsafe { std::env::set_var("SPACE_INSPECTOR_ENABLED", "true") };

    assert_inspector(&app, true).await;
}

/// The settings API turns it on and off, and each change is audited.
#[tokio::test]
#[serial]
async fn the_settings_api_turns_the_inspector_on_and_audits_it() {
    common::require_db!();
    let app = TestApp::new().await;
    let _env = InspectorEnv;
    unsafe { std::env::remove_var("SPACE_INSPECTOR_ENABLED") };
    let uri = format!("/admin/settings/{KEY}");

    let (status, _) = send(&app, "PUT", &uri, Some(json!({ "value": "true" }))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_inspector(&app, true).await;
    let (status, _) = send(&app, "PUT", &uri, Some(json!({ "value": "false" }))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_inspector(&app, false).await;

    assert_eq!(
        inspector_events(&app).await,
        ["space_inspector.enabled", "space_inspector.disabled"]
    );
}
