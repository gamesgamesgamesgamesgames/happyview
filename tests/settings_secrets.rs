//! Keys the admin API must neither show nor let a generic route replace.

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use common::app::TestApp;
use happyview::db::adapt_sql;
use happyview::plugin::attestation::LEGACY_KEY_SETTING;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn call(
    app: &TestApp,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (name, cookie) = app.admin_cookie();
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header(name, cookie)
        .header("content-type", "application/json")
        .body(body.map_or(Body::empty(), |b| Body::from(b.to_string())))
        .unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn seed_reserved_setting(app: &TestApp) {
    happyview::admin::settings::put_setting(
        &app.state.db,
        LEGACY_KEY_SETTING,
        "left-behind",
        app.state.db_backend,
    )
    .await
    .unwrap();
}

/// Even a row the boot step hasn't moved stays out of the listing.
#[tokio::test]
async fn the_settings_list_leaves_out_a_reserved_key() {
    common::require_db!();
    let app = TestApp::new().await;
    seed_reserved_setting(&app).await;

    let (status, listed) = call(&app, Method::GET, "/admin/settings", None).await;

    assert_eq!(status, StatusCode::OK);
    assert!(!listed.to_string().contains("left-behind"));
    assert!(!listed.to_string().contains(LEGACY_KEY_SETTING));
}

#[tokio::test]
async fn the_settings_routes_refuse_a_reserved_key() {
    common::require_db!();
    let app = TestApp::new().await;
    seed_reserved_setting(&app).await;
    let path = format!("/admin/settings/{LEGACY_KEY_SETTING}");

    let (put, _) = call(
        &app,
        Method::PUT,
        &path,
        Some(json!({ "value": "replaced" })),
    )
    .await;
    let (delete, _) = call(&app, Method::DELETE, &path, None).await;

    assert_eq!(put, StatusCode::BAD_REQUEST);
    assert_eq!(delete, StatusCode::BAD_REQUEST);
    let value = happyview::admin::settings::get_setting(
        &app.state.db,
        LEGACY_KEY_SETTING,
        app.state.db_backend,
    )
    .await;
    assert_eq!(value.as_deref(), Some("left-behind"));
}

async fn stored_keys(app: &TestApp) -> (Option<String>, Option<String>) {
    let sql = adapt_sql(
        "SELECT signing_key_enc, rotation_key_enc FROM happyview_service_identity WHERE id = 1",
        app.state.db_backend,
    );
    happyview::db::query_as(&sql)
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

async fn did_plc_identity(app: &TestApp) {
    let (status, _) = call(
        app,
        Method::PUT,
        "/admin/service-identity",
        Some(json!({
            "mode": "did_plc",
            "did": "did:plc:identity",
            "signing_key_enc": "sealed-signing",
            "rotation_key_enc": "sealed-rotation",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn the_service_identity_response_has_no_key_material() {
    common::require_db!();
    let app = TestApp::new().await;
    did_plc_identity(&app).await;

    let (status, identity) = call(&app, Method::GET, "/admin/service-identity", None).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(identity["did"], "did:plc:identity");
    assert!(identity.get("signing_key_enc").is_none());
    assert!(!identity.to_string().contains("sealed"));
}

/// GET doesn't return the keys, so an update can't send them back, and it must
/// not wipe them.
#[tokio::test]
async fn an_update_within_a_mode_keeps_the_keys_it_omits() {
    common::require_db!();
    let app = TestApp::new().await;
    did_plc_identity(&app).await;

    let (status, _) = call(
        &app,
        Method::PUT,
        "/admin/service-identity",
        Some(json!({ "mode": "did_plc", "did": "did:plc:identity" })),
    )
    .await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        stored_keys(&app).await,
        (
            Some("sealed-signing".into()),
            Some("sealed-rotation".into())
        )
    );
}

/// The dashboard stops exposing an identity with `{ "mode": "not_exposed" }`,
/// and that has to keep clearing the old mode's keys.
#[tokio::test]
async fn a_change_of_mode_clears_the_keys_it_omits() {
    common::require_db!();
    let app = TestApp::new().await;
    did_plc_identity(&app).await;

    let (status, _) = call(
        &app,
        Method::PUT,
        "/admin/service-identity",
        Some(json!({ "mode": "not_exposed" })),
    )
    .await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(stored_keys(&app).await, (None, None));
}
