//! The session cookie belongs to the instance's own dashboard. Outside
//! `/admin` it is the caller only on a request from the instance's own pages
//! (`csrf::cookie_request_is_same_origin`): `Sec-Fetch-Site: same-origin`, or
//! from a browser too old to send that, an `Origin` that is one of the
//! instance's domains. Any other request carrying it, including one with
//! neither header, runs as anonymous, so a query script never acts as the
//! operator for another site, an integration, or a script outside a browser.
//!
//! The query is answered by the echo interpreter, which hands back the
//! context the script was given, so each test reads the caller the script
//! would have run as. Requests go through the unmarked router, so what each
//! one carries is exactly what the test gives it.

mod common;

use axum::body::Body;
use axum::http::{Method, Request};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;
use common::echo_interpreter;
use happyview::csrf::INTEGRATION_ORIGINS_SETTING;

const QUERY: &str = "games.gamesgamesgamesgames.listGames";
const INTEGRATION: &str = "https://integration.example";
/// The instance's own origin in a test instance.
const INSTANCE: &str = "http://127.0.0.1:0";

/// An instance with a query whose script echoes what it was given.
async fn app() -> TestApp {
    assert!(echo_interpreter::is_built(), "{}", echo_interpreter::BUILD);
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .register(echo_interpreter::plugin("lua"))
        .await;
    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .method(Method::POST)
                .uri("/admin/lexicons")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "lexicon_json": common::fixtures::list_games_query_lexicon(),
                        "target_collection": "games.gamesgamesgamesgames.game",
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(resp.status().is_success(), "{}", resp.status());
    let now = happyview::db::now_rfc3339();
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_scripts (id, body, script_type, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(format!("xrpc.query:{QUERY}"))
        .bind("echo")
        .bind("lua")
        .bind(&now)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .expect("seed the query's script");
    app
}

/// A `GET` of `uri` with `cookie`, and the fetch metadata a browser sends.
fn get(
    uri: &str,
    cookie: (axum::http::HeaderName, axum::http::HeaderValue),
    fetch_site: Option<&str>,
    origin: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header("host", "127.0.0.1")
        .header(cookie.0, cookie.1);
    if let Some(site) = fetch_site {
        builder = builder.header("sec-fetch-site", site);
    }
    if let Some(origin) = origin {
        builder = builder.header("origin", origin);
    }
    builder.body(Body::empty()).unwrap()
}

async fn send(app: &TestApp, req: Request<Body>) -> (u16, Value) {
    let resp = app.unmarked_router().oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// The caller the query's script ran as, or `None` for anonymous.
async fn caller(app: &TestApp, req: Request<Body>) -> Option<String> {
    let (status, body) = send(app, req).await;
    assert_eq!(status, 200, "{body}");
    body["context"]["caller_did"].as_str().map(str::to_string)
}

fn query_uri() -> String {
    format!("/xrpc/{QUERY}?client_key=hvc_test")
}

#[tokio::test]
#[serial]
async fn a_request_from_another_site_runs_as_anonymous() {
    common::require_db!();
    let app = app().await;
    for site in ["cross-site", "same-site", "none"] {
        let req = get(&query_uri(), app.admin_cookie(), Some(site), None);
        assert_eq!(caller(&app, req).await, None, "Sec-Fetch-Site: {site}");
    }
    let req = get(
        &query_uri(),
        app.admin_cookie(),
        Some("cross-site"),
        Some("https://evil.example"),
    );
    assert_eq!(caller(&app, req).await, None);
    // The browser's word decides: an `Origin` cannot outvote it.
    let req = get(
        &query_uri(),
        app.admin_cookie(),
        Some("cross-site"),
        Some(INSTANCE),
    );
    assert_eq!(caller(&app, req).await, None);
}

/// A request with no fetch metadata did not come from the instance's pages:
/// a script or an integration outside a browser holding the cookie.
#[tokio::test]
#[serial]
async fn a_request_with_neither_header_runs_as_anonymous() {
    common::require_db!();
    let app = app().await;
    let req = get(&query_uri(), app.admin_cookie(), None, None);
    assert_eq!(caller(&app, req).await, None);
}

#[tokio::test]
#[serial]
async fn a_request_from_the_instances_own_pages_runs_as_its_caller() {
    common::require_db!();
    let app = app().await;
    let admin = Some(app.admin_did.clone());
    let req = get(&query_uri(), app.admin_cookie(), Some("same-origin"), None);
    assert_eq!(caller(&app, req).await, admin);
    // A browser too old to send `Sec-Fetch-Site` is judged by its `Origin`.
    let req = get(&query_uri(), app.admin_cookie(), None, Some(INSTANCE));
    assert_eq!(caller(&app, req).await, admin);
    let req = get(
        &query_uri(),
        app.admin_cookie(),
        None,
        Some("https://evil.example"),
    );
    assert_eq!(caller(&app, req).await, None);
}

/// A listed integration origin no longer makes the cookie count here.
#[tokio::test]
#[serial]
async fn a_listed_integration_origin_runs_as_anonymous() {
    common::require_db!();
    let app = app().await;
    let req = app
        .authed_request()
        .method("PUT")
        .uri(format!("/admin/settings/{INTEGRATION_ORIGINS_SETTING}"))
        .header("host", "127.0.0.1")
        .header("sec-fetch-site", "same-origin")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({ "value": INTEGRATION })).unwrap(),
        ))
        .unwrap();
    let (status, body) = send(&app, req).await;
    assert!((200..300).contains(&status), "{body}");

    let req = get(
        &query_uri(),
        app.admin_cookie(),
        Some("cross-site"),
        Some(INTEGRATION),
    );
    assert_eq!(caller(&app, req).await, None);
}

/// A cookie from an app's login carries its client key, so it needs nothing
/// in the URL; it is held to the same rule.
#[tokio::test]
#[serial]
async fn an_apps_login_cookie_is_held_to_the_same_rule() {
    common::require_db!();
    let app = app().await;
    let (client_key, _, _) = app.create_api_client("public", None).await;
    let cookie = || {
        happyview::testkit::app::session_cookie_header(
            &format!("{}\n{client_key}", app.admin_did),
            &app.state.cookie_key,
        )
    };
    let uri = format!("/xrpc/{QUERY}");
    assert_eq!(
        caller(&app, get(&uri, cookie(), Some("cross-site"), None)).await,
        None
    );
    assert_eq!(caller(&app, get(&uri, cookie(), None, None)).await, None);
    assert_eq!(
        caller(&app, get(&uri, cookie(), Some("same-origin"), None)).await,
        Some(app.admin_did.clone())
    );
}

/// The routes outside XRPC that take the cookie as their caller are held to
/// the same rule: the `dev.happyview.*` API-client methods, and linked
/// external accounts.
#[tokio::test]
#[serial]
async fn the_other_cookie_routes_outside_admin_are_held_to_the_same_rule() {
    common::require_db!();
    let app = app().await;
    for uri in [
        "/xrpc/dev.happyview.listApiClients?client_key=hvc_test",
        "/external-auth/accounts",
    ] {
        for (site, expected) in [
            (Some("cross-site"), 401),
            (None, 401),
            (Some("same-origin"), 200),
        ] {
            let (status, body) = send(&app, get(uri, app.admin_cookie(), site, None)).await;
            assert_eq!(
                status, expected,
                "{uri} with Sec-Fetch-Site {site:?}: {body}"
            );
        }
    }
}

/// What the dashboard itself calls with the cookie still answers it when the
/// browser marks the request as its own: the admin API, the session check,
/// linked external accounts and the setup status, and an XRPC query.
#[tokio::test]
#[serial]
async fn the_dashboards_own_requests_still_authenticate() {
    common::require_db!();
    let app = app().await;
    for uri in [
        "/admin/permissions",
        "/auth/me",
        "/external-auth/accounts",
        "/api/setup/status",
    ] {
        let (status, body) = send(
            &app,
            get(uri, app.admin_cookie(), Some("same-origin"), None),
        )
        .await;
        assert_eq!(status, 200, "{uri}: {body}");
    }
    let req = get(&query_uri(), app.admin_cookie(), Some("same-origin"), None);
    assert_eq!(caller(&app, req).await, Some(app.admin_did.clone()));
}
