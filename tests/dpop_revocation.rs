mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::oauth::pds_write::generate_dpop_proof;
use http_body_util::BodyExt;
use serde_json::json;
use serial_test::serial;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn post_json_with_headers(
    uri: &str,
    body: &serde_json::Value,
    headers: Vec<(&str, &str)>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("host", "127.0.0.1");
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    builder
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

fn delete_with_headers(uri: &str, headers: Vec<(&str, &str)>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("DELETE")
        .uri(uri)
        .header("host", "127.0.0.1");
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    builder.body(Body::empty()).unwrap()
}

async fn response_json(resp: axum::http::Response<Body>) -> serde_json::Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap_or(json!(null))
}

async fn session_count(app: &common::app::TestApp, did: &str) -> i64 {
    let row: (i64,) = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT COUNT(*) FROM happyview_dpop_sessions WHERE user_did = ?",
        app.state.db_backend,
    ))
    .bind(did)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    row.0
}

/// Set up an authorization server mock, a client, and a live session.
/// Returns (client_key, dpop_key, access_token, did).
async fn logged_in(
    app: &common::app::TestApp,
    auth_server: &MockServer,
    did: &str,
) -> (String, serde_json::Value, String) {
    let (client_key, client_secret, _id) = app.create_api_client("confidential", None).await;

    let key_req = post_json_with_headers(
        "/oauth/dpop-keys",
        &json!({}),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let key_resp = app.router.clone().oneshot(key_req).await.unwrap();
    assert_eq!(key_resp.status(), StatusCode::CREATED);
    let key_body = response_json(key_resp).await;
    let provision_id = key_body["provision_id"].as_str().unwrap().to_string();
    let dpop_key = key_body["dpop_key"].clone();

    app.mock_session_verification(did, did).await;

    let access_token = "revocation-access-token".to_string();
    let session_req = post_json_with_headers(
        "/oauth/sessions",
        &json!({
            "provision_id": provision_id,
            "did": did,
            "access_token": access_token,
            "refresh_token": "revocation-refresh-token",
            "scopes": "atproto",
            "pds_url": "https://pds.example.com",
            "issuer": auth_server.uri(),
        }),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let session_resp = app.router.clone().oneshot(session_req).await.unwrap();
    assert_eq!(session_resp.status(), StatusCode::CREATED);

    (client_key, dpop_key, access_token)
}

async fn mount_metadata(auth_server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": auth_server.uri(),
            "token_endpoint": format!("{}/oauth/token", auth_server.uri()),
            "revocation_endpoint": format!("{}/oauth/revoke", auth_server.uri()),
        })))
        .mount(auth_server)
        .await;
}

/// Logging out must tell the user's PDS, not just forget the row locally.
#[tokio::test]
#[serial]
async fn logout_revokes_the_token_at_the_authorization_server() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let did = "did:plc:revokeme";

    let auth_server = MockServer::start().await;
    mount_metadata(&auth_server).await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&auth_server)
        .await;

    let (client_key, dpop_key, access_token) = logged_in(&app, &auth_server, did).await;
    assert_eq!(session_count(&app, did).await, 1);

    let request_url = format!("http://127.0.0.1/oauth/sessions/{did}");
    let proof =
        generate_dpop_proof(&dpop_key, "DELETE", &request_url, &access_token, None).unwrap();
    let del_req = delete_with_headers(
        &format!("/oauth/sessions/{did}"),
        vec![
            ("x-client-key", &client_key),
            ("authorization", &format!("DPoP {access_token}")),
            ("dpop", &proof),
        ],
    );
    let del_resp = app.router.clone().oneshot(del_req).await.unwrap();
    assert_eq!(del_resp.status(), StatusCode::NO_CONTENT);

    let revocations: Vec<String> = auth_server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == "/oauth/revoke")
        .map(|r| String::from_utf8_lossy(&r.body).to_string())
        .collect();

    assert_eq!(
        revocations.len(),
        1,
        "logout should revoke exactly once, saw {revocations:?}"
    );
    assert!(
        revocations[0].contains("revocation-refresh-token"),
        "revocation should carry the refresh token, body was {:?}",
        revocations[0]
    );
    assert_eq!(
        session_count(&app, did).await,
        0,
        "row should still be gone"
    );
}

/// An authorization server that refuses or cannot be reached must not be able
/// to trap the user in a signed-in state. Revocation is best-effort; the local
/// delete is not.
#[tokio::test]
#[serial]
async fn logout_completes_even_when_revocation_fails() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let did = "did:plc:revokefails";

    let auth_server = MockServer::start().await;
    mount_metadata(&auth_server).await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .respond_with(ResponseTemplate::new(500).set_body_string("nope"))
        .mount(&auth_server)
        .await;

    let (client_key, dpop_key, access_token) = logged_in(&app, &auth_server, did).await;

    let request_url = format!("http://127.0.0.1/oauth/sessions/{did}");
    let proof =
        generate_dpop_proof(&dpop_key, "DELETE", &request_url, &access_token, None).unwrap();
    let del_req = delete_with_headers(
        &format!("/oauth/sessions/{did}"),
        vec![
            ("x-client-key", &client_key),
            ("authorization", &format!("DPoP {access_token}")),
            ("dpop", &proof),
        ],
    );
    let del_resp = app.router.clone().oneshot(del_req).await.unwrap();

    assert_eq!(
        del_resp.status(),
        StatusCode::NO_CONTENT,
        "a failed revocation must not fail the logout"
    );
    assert_eq!(session_count(&app, did).await, 0);
}
