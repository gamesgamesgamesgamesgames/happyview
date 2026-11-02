//! The space inspector stays off in this build whatever its setting says.
//!
//! Its own target, because the other inspector tests let the process's
//! inspector follow its setting, and that cannot be undone within a process.
//! Nothing here calls `common::let_space_inspector_follow_its_setting`.

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

async fn get(app: &TestApp, uri: &str) -> (StatusCode, Value) {
    let (name, value) = app.admin_cookie();
    let req = Request::builder()
        .uri(uri)
        .header(name, value)
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap_or(json!(null)))
}

async fn stored_row(app: &TestApp) -> Option<(String, String)> {
    let sql = adapt_sql(
        "SELECT value, updated_at FROM happyview_instance_settings WHERE key = ?",
        app.state.db_backend,
    );
    happyview::db::query_as(&sql)
        .bind(KEY)
        .fetch_optional(&app.state.db)
        .await
        .unwrap()
}

/// Every route a stored `true` would open, and `/config`, read off.
async fn assert_held_off(app: &TestApp) {
    let space = "00000000-0000-4000-8000-000000000000";
    for uri in [
        "/admin/spaces".to_string(),
        format!("/admin/spaces/{space}"),
        format!("/admin/spaces/{space}/records"),
        format!("/admin/spaces/{space}/blob?cid=bafkreiheldoff"),
        "/admin/accounts/did:plc:heldoff/spaces".to_string(),
        "/admin/accounts/did:plc:heldoff/space-records".to_string(),
    ] {
        let (status, body) = get(app, &uri).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
        assert_eq!(body["error"], "SpaceInspectorDisabled", "{uri}");
    }
    let (_, body) = get(app, "/admin/spaces/inspector").await;
    assert_eq!(body["enabled"], false);
    let (_, body) = get(app, "/config").await;
    assert_eq!(body["features"]["space_inspector"], false);
}

/// A stored `true`, as a database that ran a build with the inspector's
/// dashboard would hold, reads as off and is left as it is, so it takes
/// effect once the dashboard is available.
#[tokio::test]
#[serial]
async fn a_stored_true_is_held_off_and_kept() {
    common::require_db!();
    let app = TestApp::new().await;
    let sql = adapt_sql(
        "INSERT INTO happyview_instance_settings (key, value, updated_at) VALUES (?, 'true', ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(KEY)
        .bind("2026-10-07T00:00:00+00:00")
        .execute(&app.state.db)
        .await
        .unwrap();

    assert_held_off(&app).await;
    assert_eq!(
        stored_row(&app).await,
        Some(("true".to_string(), "2026-10-07T00:00:00+00:00".to_string()))
    );
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
async fn the_environment_does_not_turn_the_inspector_on() {
    common::require_db!();
    let app = TestApp::new().await;
    let _env = InspectorEnv;
    unsafe { std::env::set_var("SPACE_INSPECTOR_ENABLED", "true") };

    assert_held_off(&app).await;
    assert_eq!(stored_row(&app).await, None);
}
