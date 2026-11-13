//! Registering a DPoP session confirms its scopes with the user's
//! authorization server: anything beyond `atproto` must have been granted.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::app::TestApp;

const DID: &str = "did:plc:scopegrantuser";
const SPACE: &str = "space:com.example.conformance.space?authority=*&manage=create&manage=update&manage=delete&collection=*";

async fn response_json(resp: axum::http::Response<Body>) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

/// An authorization server whose refresh reports `granted` for `DID`, listed by
/// the DID's PDS when `listed`.
async fn authorization_server(app: &TestApp, granted: &str, listed: bool) -> MockServer {
    let server = MockServer::start().await;
    let issuer = server.uri();
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": issuer,
            "token_endpoint": format!("{issuer}/oauth/token"),
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "refreshed-access-token",
            "refresh_token": "refreshed-refresh-token",
            "token_type": "DPoP",
            "expires_in": 3600,
            "sub": DID,
            "scope": granted,
        })))
        .mount(&server)
        .await;

    app.mock_session_verification(DID, DID).await;
    let named = if listed {
        issuer.clone()
    } else {
        "https://elsewhere.example".to_string()
    };
    Mock::given(method("GET"))
        .and(path(format!(
            "/pds/{DID}/.well-known/oauth-protected-resource"
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "authorization_servers": [named] })),
        )
        .mount(&app.mock_server)
        .await;
    server
}

/// Provision a key for a confidential client allowed `client_scopes`, then
/// register a session asking for `scopes`. Answers the status and the body.
async fn register(
    app: &TestApp,
    server: &MockServer,
    client_scopes: &str,
    scopes: &str,
    refresh_token: Option<&str>,
) -> (StatusCode, Value) {
    register_with_issuer(
        app,
        client_scopes,
        scopes,
        refresh_token,
        Some(server.uri()),
    )
    .await
}

/// [`register`] naming `issuer`, or no issuer at all.
async fn register_with_issuer(
    app: &TestApp,
    client_scopes: &str,
    scopes: &str,
    refresh_token: Option<&str>,
    issuer: Option<String>,
) -> (StatusCode, Value) {
    let (client_key, client_secret, client_id) = app.create_api_client("confidential", None).await;
    let sql = happyview::db::adapt_sql(
        "UPDATE happyview_api_clients SET scopes = ? WHERE id = ?",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(client_scopes)
        .bind(&client_id)
        .execute(&app.state.db)
        .await
        .unwrap();

    let req = Request::builder()
        .method("POST")
        .uri("/oauth/dpop-keys")
        .header("content-type", "application/json")
        .header("x-client-key", &client_key)
        .header("x-client-secret", &client_secret)
        .body(Body::from("{}"))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let provision = response_json(resp).await;

    let mut body = json!({
        "provision_id": provision["provision_id"],
        "did": DID,
        "access_token": "app-access-token",
        "expires_at": (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
        "scopes": scopes,
    });
    if let Some(issuer) = issuer {
        body["issuer"] = json!(issuer);
    }
    if let Some(token) = refresh_token {
        body["refresh_token"] = json!(token);
    }
    let req = Request::builder()
        .method("POST")
        .uri("/oauth/sessions")
        .header("content-type", "application/json")
        .header("x-client-key", &client_key)
        .header("x-client-secret", &client_secret)
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    (status, response_json(resp).await)
}

async fn token_requests(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/oauth/token")
        .count()
}

async fn stored_sessions(app: &TestApp) -> i64 {
    let sql = happyview::db::adapt_sql(
        "SELECT COUNT(*) FROM happyview_dpop_sessions WHERE user_did = ?",
        app.state.db_backend,
    );
    let (n,): (i64,) = happyview::db::query_as(&sql)
        .bind(DID)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    n
}

/// The user consented to `atproto` only; the app asks for a space scope. The
/// registration is refused, naming the scope, and no session is kept.
#[tokio::test]
#[serial]
async fn a_scope_the_server_did_not_grant_is_refused() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let server = authorization_server(&app, "atproto", true).await;
    let requested = format!("atproto {SPACE}");

    let (status, body) = register(&app, &server, &requested, &requested, Some("rt")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let error = body["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("did not grant") && error.contains(SPACE),
        "{body}"
    );
    assert_eq!(token_requests(&server).await, 1);
    assert_eq!(stored_sessions(&app).await, 0);
}

#[tokio::test]
#[serial]
async fn a_scope_the_server_granted_is_registered() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let requested = format!("atproto {SPACE}");
    let server = authorization_server(&app, &requested, true).await;

    let (status, body) = register(&app, &server, &requested, &requested, Some("rt")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let scopes: Vec<&str> = body["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(scopes.contains(&SPACE), "{body}");

    let sql = happyview::db::adapt_sql(
        "SELECT scopes FROM happyview_dpop_sessions WHERE user_did = ?",
        app.state.db_backend,
    );
    let (stored,): (String,) = happyview::db::query_as(&sql)
        .bind(DID)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(stored, requested);
}

/// Without a refresh token there is nothing to confirm with: only `atproto`.
#[tokio::test]
#[serial]
async fn a_session_without_a_refresh_token_holds_only_atproto() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let requested = format!("atproto {SPACE}");
    let server = authorization_server(&app, &requested, true).await;

    let (status, body) = register(&app, &server, &requested, &requested, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("without a refresh token"),
        "{body}"
    );

    let (status, body) = register(&app, &server, &requested, "atproto", None).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(token_requests(&server).await, 0);
}

/// The grant is asked only of an issuer the user's PDS names: one it doesn't
/// is refused before any request reaches it.
#[tokio::test]
#[serial]
async fn an_issuer_the_pds_does_not_name_is_never_asked() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let requested = format!("atproto {SPACE}");
    let server = authorization_server(&app, &requested, false).await;

    let (status, body) = register(&app, &server, &requested, &requested, Some("rt")).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(token_requests(&server).await, 0);
    assert_eq!(stored_sessions(&app).await, 0);
}

/// With no issuer there is no authorization server to ask, so a scope beyond
/// `atproto` is refused and nothing is stored.
#[tokio::test]
#[serial]
async fn a_registration_without_an_issuer_holds_only_atproto() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let requested = format!("atproto {SPACE}");
    let server = authorization_server(&app, &requested, true).await;

    let (status, body) = register_with_issuer(&app, &requested, &requested, Some("rt"), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(token_requests(&server).await, 0);
    assert_eq!(stored_sessions(&app).await, 0);
}

/// While the authorization server is asked, the session holds `atproto` only:
/// the scopes the app registered are stored only once the grant covers them.
#[tokio::test]
#[serial]
async fn a_session_holds_only_atproto_until_its_grant_is_confirmed() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let requested = format!("atproto {SPACE}");
    let server = MockServer::start().await;
    let issuer = server.uri();
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": issuer,
            "token_endpoint": format!("{issuer}/oauth/token"),
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({
                    "access_token": "refreshed-access-token",
                    "refresh_token": "refreshed-refresh-token",
                    "token_type": "DPoP",
                    "expires_in": 3600,
                    "sub": DID,
                    "scope": requested,
                }))
                .set_delay(std::time::Duration::from_millis(1500)),
        )
        .mount(&server)
        .await;
    app.mock_session_verification(DID, DID).await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/pds/{DID}/.well-known/oauth-protected-resource"
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "authorization_servers": [issuer] })),
        )
        .mount(&app.mock_server)
        .await;

    let (client_key, client_secret, client_id) = app.create_api_client("confidential", None).await;
    let sql = happyview::db::adapt_sql(
        "UPDATE happyview_api_clients SET scopes = ? WHERE id = ?",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(&requested)
        .bind(&client_id)
        .execute(&app.state.db)
        .await
        .unwrap();
    let req = Request::builder()
        .method("POST")
        .uri("/oauth/dpop-keys")
        .header("content-type", "application/json")
        .header("x-client-key", &client_key)
        .header("x-client-secret", &client_secret)
        .body(Body::from("{}"))
        .unwrap();
    let provision = response_json(app.router.clone().oneshot(req).await.unwrap()).await;
    let session_req = Request::builder()
        .method("POST")
        .uri("/oauth/sessions")
        .header("content-type", "application/json")
        .header("x-client-key", &client_key)
        .header("x-client-secret", &client_secret)
        .body(Body::from(
            serde_json::to_vec(&json!({
                "provision_id": provision["provision_id"],
                "did": DID,
                "access_token": "app-access-token",
                "refresh_token": "rt",
                "expires_at": (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
                "scopes": requested,
                "issuer": issuer,
            }))
            .unwrap(),
        ))
        .unwrap();
    let router = app.router.clone();
    let registering =
        tokio::spawn(async move { router.oneshot(session_req).await.unwrap().status() });

    let sql = happyview::db::adapt_sql(
        "SELECT scopes FROM happyview_dpop_sessions WHERE user_did = ?",
        app.state.db_backend,
    );
    let mut held = None;
    for _ in 0..100 {
        let row: Option<(String,)> = happyview::db::query_as(&sql)
            .bind(DID)
            .fetch_optional(&app.state.db)
            .await
            .unwrap();
        if let Some((scopes,)) = row {
            held = Some(scopes);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(held.as_deref(), Some("atproto"));

    assert_eq!(registering.await.unwrap(), StatusCode::CREATED);
    let (stored,): (String,) = happyview::db::query_as(&sql)
        .bind(DID)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(stored, requested);
}
