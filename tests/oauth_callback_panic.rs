//! A refused token exchange reaches a `todo!()` inside atrium-oauth's
//! `callback`. These drive both callback paths into it against a mock
//! authorization server, so removing the guard at either call site fails here.

mod common;

use atrium_common::store::Store;
use atrium_oauth::store::state::InternalStateData;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::app::TestApp;
use serial_test::serial;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// An authorization server whose token endpoint refuses every code.
async fn refusing_authorization_server() -> MockServer {
    let server = MockServer::start().await;
    let issuer = server.uri();
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{issuer}/oauth/authorize"),
            "token_endpoint": format!("{issuer}/oauth/token"),
            "pushed_authorization_request_endpoint": format!("{issuer}/oauth/par"),
            "scopes_supported": ["atproto"],
            "response_types_supported": ["code"],
            "token_endpoint_auth_methods_supported": ["none", "private_key_jwt"],
            "token_endpoint_auth_signing_alg_values_supported": ["ES256"],
            "dpop_signing_alg_values_supported": ["ES256"],
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant",
            "error_description": "Invalid code",
        })))
        .expect(1..)
        .mount(&server)
        .await;
    server
}

/// Stores the state atrium's `authorize` would have stored for `issuer`.
async fn store_oauth_state(app: &TestApp, state_key: &str, issuer: &str) {
    let private = happyview::oauth::client_keys::generate_client_key("test")
        .unwrap()
        .private_jwk;
    let jwk: jose_jwk::Jwk = serde_json::from_value(private).unwrap();
    app.state
        .oauth_state_store
        .set(
            state_key.to_string(),
            InternalStateData {
                iss: issuer.to_string(),
                dpop_key: jwk.key,
                verifier: "v".repeat(64),
                app_state: None,
            },
        )
        .await
        .unwrap();
}

fn callback_request(state: &str) -> Request<Body> {
    Request::builder()
        .uri(format!("/auth/callback?code=refused-code&state={state}"))
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
#[serial]
async fn a_refused_exchange_on_dashboard_login_is_a_502() {
    common::require_db!();
    let app = TestApp::new().await;
    let server = refusing_authorization_server().await;
    store_oauth_state(&app, "dashboard-state", &server.uri()).await;

    let resp = app
        .router
        .clone()
        .oneshot(callback_request("dashboard-state"))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    assert!(resp.headers().get("set-cookie").is_none());
}

#[tokio::test]
#[serial]
async fn a_refused_exchange_on_a_linked_repo_reports_failed() {
    common::require_db!();
    let app = TestApp::new().await;
    let server = refusing_authorization_server().await;

    let grant = happyview::linked_repos::db::create(
        &app.state,
        None,
        None,
        None,
        "atproto",
        "did:plc:admin",
    )
    .await
    .unwrap();
    let expires_at = (chrono::Utc::now() + chrono::Duration::minutes(10)).to_rfc3339();
    happyview::db::query(&happyview::db::adapt_sql(
        "INSERT INTO happyview_linked_repo_auth_state (state, grant_id, token_hash, expires_at) \
         VALUES (?, ?, NULL, ?)",
        app.state.db_backend,
    ))
    .bind("linked-state")
    .bind(&grant.id)
    .bind(&expires_at)
    .execute(&app.state.db)
    .await
    .unwrap();
    store_oauth_state(&app, "linked-state", &server.uri()).await;

    let resp = app
        .router
        .clone()
        .oneshot(callback_request("linked-state"))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(
        location.contains("/link/result?status=failed"),
        "got: {location}"
    );
}
