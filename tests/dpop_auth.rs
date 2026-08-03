mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::oauth::pds_write::generate_dpop_proof;
use http_body_util::BodyExt;
use serde_json::json;
use serial_test::serial;
use tower::ServiceExt;

/// Helper to make a POST request with JSON body and extra headers
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

/// Helper to make a GET request with headers
fn get_with_headers(uri: &str, headers: Vec<(&str, &str)>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", "127.0.0.1");
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    builder.body(Body::empty()).unwrap()
}

/// Helper to make a DELETE request with headers
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

#[tokio::test]
#[serial]
async fn test_provision_dpop_key_confidential_client() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let (client_key, client_secret, _id) = app.create_api_client("confidential", None).await;

    let req = post_json_with_headers(
        "/oauth/dpop-keys",
        &json!({}),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );

    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = response_json(resp).await;
    assert!(body["provision_id"].is_string());
    assert!(body["dpop_key"]["d"].is_string());
    assert_eq!(body["dpop_key"]["kty"], "EC");
    assert_eq!(body["dpop_key"]["crv"], "P-256");
}

#[tokio::test]
#[serial]
async fn test_provision_dpop_key_public_client_requires_pkce() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let (client_key, _secret, _id) = app
        .create_api_client("public", Some(vec!["http://localhost:3000".to_string()]))
        .await;

    // Without PKCE challenge should fail
    let req = post_json_with_headers(
        "/oauth/dpop-keys",
        &json!({}),
        vec![
            ("x-client-key", &client_key),
            ("origin", "http://localhost:3000"),
        ],
    );

    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[serial]
async fn test_provision_dpop_key_public_client_with_pkce() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let (client_key, _secret, _id) = app
        .create_api_client("public", Some(vec!["http://localhost:3000".to_string()]))
        .await;

    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use sha2::{Digest, Sha256};

    let verifier = "test-verifier-string-for-pkce-challenge-1234";
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));

    let req = post_json_with_headers(
        "/oauth/dpop-keys",
        &json!({
            "pkce_challenge": challenge,
        }),
        vec![
            ("x-client-key", &client_key),
            ("origin", "http://localhost:3000"),
        ],
    );

    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = response_json(resp).await;
    assert!(body["provision_id"].is_string());
}

#[tokio::test]
#[serial]
async fn test_register_session_validates_scopes() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let (client_key, client_secret, _id) = app.create_api_client("confidential", None).await;

    // Provision a key first
    let key_req = post_json_with_headers(
        "/oauth/dpop-keys",
        &json!({}),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let key_resp = app.router.clone().oneshot(key_req).await.unwrap();
    let key_body = response_json(key_resp).await;
    let provision_id = key_body["provision_id"].as_str().unwrap();

    // Try to register with a scope the client doesn't have
    let req = post_json_with_headers(
        "/oauth/sessions",
        &json!({
            "provision_id": provision_id,
            "did": "did:plc:test123",
            "access_token": "test-token",
            "scopes": "atproto com.unauthorized.scope",
        }),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );

    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[serial]
async fn test_register_session_requires_atproto_scope() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
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
    let key_body = response_json(key_resp).await;
    let provision_id = key_body["provision_id"].as_str().unwrap();

    let req = post_json_with_headers(
        "/oauth/sessions",
        &json!({
            "provision_id": provision_id,
            "did": "did:plc:test123",
            "access_token": "test-token",
            "scopes": "transition:generic",
        }),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );

    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[serial]
async fn test_register_session_rejects_did_not_matching_token() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let (client_key, client_secret, _id) = app.create_api_client("confidential", None).await;

    // The attacker claims to be the victim, but the access token they present
    // belongs to the attacker's own DID. getSession on the victim's PDS reports
    // the attacker DID, so registration must be rejected.
    app.mock_session_verification("did:plc:victim", "did:plc:attacker")
        .await;

    let key_req = post_json_with_headers(
        "/oauth/dpop-keys",
        &json!({}),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let key_resp = app.router.clone().oneshot(key_req).await.unwrap();
    let key_body = response_json(key_resp).await;
    let provision_id = key_body["provision_id"].as_str().unwrap();

    let req = post_json_with_headers(
        "/oauth/sessions",
        &json!({
            "provision_id": provision_id,
            "did": "did:plc:victim",
            "access_token": "attacker-token",
            "scopes": "atproto",
        }),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );

    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "registering a session for a DID the token does not belong to must be rejected"
    );
}

#[tokio::test]
#[serial]
async fn test_register_session_rejects_token_pds_refuses() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};

    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let (client_key, client_secret, _id) = app.create_api_client("confidential", None).await;

    // The victim's DID document resolves to their real PDS, which rejects the
    // attacker's token outright (401). Registration must fail.
    let pds_url = app.mock_server.uri();
    Mock::given(method("GET"))
        .and(path("/did:plc:victim2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "did:plc:victim2",
            "service": [{
                "id": "#atproto_pds",
                "type": "AtprotoPersonalDataServer",
                "serviceEndpoint": pds_url,
            }]
        })))
        .mount(&app.mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/xrpc/com.atproto.server.getSession"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({ "error": "InvalidToken" })))
        .mount(&app.mock_server)
        .await;

    let key_req = post_json_with_headers(
        "/oauth/dpop-keys",
        &json!({}),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let key_resp = app.router.clone().oneshot(key_req).await.unwrap();
    let key_body = response_json(key_resp).await;
    let provision_id = key_body["provision_id"].as_str().unwrap();

    let req = post_json_with_headers(
        "/oauth/sessions",
        &json!({
            "provision_id": provision_id,
            "did": "did:plc:victim2",
            "access_token": "attacker-token",
            "scopes": "atproto",
        }),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );

    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "registration must fail when the PDS refuses the presented access token"
    );
}

#[tokio::test]
#[serial]
async fn test_full_flow_provision_register_delete() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let (client_key, client_secret, _id) = app.create_api_client("confidential", None).await;

    // 1. Provision key
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
    let provision_id = key_body["provision_id"].as_str().unwrap();

    // 2. Register session
    app.mock_session_verification("did:plc:testuser", "did:plc:testuser")
        .await;
    let session_req = post_json_with_headers(
        "/oauth/sessions",
        &json!({
            "provision_id": provision_id,
            "did": "did:plc:testuser",
            "access_token": "test-access-token-123",
            "refresh_token": "test-refresh-token-456",
            "scopes": "atproto",
            "pds_url": "https://pds.example.com",
        }),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let session_resp = app.router.clone().oneshot(session_req).await.unwrap();
    assert_eq!(session_resp.status(), StatusCode::CREATED);
    let session_body = response_json(session_resp).await;
    assert_eq!(session_body["did"], "did:plc:testuser");

    // 3. Delete session
    let delete_req = delete_with_headers(
        "/oauth/sessions/did:plc:testuser",
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let delete_resp = app.router.clone().oneshot(delete_req).await.unwrap();
    assert_eq!(delete_resp.status(), StatusCode::NO_CONTENT);

    // 4. Verify session is gone (delete is idempotent for confidential clients)
    let delete_req2 = delete_with_headers(
        "/oauth/sessions/did:plc:testuser",
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let delete_resp2 = app.router.clone().oneshot(delete_req2).await.unwrap();
    assert_eq!(delete_resp2.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
#[serial]
async fn test_xrpc_rejects_bearer_auth() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;

    // Bearer auth should be explicitly rejected on XRPC routes
    let req = Request::builder()
        .method("GET")
        .uri("/xrpc/com.example.test.getStuff")
        .header("host", "127.0.0.1")
        .header("x-client-key", "hvc_fake")
        .header("authorization", "Bearer hv_some-api-key")
        .body(Body::empty())
        .unwrap();

    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = response_json(resp).await;
    let msg = body["error"].as_str().unwrap_or_default();
    assert!(
        msg.contains("XRPC routes do not accept Bearer auth"),
        "expected Bearer rejection message, got: {msg}"
    );
}

#[tokio::test]
#[serial]
async fn test_xrpc_allows_anonymous_queries() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;

    // Anonymous access (no auth header) should pass through to lexicon lookup.
    // Since no lexicon is registered, we expect a proxy attempt or 502, not 401.
    let req = Request::builder()
        .method("GET")
        .uri("/xrpc/com.example.test.getStuff")
        .header("host", "127.0.0.1")
        .header("x-client-key", "hvc_fake")
        .body(Body::empty())
        .unwrap();

    let resp = app.router.clone().oneshot(req).await.unwrap();
    // Not a 401 — anonymous access was allowed, the error is from the handler (no lexicon)
    assert_ne!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "anonymous XRPC queries should not require auth"
    );
}

#[tokio::test]
#[serial]
async fn test_xrpc_procedure_requires_dpop_auth() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;

    // POST to an XRPC procedure without DPoP auth should be rejected
    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.example.test.createStuff")
        .header("host", "127.0.0.1")
        .header("x-client-key", "hvc_fake")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();

    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = response_json(resp).await;
    let msg = body["error"].as_str().unwrap_or_default();
    assert!(
        msg.contains("DPoP authentication"),
        "expected DPoP requirement message, got: {msg}"
    );
}

#[tokio::test]
#[serial]
async fn test_xrpc_dpop_auth_accepted() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let (client_key, client_secret, _id) = app.create_api_client("confidential", None).await;

    // 1. Provision key
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
    let provision_id = key_body["provision_id"].as_str().unwrap();
    let dpop_key = &key_body["dpop_key"];

    // 2. Register session
    let access_token = "test-xrpc-access-token";
    app.mock_session_verification("did:plc:xrpcuser", "did:plc:xrpcuser")
        .await;
    let session_req = post_json_with_headers(
        "/oauth/sessions",
        &json!({
            "provision_id": provision_id,
            "did": "did:plc:xrpcuser",
            "access_token": access_token,
            "scopes": "atproto",
            "pds_url": "https://pds.example.com",
        }),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let session_resp = app.router.clone().oneshot(session_req).await.unwrap();
    assert_eq!(session_resp.status(), StatusCode::CREATED);

    // 3. Generate a DPoP proof for an XRPC GET request
    let request_url = "http://127.0.0.1:0/xrpc/com.example.test.getStuff";
    let proof = generate_dpop_proof(dpop_key, "GET", request_url, access_token, None)
        .expect("failed to generate DPoP proof");

    // 4. Make an XRPC request with DPoP auth
    let xrpc_req = Request::builder()
        .method("GET")
        .uri("/xrpc/com.example.test.getStuff")
        .header("host", "127.0.0.1:0")
        .header("x-client-key", &client_key)
        .header("authorization", format!("DPoP {}", access_token))
        .header("dpop", &proof)
        .body(Body::empty())
        .unwrap();

    let xrpc_resp = app.router.clone().oneshot(xrpc_req).await.unwrap();
    // Auth should succeed — any non-401 status means DPoP auth was accepted.
    // We expect a 502 (proxy attempt for unknown lexicon) or similar, not 401.
    assert_ne!(
        xrpc_resp.status(),
        StatusCode::UNAUTHORIZED,
        "DPoP-authenticated XRPC request should not get 401"
    );
}

/// Helper: provision a DPoP key and register a session. Returns (provision_id, dpop_key, session_id).
async fn provision_and_register(
    app: &common::app::TestApp,
    client_key: &str,
    client_secret: &str,
    did: &str,
    access_token: &str,
) -> (String, serde_json::Value, String) {
    let key_req = post_json_with_headers(
        "/oauth/dpop-keys",
        &json!({}),
        vec![
            ("x-client-key", client_key),
            ("x-client-secret", client_secret),
        ],
    );
    let key_resp = app.router.clone().oneshot(key_req).await.unwrap();
    assert_eq!(key_resp.status(), StatusCode::CREATED);
    let key_body = response_json(key_resp).await;
    let provision_id = key_body["provision_id"].as_str().unwrap().to_string();
    let dpop_key = key_body["dpop_key"].clone();

    app.mock_session_verification(did, did).await;

    let session_req = post_json_with_headers(
        "/oauth/sessions",
        &json!({
            "provision_id": provision_id,
            "did": did,
            "access_token": access_token,
            "scopes": "atproto",
            "pds_url": "https://pds.example.com",
        }),
        vec![
            ("x-client-key", client_key),
            ("x-client-secret", client_secret),
        ],
    );
    let session_resp = app.router.clone().oneshot(session_req).await.unwrap();
    assert_eq!(session_resp.status(), StatusCode::CREATED);
    let session_body = response_json(session_resp).await;
    let session_id = session_body["session_id"].as_str().unwrap().to_string();

    (provision_id, dpop_key, session_id)
}

#[tokio::test]
#[serial]
async fn test_multi_device_sessions_coexist() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let (client_key, client_secret, _id) = app.create_api_client("confidential", None).await;
    let did = "did:plc:multidevice";

    let (_prov1, _key1, session_id_1) =
        provision_and_register(&app, &client_key, &client_secret, did, "token-device-1").await;
    let (_prov2, _key2, session_id_2) =
        provision_and_register(&app, &client_key, &client_secret, did, "token-device-2").await;

    assert_ne!(session_id_1, session_id_2);

    // Both sessions should appear in the device list
    let list_req = get_with_headers(
        &format!("/oauth/sessions/{}/devices", did),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let list_resp = app.router.clone().oneshot(list_req).await.unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let devices: Vec<serde_json::Value> =
        serde_json::from_value(response_json(list_resp).await).unwrap();
    assert_eq!(devices.len(), 2);

    let ids: Vec<&str> = devices.iter().map(|d| d["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&session_id_1.as_str()));
    assert!(ids.contains(&session_id_2.as_str()));
}

#[tokio::test]
#[serial]
async fn test_list_device_sessions_empty() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let (client_key, client_secret, _id) = app.create_api_client("confidential", None).await;

    let list_req = get_with_headers(
        "/oauth/sessions/did:plc:nobody/devices",
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let list_resp = app.router.clone().oneshot(list_req).await.unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let devices: Vec<serde_json::Value> =
        serde_json::from_value(response_json(list_resp).await).unwrap();
    assert!(devices.is_empty());
}

#[tokio::test]
#[serial]
async fn test_delete_device_session_by_id() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let (client_key, client_secret, _id) = app.create_api_client("confidential", None).await;
    let did = "did:plc:deletedevice";

    let (_prov1, _key1, session_id_1) =
        provision_and_register(&app, &client_key, &client_secret, did, "token-a").await;
    let (_prov2, _key2, session_id_2) =
        provision_and_register(&app, &client_key, &client_secret, did, "token-b").await;

    // Delete session 1
    let del_req = delete_with_headers(
        &format!("/oauth/sessions/{}/devices/{}", did, session_id_1),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let del_resp = app.router.clone().oneshot(del_req).await.unwrap();
    assert_eq!(del_resp.status(), StatusCode::NO_CONTENT);

    // Only session 2 should remain
    let list_req = get_with_headers(
        &format!("/oauth/sessions/{}/devices", did),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let list_resp = app.router.clone().oneshot(list_req).await.unwrap();
    let devices: Vec<serde_json::Value> =
        serde_json::from_value(response_json(list_resp).await).unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0]["id"].as_str().unwrap(), session_id_2);
}

#[tokio::test]
#[serial]
async fn test_delete_device_session_not_found() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let (client_key, client_secret, _id) = app.create_api_client("confidential", None).await;

    let del_req = delete_with_headers(
        "/oauth/sessions/did:plc:nobody/devices/nonexistent-id",
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let del_resp = app.router.clone().oneshot(del_req).await.unwrap();
    assert_eq!(del_resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[serial]
async fn test_session_upsert_same_device() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let (client_key, client_secret, _id) = app.create_api_client("confidential", None).await;
    let did = "did:plc:upsertuser";

    // Provision one key
    let key_req = post_json_with_headers(
        "/oauth/dpop-keys",
        &json!({}),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let key_resp = app.router.clone().oneshot(key_req).await.unwrap();
    let key_body = response_json(key_resp).await;
    let provision_id = key_body["provision_id"].as_str().unwrap();

    app.mock_session_verification(did, did).await;

    // Register session with token-v1
    let reg1 = post_json_with_headers(
        "/oauth/sessions",
        &json!({
            "provision_id": provision_id,
            "did": did,
            "access_token": "token-v1",
            "scopes": "atproto",
        }),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let resp1 = app.router.clone().oneshot(reg1).await.unwrap();
    assert_eq!(resp1.status(), StatusCode::CREATED);

    // Re-register with same provision_id (same device key) but new token
    let reg2 = post_json_with_headers(
        "/oauth/sessions",
        &json!({
            "provision_id": provision_id,
            "did": did,
            "access_token": "token-v2",
            "scopes": "atproto",
        }),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let resp2 = app.router.clone().oneshot(reg2).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::CREATED);

    // Should still be exactly one device session (upsert, not duplicate)
    let list_req = get_with_headers(
        &format!("/oauth/sessions/{}/devices", did),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let list_resp = app.router.clone().oneshot(list_req).await.unwrap();
    let devices: Vec<serde_json::Value> =
        serde_json::from_value(response_json(list_resp).await).unwrap();
    assert_eq!(devices.len(), 1);
}

#[tokio::test]
#[serial]
async fn test_get_session_with_confidential_client() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let (client_key, client_secret, _id) = app.create_api_client("confidential", None).await;
    let did = "did:plc:getsession";

    provision_and_register(&app, &client_key, &client_secret, did, "some-token").await;

    let get_req = get_with_headers(
        &format!("/oauth/sessions/{}", did),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let get_resp = app.router.clone().oneshot(get_req).await.unwrap();
    assert_eq!(get_resp.status(), StatusCode::OK);
    let body = response_json(get_resp).await;
    assert_eq!(body["did"], did);
    assert!(body["scopes"].is_array());
}

#[tokio::test]
#[serial]
async fn test_device_list_response_format() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let (client_key, client_secret, _id) = app.create_api_client("confidential", None).await;
    let did = "did:plc:formatcheck";

    provision_and_register(&app, &client_key, &client_secret, did, "token-fmt").await;

    let list_req = get_with_headers(
        &format!("/oauth/sessions/{}/devices", did),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let list_resp = app.router.clone().oneshot(list_req).await.unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let devices: Vec<serde_json::Value> =
        serde_json::from_value(response_json(list_resp).await).unwrap();
    assert_eq!(devices.len(), 1);

    let device = &devices[0];
    assert!(device["id"].is_string());
    assert!(device["dpop_key_id"].is_string());
    assert!(device["scopes"].is_array());
    assert!(device["created_at"].is_string());
    assert!(device["updated_at"].is_string());
}

#[tokio::test]
#[serial]
async fn test_public_client_dpop_get_session() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let (client_key, _secret, _id) = app
        .create_api_client("public", Some(vec!["http://localhost:3000".to_string()]))
        .await;

    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use sha2::{Digest, Sha256};

    let verifier = "test-verifier-for-public-client-session";
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));

    // Provision key with PKCE
    let key_req = post_json_with_headers(
        "/oauth/dpop-keys",
        &json!({ "pkce_challenge": challenge }),
        vec![
            ("x-client-key", &client_key),
            ("origin", "http://localhost:3000"),
        ],
    );
    let key_resp = app.router.clone().oneshot(key_req).await.unwrap();
    assert_eq!(key_resp.status(), StatusCode::CREATED);
    let key_body = response_json(key_resp).await;
    let provision_id = key_body["provision_id"].as_str().unwrap();
    let dpop_key = &key_body["dpop_key"];

    let did = "did:plc:publicuser";
    let access_token = "public-client-access-token";

    app.mock_session_verification(did, did).await;

    // Register session with PKCE verifier
    let session_req = post_json_with_headers(
        "/oauth/sessions",
        &json!({
            "provision_id": provision_id,
            "pkce_verifier": verifier,
            "did": did,
            "access_token": access_token,
            "scopes": "atproto",
            "pds_url": "https://pds.example.com",
        }),
        vec![("x-client-key", &client_key)],
    );
    let session_resp = app.router.clone().oneshot(session_req).await.unwrap();
    assert_eq!(session_resp.status(), StatusCode::CREATED);

    // GET session with DPoP proof
    let request_url = format!("http://127.0.0.1/oauth/sessions/{}", did);
    let proof = generate_dpop_proof(dpop_key, "GET", &request_url, access_token, None)
        .expect("failed to generate DPoP proof");

    let get_req = get_with_headers(
        &format!("/oauth/sessions/{}", did),
        vec![
            ("x-client-key", &client_key),
            ("authorization", &format!("DPoP {}", access_token)),
            ("dpop", &proof),
        ],
    );
    let get_resp = app.router.clone().oneshot(get_req).await.unwrap();
    assert_eq!(get_resp.status(), StatusCode::OK);
    let body = response_json(get_resp).await;
    assert_eq!(body["did"], did);
}

#[tokio::test]
#[serial]
async fn test_public_client_dpop_delete_session() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let (client_key, _secret, _id) = app
        .create_api_client("public", Some(vec!["http://localhost:3000".to_string()]))
        .await;

    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use sha2::{Digest, Sha256};

    let verifier = "test-verifier-for-public-delete";
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));

    let key_req = post_json_with_headers(
        "/oauth/dpop-keys",
        &json!({ "pkce_challenge": challenge }),
        vec![
            ("x-client-key", &client_key),
            ("origin", "http://localhost:3000"),
        ],
    );
    let key_resp = app.router.clone().oneshot(key_req).await.unwrap();
    assert_eq!(key_resp.status(), StatusCode::CREATED);
    let key_body = response_json(key_resp).await;
    let provision_id = key_body["provision_id"].as_str().unwrap();
    let dpop_key = &key_body["dpop_key"];

    let did = "did:plc:publicdelete";
    let access_token = "public-delete-token";

    app.mock_session_verification(did, did).await;

    let session_req = post_json_with_headers(
        "/oauth/sessions",
        &json!({
            "provision_id": provision_id,
            "pkce_verifier": verifier,
            "did": did,
            "access_token": access_token,
            "scopes": "atproto",
            "pds_url": "https://pds.example.com",
        }),
        vec![("x-client-key", &client_key)],
    );
    let session_resp = app.router.clone().oneshot(session_req).await.unwrap();
    assert_eq!(session_resp.status(), StatusCode::CREATED);

    // DELETE session with DPoP proof
    let request_url = format!("http://127.0.0.1/oauth/sessions/{}", did);
    let proof = generate_dpop_proof(dpop_key, "DELETE", &request_url, access_token, None)
        .expect("failed to generate DPoP proof");

    let del_req = delete_with_headers(
        &format!("/oauth/sessions/{}", did),
        vec![
            ("x-client-key", &client_key),
            ("authorization", &format!("DPoP {}", access_token)),
            ("dpop", &proof),
        ],
    );
    let del_resp = app.router.clone().oneshot(del_req).await.unwrap();
    assert_eq!(del_resp.status(), StatusCode::NO_CONTENT);

    // Verify session is gone — GET should fail
    let request_url2 = format!("http://127.0.0.1/oauth/sessions/{}", did);
    let proof2 = generate_dpop_proof(dpop_key, "GET", &request_url2, access_token, None)
        .expect("failed to generate DPoP proof");

    let get_req = get_with_headers(
        &format!("/oauth/sessions/{}", did),
        vec![
            ("x-client-key", &client_key),
            ("authorization", &format!("DPoP {}", access_token)),
            ("dpop", &proof2),
        ],
    );
    let get_resp = app.router.clone().oneshot(get_req).await.unwrap();
    assert_ne!(get_resp.status(), StatusCode::OK);
}

/// A confidential client can provision a DPoP key and use it against `/xrpc/*`
/// without ever presenting its secret, because `resolve_dpop_claims` does not
/// look at `client_type`. Revoking the session it just used must not be the one
/// operation that demands more — otherwise logout 401s forever while every
/// other call succeeds, and the client cannot get out of the loop.
#[tokio::test]
#[serial]
async fn test_confidential_client_dpop_delete_session_without_secret() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
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
    let provision_id = key_body["provision_id"].as_str().unwrap();
    let dpop_key = &key_body["dpop_key"];

    let did = "did:plc:confidentialdelete";
    let access_token = "confidential-delete-token";

    app.mock_session_verification(did, did).await;

    let session_req = post_json_with_headers(
        "/oauth/sessions",
        &json!({
            "provision_id": provision_id,
            "did": did,
            "access_token": access_token,
            "scopes": "atproto",
            "pds_url": "https://pds.example.com",
        }),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let session_resp = app.router.clone().oneshot(session_req).await.unwrap();
    assert_eq!(session_resp.status(), StatusCode::CREATED);

    // DPoP proof only — no X-Client-Secret, exactly what the JS SDK sends.
    let request_url = format!("http://127.0.0.1/oauth/sessions/{}", did);
    let proof = generate_dpop_proof(dpop_key, "DELETE", &request_url, access_token, None)
        .expect("failed to generate DPoP proof");

    let del_req = delete_with_headers(
        &format!("/oauth/sessions/{}", did),
        vec![
            ("x-client-key", &client_key),
            ("authorization", &format!("DPoP {}", access_token)),
            ("dpop", &proof),
        ],
    );
    let del_resp = app.router.clone().oneshot(del_req).await.unwrap();
    assert_eq!(del_resp.status(), StatusCode::NO_CONTENT);
}

/// The device-session route builds its htu from the request path rather than
/// from a format string, and this router is nested under `/oauth` — so the
/// handler's own `req.uri()` has that prefix stripped. The client signs the
/// full URL it requested, so the server has to reconstruct the full one too.
/// Every existing test of this route authenticates with the client secret and
/// so never reaches the proof check.
#[tokio::test]
#[serial]
async fn test_dpop_delete_device_session_htu_includes_router_prefix() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let (client_key, client_secret, _id) = app.create_api_client("confidential", None).await;
    let did = "did:plc:devicedpop";

    let (_prov, dpop_key, session_id) =
        provision_and_register(&app, &client_key, &client_secret, did, "device-dpop-token").await;

    let path = format!("/oauth/sessions/{}/devices/{}", did, session_id);
    let request_url = format!("http://127.0.0.1{}", path);
    let proof = generate_dpop_proof(&dpop_key, "DELETE", &request_url, "device-dpop-token", None)
        .expect("failed to generate DPoP proof");

    let del_req = delete_with_headers(
        &path,
        vec![
            ("x-client-key", &client_key),
            ("authorization", "DPoP device-dpop-token"),
            ("dpop", &proof),
        ],
    );
    let del_resp = app.router.clone().oneshot(del_req).await.unwrap();
    assert_eq!(del_resp.status(), StatusCode::NO_CONTENT);
}

/// The htu is whatever the client actually signed, which is whatever it put on
/// the wire. A client that percent-encodes the DID signs the encoded form, so
/// rebuilding the URL from a decoded path segment produces a mismatch that no
/// caller can fix from their side.
#[tokio::test]
#[serial]
async fn test_public_client_dpop_delete_session_percent_encoded_did() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let (client_key, _secret, _id) = app
        .create_api_client("public", Some(vec!["http://localhost:3000".to_string()]))
        .await;

    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use sha2::{Digest, Sha256};

    let verifier = "test-verifier-for-encoded-delete";
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));

    let key_req = post_json_with_headers(
        "/oauth/dpop-keys",
        &json!({ "pkce_challenge": challenge }),
        vec![
            ("x-client-key", &client_key),
            ("origin", "http://localhost:3000"),
        ],
    );
    let key_resp = app.router.clone().oneshot(key_req).await.unwrap();
    assert_eq!(key_resp.status(), StatusCode::CREATED);
    let key_body = response_json(key_resp).await;
    let provision_id = key_body["provision_id"].as_str().unwrap();
    let dpop_key = &key_body["dpop_key"];

    let did = "did:plc:encodeddelete";
    let encoded_did = "did%3Aplc%3Aencodeddelete";
    let access_token = "encoded-delete-token";

    app.mock_session_verification(did, did).await;

    let session_req = post_json_with_headers(
        "/oauth/sessions",
        &json!({
            "provision_id": provision_id,
            "pkce_verifier": verifier,
            "did": did,
            "access_token": access_token,
            "scopes": "atproto",
            "pds_url": "https://pds.example.com",
        }),
        vec![("x-client-key", &client_key)],
    );
    let session_resp = app.router.clone().oneshot(session_req).await.unwrap();
    assert_eq!(session_resp.status(), StatusCode::CREATED);

    let request_url = format!("http://127.0.0.1/oauth/sessions/{}", encoded_did);
    let proof = generate_dpop_proof(dpop_key, "DELETE", &request_url, access_token, None)
        .expect("failed to generate DPoP proof");

    let del_req = delete_with_headers(
        &format!("/oauth/sessions/{}", encoded_did),
        vec![
            ("x-client-key", &client_key),
            ("authorization", &format!("DPoP {}", access_token)),
            ("dpop", &proof),
        ],
    );
    let del_resp = app.router.clone().oneshot(del_req).await.unwrap();
    assert_eq!(del_resp.status(), StatusCode::NO_CONTENT);
}
