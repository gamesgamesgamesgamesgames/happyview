mod common;

use axum::body::Body;
use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;

async fn put(app: &TestApp, path: &str, body: Value) -> StatusCode {
    let req = app
        .authed_request()
        .method(Method::PUT)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    app.router.clone().oneshot(req).await.unwrap().status()
}

async fn delete(app: &TestApp, path: &str) -> StatusCode {
    let req = app
        .authed_request()
        .method(Method::DELETE)
        .uri(path)
        .body(Body::empty())
        .unwrap();
    app.router.clone().oneshot(req).await.unwrap().status()
}

#[tokio::test]
#[serial]
async fn a_platform_owns_the_telemetry_settings_of_an_instance_it_manages() {
    let mut app = TestApp::new().await;

    // What the tenant chose before the platform took over.
    assert_eq!(
        put(
            &app,
            "/admin/settings/telemetry",
            json!({"mode": "off", "contact": "operator@example.com", "lexicon_names": true}),
        )
        .await,
        StatusCode::OK
    );

    app.state.config.telemetry_managed_by = Some("acme".into());
    app.rebuild_router();

    let settings = app.get_json("/admin/settings/telemetry").await;
    assert_eq!(settings["managed"], true);
    assert_eq!(settings["mode"], "auto");
    assert_eq!(settings["contact"], Value::Null);
    assert_eq!(settings["lexicon_names"], false);
    assert_eq!(settings["prompted"], true);

    assert_eq!(
        put(&app, "/admin/settings/telemetry", json!({"mode": "off"})).await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        put(
            &app,
            "/admin/settings/telemetry.mode",
            json!({"value": "off"})
        )
        .await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        delete(&app, "/admin/settings/telemetry.contact").await,
        StatusCode::CONFLICT
    );
    // Other settings stay the operator's.
    assert_eq!(
        put(
            &app,
            "/admin/settings/app_name",
            json!({"value": "Example"})
        )
        .await,
        StatusCode::NO_CONTENT
    );

    let preview = app.get_json("/admin/settings/telemetry/preview").await;
    assert_eq!(preview["managed"], "acme");
    assert_eq!(preview["report_mode"], "auto");
    assert!(preview.get("contact").is_none(), "{preview}");
    assert!(preview["lexicons"].get("names").is_none(), "{preview}");

    // The stored choices were never touched, so they apply again once the
    // platform lets go.
    app.state.config.telemetry_managed_by = None;
    app.rebuild_router();

    let settings = app.get_json("/admin/settings/telemetry").await;
    assert_eq!(settings["managed"], false);
    assert_eq!(settings["mode"], "off");
    assert_eq!(settings["contact"], "operator@example.com");
    assert_eq!(
        put(&app, "/admin/settings/telemetry", json!({"mode": "manual"})).await,
        StatusCode::OK
    );
}
