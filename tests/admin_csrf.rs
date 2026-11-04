//! Cross-site request forgery against cookie-authenticated routes.
//!
//! The dashboard's session cookie is `SameSite=Lax`, so a browser still sends
//! it with a request from a sibling on the same site, and an older browser may
//! not honour the attribute. CORS keeps a page from reading the response, not
//! the request from running, and a form can POST without a preflight to any
//! route that reads no JSON body. `/admin/settings/telemetry/dismiss` is one:
//! it needs `settings:manage` and changes a setting.

mod common;

use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Request, StatusCode};
use common::app::TestApp;
use serial_test::serial;
use tower::ServiceExt;

const ROUTE: &str = "/admin/settings/telemetry/dismiss";
const INSTANCE: &str = "http://127.0.0.1:0";
const INTEGRATION: &str = "https://pentaract.example";
/// The setting that once listed integration origins. It is gone, and a row
/// left behind by it must grant nothing.
const REMOVED_SETTING: &str = "cookie_integration_origins";

/// A form POST as a browser sends it, carrying `cookie`, with `Origin` and
/// `Sec-Fetch-Site` when given.
fn form_post(
    cookie: (HeaderName, HeaderValue),
    origin: Option<&str>,
    fetch_site: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(ROUTE)
        .header("host", "127.0.0.1")
        .header(cookie.0, cookie.1)
        .header("content-type", "application/x-www-form-urlencoded");
    if let Some(origin) = origin {
        builder = builder.header("origin", origin);
    }
    if let Some(fetch_site) = fetch_site {
        builder = builder.header("sec-fetch-site", fetch_site);
    }
    builder.body(Body::empty()).unwrap()
}

async fn status(app: &TestApp, req: Request<Body>) -> StatusCode {
    app.unmarked_router().oneshot(req).await.unwrap().status()
}

async fn put_setting(app: &TestApp, key: &str, value: &str) -> StatusCode {
    let req = app
        .authed_request()
        .method("PUT")
        .uri(format!("/admin/settings/{key}"))
        .header("host", "127.0.0.1")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({ "value": value })).unwrap(),
        ))
        .unwrap();
    status(app, req).await
}

/// The dashboard's own request.
#[tokio::test]
#[serial]
async fn a_same_origin_form_post_with_the_session_cookie_runs() {
    common::require_db!();
    let app = TestApp::new().await;
    let req = form_post(app.admin_cookie(), Some(INSTANCE), Some("same-origin"));
    assert_eq!(status(&app, req).await, StatusCode::OK);
}

#[tokio::test]
#[serial]
async fn a_cross_site_form_post_with_the_session_cookie_is_refused() {
    common::require_db!();
    let app = TestApp::new().await;
    let req = form_post(
        app.admin_cookie(),
        Some("https://evil.example"),
        Some("cross-site"),
    );
    assert_eq!(status(&app, req).await, StatusCode::FORBIDDEN);

    // A browser that sends `Origin` but not `Sec-Fetch-Site` is judged by the
    // origin alone, and one that sends `Sec-Fetch-Site` but no `Origin` is
    // refused.
    let req = form_post(app.admin_cookie(), Some("https://evil.example"), None);
    assert_eq!(status(&app, req).await, StatusCode::FORBIDDEN);
    let req = form_post(app.admin_cookie(), None, Some("cross-site"));
    assert_eq!(status(&app, req).await, StatusCode::FORBIDDEN);
}

/// A request with neither header did not come from a browser: there is no
/// ambient cookie to abuse, and scripts and curl keep working.
#[tokio::test]
#[serial]
async fn a_request_from_outside_a_browser_runs() {
    common::require_db!();
    let app = TestApp::new().await;
    let req = form_post(app.admin_cookie(), None, None);
    assert_eq!(status(&app, req).await, StatusCode::OK);
}

/// `same-site` is a sibling subdomain, not the instance, so it is judged by
/// its origin: one of the instance's own domains runs, and a sibling that is
/// not one is refused.
#[tokio::test]
#[serial]
async fn an_origin_among_the_instances_domains_runs() {
    common::require_db!();
    let app = TestApp::new().await;
    let req = form_post(app.admin_cookie(), Some(INSTANCE), Some("same-site"));
    assert_eq!(status(&app, req).await, StatusCode::OK);
    let req = form_post(
        app.admin_cookie(),
        Some("http://other.127.0.0.1:0"),
        Some("same-site"),
    );
    assert_eq!(status(&app, req).await, StatusCode::FORBIDDEN);
}

/// Only the instance's own pages may use the cookie on `/admin`: an
/// integration on another origin authenticates with OAuth or an API key.
#[tokio::test]
#[serial]
async fn an_origin_outside_the_instances_domains_is_refused() {
    common::require_db!();
    let app = TestApp::new().await;
    let req = form_post(app.admin_cookie(), Some(INTEGRATION), Some("cross-site"));
    assert_eq!(status(&app, req).await, StatusCode::FORBIDDEN);
    let req = form_post(app.admin_cookie(), Some(INTEGRATION), None);
    assert_eq!(status(&app, req).await, StatusCode::FORBIDDEN);
}

/// A `cookie_integration_origins` row left from before the setting was
/// removed lists an origin that is still refused, and still gets no CORS
/// credentials.
#[tokio::test]
#[serial]
async fn a_leftover_integration_origins_setting_grants_nothing() {
    common::require_db!();
    let app = TestApp::new().await;
    assert_eq!(
        put_setting(&app, REMOVED_SETTING, INTEGRATION).await,
        StatusCode::NO_CONTENT
    );
    let req = form_post(app.admin_cookie(), Some(INTEGRATION), Some("cross-site"));
    let resp = app.unmarked_router().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(resp.headers().get("access-control-allow-credentials"), None);
}

/// CORS allows credentials to the instance's own domains and no one else.
#[tokio::test]
#[serial]
async fn only_the_instances_own_domains_are_allowed_credentials() {
    common::require_db!();
    let app = TestApp::new().await;
    let preflight = |origin: &str| {
        Request::builder()
            .method("OPTIONS")
            .uri(ROUTE)
            .header("host", "127.0.0.1")
            .header("origin", origin)
            .header("access-control-request-method", "POST")
            .header("access-control-request-headers", "content-type")
            .body(Body::empty())
            .unwrap()
    };
    let credentials = |resp: axum::response::Response| {
        resp.headers()
            .get("access-control-allow-credentials")
            .map(|v| v.to_str().unwrap().to_string())
    };
    let resp = app
        .router
        .clone()
        .oneshot(preflight(INTEGRATION))
        .await
        .unwrap();
    assert_eq!(credentials(resp), None);
    let resp = app
        .router
        .clone()
        .oneshot(preflight(INSTANCE))
        .await
        .unwrap();
    assert_eq!(credentials(resp).as_deref(), Some("true"));
}

/// An opaque origin (a sandboxed frame, a `data:` page) names no site, so it
/// is never the instance.
#[tokio::test]
#[serial]
async fn an_opaque_origin_is_refused() {
    common::require_db!();
    let app = TestApp::new().await;
    let req = form_post(app.admin_cookie(), Some("null"), Some("cross-site"));
    assert_eq!(status(&app, req).await, StatusCode::FORBIDDEN);
    let req = form_post(app.admin_cookie(), Some("null"), None);
    assert_eq!(status(&app, req).await, StatusCode::FORBIDDEN);
}

/// The cookie an app's `/auth/login` flow sets carries the app's client key
/// beside the DID, and is the same ambient credential.
#[tokio::test]
#[serial]
async fn an_apps_login_cookie_is_held_to_the_same_rule() {
    common::require_db!();
    let app = TestApp::new().await;
    let (client_key, _, _) = app.create_api_client("public", None).await;
    let cookie = || {
        happyview::testkit::app::session_cookie_header(
            &format!("{}\n{client_key}", app.admin_did),
            &app.state.cookie_key,
        )
    };

    let req = form_post(cookie(), Some("https://evil.example"), Some("cross-site"));
    assert_eq!(status(&app, req).await, StatusCode::FORBIDDEN);
    let req = form_post(cookie(), Some(INSTANCE), Some("same-origin"));
    assert_eq!(status(&app, req).await, StatusCode::OK);
}

/// A request without the session cookie is not this guard's to judge.
#[tokio::test]
#[serial]
async fn a_cross_site_request_without_the_cookie_reaches_authentication() {
    common::require_db!();
    let app = TestApp::new().await;
    let req = Request::builder()
        .method("POST")
        .uri(ROUTE)
        .header("host", "127.0.0.1")
        .header("origin", "https://evil.example")
        .header("sec-fetch-site", "cross-site")
        .body(Body::empty())
        .unwrap();
    assert_eq!(status(&app, req).await, StatusCode::UNAUTHORIZED);
}
