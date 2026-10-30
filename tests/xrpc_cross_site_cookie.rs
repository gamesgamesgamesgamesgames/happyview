//! A request another site starts carries the session cookie, because on
//! https it is `SameSite=None`, and the CSRF guard lets every `GET` through.
//! On XRPC that cookie is the caller only when the request is first-party
//! (`csrf::cookie_request_is_first_party`); otherwise the request runs as
//! anonymous, so a query script never acts as the operator for a page on
//! another site.
//!
//! The query is answered by the echo interpreter, which hands back the
//! context the script was given, so each test reads the caller the script
//! would have run as.

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
    let resp = app.router.clone().oneshot(req).await.unwrap();
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
async fn a_cross_site_get_with_the_cookie_runs_as_anonymous() {
    common::require_db!();
    let app = app().await;
    for site in ["cross-site", "same-site"] {
        let req = get(&query_uri(), app.admin_cookie(), Some(site), None);
        assert_eq!(caller(&app, req).await, None, "Sec-Fetch-Site: {site}");
    }
    // An `Origin` that is not the instance's does not make it first-party.
    let req = get(
        &query_uri(),
        app.admin_cookie(),
        Some("cross-site"),
        Some("https://evil.example"),
    );
    assert_eq!(caller(&app, req).await, None);
}

#[tokio::test]
#[serial]
async fn a_first_party_get_with_the_cookie_runs_as_its_caller() {
    common::require_db!();
    let app = app().await;
    let admin = Some(app.admin_did.clone());
    for site in [Some("same-origin"), Some("none"), None] {
        let req = get(&query_uri(), app.admin_cookie(), site, None);
        assert_eq!(caller(&app, req).await, admin, "Sec-Fetch-Site: {site:?}");
    }
}

/// An integration an admin listed is trusted as #243 trusts it.
#[tokio::test]
#[serial]
async fn a_listed_integration_origin_runs_as_its_caller() {
    common::require_db!();
    let app = app().await;
    let req = app
        .authed_request()
        .method("PUT")
        .uri(format!("/admin/settings/{INTEGRATION_ORIGINS_SETTING}"))
        .header("host", "127.0.0.1")
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
    assert_eq!(caller(&app, req).await, Some(app.admin_did.clone()));
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
    assert_eq!(
        caller(&app, get(&uri, cookie(), Some("same-origin"), None)).await,
        Some(app.admin_did.clone())
    );
}

/// The `dev.happyview.*` methods read their caller from the same place, so a
/// cross-site cookie is no caller there either.
#[tokio::test]
#[serial]
async fn a_cross_site_cookie_is_no_caller_for_the_api_client_methods() {
    common::require_db!();
    let app = app().await;
    let uri = "/xrpc/dev.happyview.listApiClients?client_key=hvc_test";
    let (status, body) = send(&app, get(uri, app.admin_cookie(), Some("cross-site"), None)).await;
    assert_eq!(status, 401, "{body}");
    let (status, body) = send(
        &app,
        get(uri, app.admin_cookie(), Some("same-origin"), None),
    )
    .await;
    assert_eq!(status, 200, "{body}");
}
