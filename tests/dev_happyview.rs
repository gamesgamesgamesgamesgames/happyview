mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::oauth::pds_write::generate_dpop_proof;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn response_json(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap_or(json!(null))
}

fn post_json_with_headers(uri: &str, body: &Value, headers: Vec<(&str, &str)>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("host", "127.0.0.1:0");
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    builder
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

/// Set up a full DPoP session and return `(client_key, dpop_key, access_token)`.
async fn setup_dpop_session(app: &common::app::TestApp, user_did: &str) -> (String, Value, String) {
    let (client_key, client_secret, _id) = app.create_api_client("confidential", None).await;

    // 1. Provision DPoP key
    let key_req = post_json_with_headers(
        "/oauth/dpop-keys",
        &json!({}),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let key_resp = app.router.clone().oneshot(key_req).await.unwrap();
    assert_eq!(
        key_resp.status(),
        StatusCode::CREATED,
        "dpop key provisioning failed"
    );
    let key_body = response_json(key_resp).await;
    let provision_id = key_body["provision_id"].as_str().unwrap().to_string();
    let dpop_key = key_body["dpop_key"].clone();

    // 2. Register session
    let access_token = format!("test-access-{}", uuid::Uuid::new_v4());
    let session_req = post_json_with_headers(
        "/oauth/sessions",
        &json!({
            "provision_id": provision_id,
            "did": user_did,
            "access_token": &access_token,
            "scopes": "atproto",
            "pds_url": "https://pds.example.com",
        }),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let session_resp = app.router.clone().oneshot(session_req).await.unwrap();
    assert_eq!(
        session_resp.status(),
        StatusCode::CREATED,
        "session registration failed"
    );

    (client_key, dpop_key, access_token)
}

// ---------------------------------------------------------------------------
// listApiClients tests
// ---------------------------------------------------------------------------

/// Unauthenticated request (no Authorization header) should be rejected.
#[tokio::test]
#[serial]
async fn list_api_clients_unauthenticated_returns_non_200() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;

    let req = Request::builder()
        .method("GET")
        .uri("/xrpc/dev.happyview.listApiClients")
        .header("host", "127.0.0.1:0")
        .header("x-client-key", "hvc_fake")
        .body(Body::empty())
        .unwrap();

    let resp = app.router.clone().oneshot(req).await.unwrap();
    // Handler requires DPoP auth — anonymous access should be rejected (non-200)
    assert_ne!(
        resp.status(),
        StatusCode::OK,
        "unauthenticated request should not return 200"
    );
}

/// DPoP-authenticated request returns 200 with a `clients` array.
#[tokio::test]
#[serial]
async fn list_api_clients_authenticated_returns_200_with_clients_array() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let user_did = "did:plc:testowner";
    let (client_key, dpop_key, access_token) = setup_dpop_session(&app, user_did).await;

    let request_url = "http://127.0.0.1:0/xrpc/dev.happyview.listApiClients";
    let proof = generate_dpop_proof(&dpop_key, "GET", request_url, &access_token, None)
        .expect("failed to generate DPoP proof");

    let req = Request::builder()
        .method("GET")
        .uri("/xrpc/dev.happyview.listApiClients")
        .header("host", "127.0.0.1:0")
        .header("x-client-key", &client_key)
        .header("authorization", format!("DPoP {}", access_token))
        .header("dpop", &proof)
        .body(Body::empty())
        .unwrap();

    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = response_json(resp).await;
    assert!(
        body["clients"].is_array(),
        "response should contain a 'clients' array, got: {body}"
    );
}

// ---------------------------------------------------------------------------
// getApiClient tests
// ---------------------------------------------------------------------------

/// Authenticated request for a nonexistent client ID returns 404.
#[tokio::test]
#[serial]
async fn get_api_client_not_found() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let user_did = "did:plc:testowner404";
    let (client_key, dpop_key, access_token) = setup_dpop_session(&app, user_did).await;

    let request_url = "http://127.0.0.1:0/xrpc/dev.happyview.getApiClient";
    let proof = generate_dpop_proof(&dpop_key, "GET", request_url, &access_token, None)
        .expect("failed to generate DPoP proof");

    let req = Request::builder()
        .method("GET")
        .uri("/xrpc/dev.happyview.getApiClient?id=nonexistent-id")
        .header("host", "127.0.0.1:0")
        .header("x-client-key", &client_key)
        .header("authorization", format!("DPoP {}", access_token))
        .header("dpop", &proof)
        .body(Body::empty())
        .unwrap();

    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// createApiClient tests
// ---------------------------------------------------------------------------

/// DPoP-authenticated request creates a confidential client and returns
/// clientKey (hvc_) and clientSecret (hvs_) in the response.
#[tokio::test]
#[serial]
async fn create_api_client_via_xrpc() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let user_did = "did:plc:testcreator";
    let (client_key, dpop_key, access_token) = setup_dpop_session(&app, user_did).await;

    let request_url = "http://127.0.0.1:0/xrpc/dev.happyview.createApiClient";
    let proof = generate_dpop_proof(&dpop_key, "POST", request_url, &access_token, None)
        .expect("failed to generate DPoP proof");

    let body = json!({
        "name": "My Confidential Client",
        "clientIdUrl": "https://myapp.example.com/oauth/client",
        "clientUri": "https://myapp.example.com",
        "redirectUris": ["https://myapp.example.com/callback"],
        "clientType": "confidential",
    });

    let req = post_json_with_headers(
        "/xrpc/dev.happyview.createApiClient",
        &body,
        vec![
            ("x-client-key", &client_key),
            ("authorization", &format!("DPoP {}", access_token)),
            ("dpop", &proof),
        ],
    );

    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "expected 201 CREATED");

    let resp_body = response_json(resp).await;
    let client_key_val = resp_body["client"]["clientKey"].as_str().unwrap_or("");
    assert!(
        client_key_val.starts_with("hvc_"),
        "clientKey should start with 'hvc_', got: {client_key_val}"
    );

    let client_secret_val = resp_body["clientSecret"].as_str().unwrap_or("");
    assert!(
        client_secret_val.starts_with("hvs_"),
        "clientSecret should start with 'hvs_', got: {client_secret_val}"
    );

    assert_eq!(
        resp_body["client"]["clientType"].as_str().unwrap_or(""),
        "confidential"
    );
}

/// Creating a public client returns no clientSecret in the response.
#[tokio::test]
#[serial]
async fn create_api_client_public_no_secret() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let user_did = "did:plc:testcreatorpublic";
    let (client_key, dpop_key, access_token) = setup_dpop_session(&app, user_did).await;

    let request_url = "http://127.0.0.1:0/xrpc/dev.happyview.createApiClient";
    let proof = generate_dpop_proof(&dpop_key, "POST", request_url, &access_token, None)
        .expect("failed to generate DPoP proof");

    let body = json!({
        "name": "My Public Client",
        "clientIdUrl": "https://pubapp.example.com/oauth/client",
        "clientUri": "https://pubapp.example.com",
        "redirectUris": ["https://pubapp.example.com/callback"],
        "clientType": "public",
    });

    let req = post_json_with_headers(
        "/xrpc/dev.happyview.createApiClient",
        &body,
        vec![
            ("x-client-key", &client_key),
            ("authorization", &format!("DPoP {}", access_token)),
            ("dpop", &proof),
        ],
    );

    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "expected 201 CREATED");

    let resp_body = response_json(resp).await;
    let client_key_val = resp_body["client"]["clientKey"].as_str().unwrap_or("");
    assert!(
        client_key_val.starts_with("hvc_"),
        "clientKey should start with 'hvc_', got: {client_key_val}"
    );

    assert!(
        resp_body["clientSecret"].is_null(),
        "public client should have no clientSecret, got: {}",
        resp_body["clientSecret"]
    );

    assert_eq!(
        resp_body["client"]["clientType"].as_str().unwrap_or(""),
        "public"
    );
}

// ---------------------------------------------------------------------------
// deleteApiClient tests
// ---------------------------------------------------------------------------

/// Create a client, delete it, then verify a GET returns 404.
#[tokio::test]
#[serial]
async fn delete_api_client_success() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let user_did = "did:plc:testownerdelete";
    let (client_key, dpop_key, access_token) = setup_dpop_session(&app, user_did).await;

    // Insert a client owned by user_did.
    let client_id = uuid::Uuid::new_v4().to_string();
    let now = happyview::db::now_rfc3339();
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_api_clients (id, client_key, client_secret_hash, name, client_id_url, client_uri, redirect_uris, scopes, client_type, allowed_origins, is_active, created_by, created_at, updated_at, owner_did) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?)",
        app.state.db_backend,
    );
    sqlx::query(&sql)
        .bind(&client_id)
        .bind("hvc_delete_test_key")
        .bind("dummyhash")
        .bind("to-be-deleted")
        .bind("https://delete.example.com/oauth/abc")
        .bind("https://delete.example.com")
        .bind("[]")
        .bind("atproto")
        .bind("confidential")
        .bind::<Option<String>>(None)
        .bind(user_did)
        .bind(&now)
        .bind(&now)
        .bind(user_did)
        .execute(&app.state.db)
        .await
        .expect("failed to insert client for deletion test");

    // Delete the client via XRPC.
    let delete_url = "http://127.0.0.1:0/xrpc/dev.happyview.deleteApiClient";
    let delete_proof = generate_dpop_proof(&dpop_key, "POST", delete_url, &access_token, None)
        .expect("failed to generate DPoP proof for delete");

    let delete_req = post_json_with_headers(
        "/xrpc/dev.happyview.deleteApiClient",
        &json!({ "id": client_id }),
        vec![
            ("x-client-key", &client_key),
            ("authorization", &format!("DPoP {}", access_token)),
            ("dpop", &delete_proof),
        ],
    );

    let delete_resp = app.router.clone().oneshot(delete_req).await.unwrap();
    assert_eq!(
        delete_resp.status(),
        StatusCode::OK,
        "expected 200 OK on delete"
    );

    // Verify GET now returns 404.
    let get_uri = format!("/xrpc/dev.happyview.getApiClient?id={}", client_id);
    let get_url = "http://127.0.0.1:0/xrpc/dev.happyview.getApiClient";
    let get_proof = generate_dpop_proof(&dpop_key, "GET", get_url, &access_token, None)
        .expect("failed to generate DPoP proof for get");

    let get_req = Request::builder()
        .method("GET")
        .uri(&get_uri)
        .header("host", "127.0.0.1:0")
        .header("x-client-key", &client_key)
        .header("authorization", format!("DPoP {}", access_token))
        .header("dpop", &get_proof)
        .body(Body::empty())
        .unwrap();

    let get_resp = app.router.clone().oneshot(get_req).await.unwrap();
    assert_eq!(
        get_resp.status(),
        StatusCode::NOT_FOUND,
        "client should be gone after deletion"
    );
}

/// Attempting to delete a nonexistent client returns 404.
#[tokio::test]
#[serial]
async fn delete_api_client_not_found() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let user_did = "did:plc:testownerdel404";
    let (client_key, dpop_key, access_token) = setup_dpop_session(&app, user_did).await;

    let delete_url = "http://127.0.0.1:0/xrpc/dev.happyview.deleteApiClient";
    let delete_proof = generate_dpop_proof(&dpop_key, "POST", delete_url, &access_token, None)
        .expect("failed to generate DPoP proof");

    let delete_req = post_json_with_headers(
        "/xrpc/dev.happyview.deleteApiClient",
        &json!({ "id": "nonexistent-id-12345" }),
        vec![
            ("x-client-key", &client_key),
            ("authorization", &format!("DPoP {}", access_token)),
            ("dpop", &delete_proof),
        ],
    );

    let resp = app.router.clone().oneshot(delete_req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "deleting nonexistent client should return 404"
    );
}

/// Authenticated request returns 200 with the matching client.
#[tokio::test]
#[serial]
async fn get_api_client_returns_client() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let user_did = "did:plc:testownerget";
    let (client_key, dpop_key, access_token) = setup_dpop_session(&app, user_did).await;

    // Insert a child client owned by user_did directly.
    let client_id = uuid::Uuid::new_v4().to_string();
    let now = happyview::db::now_rfc3339();
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_api_clients (id, client_key, client_secret_hash, name, client_id_url, client_uri, redirect_uris, scopes, client_type, allowed_origins, is_active, created_by, created_at, updated_at, owner_did) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?)",
        app.state.db_backend,
    );
    sqlx::query(&sql)
        .bind(&client_id)
        .bind("hvc_owned_test_key")
        .bind("dummyhash")
        .bind("owned-client")
        .bind("https://owned.example.com/oauth/abc")
        .bind("https://owned.example.com")
        .bind("[]")
        .bind("atproto")
        .bind("confidential")
        .bind::<Option<String>>(None)
        .bind(user_did)
        .bind(&now)
        .bind(&now)
        .bind(user_did)
        .execute(&app.state.db)
        .await
        .expect("failed to insert owned client");

    let uri = format!("/xrpc/dev.happyview.getApiClient?id={}", client_id);
    let request_url = "http://127.0.0.1:0/xrpc/dev.happyview.getApiClient";
    let proof = generate_dpop_proof(&dpop_key, "GET", request_url, &access_token, None)
        .expect("failed to generate DPoP proof");

    let req = Request::builder()
        .method("GET")
        .uri(&uri)
        .header("host", "127.0.0.1:0")
        .header("x-client-key", &client_key)
        .header("authorization", format!("DPoP {}", access_token))
        .header("dpop", &proof)
        .body(Body::empty())
        .unwrap();

    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = response_json(resp).await;
    assert!(
        body["client"].is_object(),
        "response should contain a 'client' object, got: {body}"
    );
    assert_eq!(
        body["client"]["id"].as_str().unwrap(),
        client_id,
        "returned client id should match"
    );
    assert_eq!(
        body["client"]["name"].as_str().unwrap(),
        "owned-client",
        "returned client name should match"
    );
}
