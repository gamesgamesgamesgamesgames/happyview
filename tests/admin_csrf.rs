//! Cross-site request forgery against cookie-authenticated admin routes.
//!
//! On an https instance the dashboard's session cookie is `SameSite=None`
//! (`auth::routes::callback`), so a browser sends it with a form another site
//! submits. CORS keeps that site from reading the response, not the request
//! from running, and a form can POST without a preflight to any route that
//! reads no JSON body. `/admin/settings/telemetry/dismiss` is one: it needs
//! `settings:manage` and changes a setting.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::app::TestApp;
use serial_test::serial;
use tower::ServiceExt;

const ROUTE: &str = "/admin/settings/telemetry/dismiss";

/// A form POST as a browser sends it, carrying the admin's session cookie.
fn form_post(app: &TestApp, origin: &str, fetch_site: &str) -> Request<Body> {
    let cookie = app.admin_cookie();
    Request::builder()
        .method("POST")
        .uri(ROUTE)
        .header("host", "127.0.0.1")
        .header(cookie.0, cookie.1)
        .header("origin", origin)
        .header("sec-fetch-site", fetch_site)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::empty())
        .unwrap()
}

/// The dashboard's own request, which any fix has to keep working.
#[tokio::test]
#[serial]
async fn a_same_origin_form_post_with_the_session_cookie_runs() {
    common::require_db!();
    let app = TestApp::new().await;
    let resp = app
        .router
        .clone()
        .oneshot(form_post(&app, "http://127.0.0.1", "same-origin"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// What a fix has to make true. It fails today: the route answers 200 and the
/// setting changes.
#[tokio::test]
#[serial]
#[ignore = "CSRF: a cross-site form POST still runs on cookie-authenticated admin routes"]
async fn a_cross_site_form_post_with_the_session_cookie_is_refused() {
    common::require_db!();
    let app = TestApp::new().await;
    let resp = app
        .router
        .clone()
        .oneshot(form_post(&app, "https://evil.example", "cross-site"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}
