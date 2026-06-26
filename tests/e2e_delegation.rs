mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::db::adapt_sql;
use happyview::oauth::pds_write::generate_dpop_proof;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

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

fn get_with_headers(uri: &str, headers: Vec<(&str, &str)>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", "127.0.0.1:0");
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    builder.body(Body::empty()).unwrap()
}

/// Set up a full DPoP session and return `(client_key, dpop_key, access_token)`.
async fn setup_dpop_session(app: &common::app::TestApp, user_did: &str) -> (String, Value, String) {
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
    assert_eq!(session_resp.status(), StatusCode::CREATED);

    (client_key, dpop_key, access_token)
}

/// Build DPoP auth headers for a request.
fn dpop_auth_headers<'a>(
    client_key: &'a str,
    dpop_key: &Value,
    access_token: &'a str,
    method: &str,
    url: &str,
) -> Vec<(&'static str, String)> {
    // DPoP htu must not include query/fragment
    let htu = url.split('?').next().unwrap_or(url);
    let proof = generate_dpop_proof(dpop_key, method, htu, access_token, None)
        .expect("failed to generate DPoP proof");
    vec![
        ("x-client-key", client_key.to_string()),
        ("authorization", format!("DPoP {}", access_token)),
        ("dpop", proof),
    ]
}

/// Make an authenticated POST request.
async fn dpop_post(
    app: &common::app::TestApp,
    path: &str,
    body: &Value,
    client_key: &str,
    dpop_key: &Value,
    access_token: &str,
) -> axum::response::Response {
    let url = format!("http://127.0.0.1:0{}", path);
    let headers = dpop_auth_headers(client_key, dpop_key, access_token, "POST", &url);
    let str_headers: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let req = post_json_with_headers(path, body, str_headers);
    app.router.clone().oneshot(req).await.unwrap()
}

/// Make an authenticated GET request.
async fn dpop_get(
    app: &common::app::TestApp,
    path: &str,
    client_key: &str,
    dpop_key: &Value,
    access_token: &str,
) -> axum::response::Response {
    let url = format!("http://127.0.0.1:0{}", path);
    let headers = dpop_auth_headers(client_key, dpop_key, access_token, "GET", &url);
    let str_headers: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let req = get_with_headers(path, str_headers);
    app.router.clone().oneshot(req).await.unwrap()
}

/// Register a DPoP session for a target DID using the same API client.
/// This simulates the client completing OAuth for the target account.
async fn register_target_session(
    app: &common::app::TestApp,
    client_key: &str,
    client_secret: &str,
    target_did: &str,
) {
    // Look up the client secret from the DB — we need it for the dpop-keys endpoint.
    // Actually, setup_dpop_session already provisions a key, but we need a separate session
    // for the target DID under the same api_client.
    // We can reuse the same provision_id (DPoP key) — register another session with a
    // different DID.

    // Provision a new DPoP key for this target session
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

    let access_token = format!("test-target-access-{}", uuid::Uuid::new_v4());
    let session_req = post_json_with_headers(
        "/oauth/sessions",
        &json!({
            "provision_id": provision_id,
            "did": target_did,
            "access_token": &access_token,
            "scopes": "atproto",
            "pds_url": "https://pds.example.com",
        }),
        vec![
            ("x-client-key", client_key),
            ("x-client-secret", client_secret),
        ],
    );
    let session_resp = app.router.clone().oneshot(session_req).await.unwrap();
    assert_eq!(
        session_resp.status(),
        StatusCode::CREATED,
        "failed to register target session"
    );
}

/// Full setup: create an API client, register DPoP sessions for both the owner
/// and the target account, then call linkAccount.
/// Returns `(client_key, client_secret, dpop_key, access_token)`.
async fn setup_linked_account(
    app: &common::app::TestApp,
    owner_did: &str,
    target_did: &str,
) -> (String, String, Value, String) {
    let (client_key, client_secret, _id) = app.create_api_client("confidential", None).await;

    // Provision DPoP key + session for the owner
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

    let access_token = format!("test-owner-access-{}", uuid::Uuid::new_v4());
    let session_req = post_json_with_headers(
        "/oauth/sessions",
        &json!({
            "provision_id": provision_id,
            "did": owner_did,
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
    assert_eq!(session_resp.status(), StatusCode::CREATED);

    // Register a session for the target account under the same API client
    register_target_session(app, &client_key, &client_secret, target_did).await;

    // Link the account
    let resp = dpop_post(
        app,
        "/xrpc/dev.happyview.delegation.linkAccount",
        &json!({ "did": target_did }),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED, "linkAccount failed");

    (client_key, client_secret, dpop_key, access_token)
}

/// Provision a DPoP session for a user under an existing API client.
/// Returns `(dpop_key, access_token)` — use the shared `client_key` for requests.
async fn setup_session_for_client(
    app: &common::app::TestApp,
    client_key: &str,
    client_secret: &str,
    user_did: &str,
) -> (Value, String) {
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
            ("x-client-key", client_key),
            ("x-client-secret", client_secret),
        ],
    );
    let session_resp = app.router.clone().oneshot(session_req).await.unwrap();
    assert_eq!(session_resp.status(), StatusCode::CREATED);

    (dpop_key, access_token)
}

// ---------------------------------------------------------------------------
// linkAccount
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn link_account_success() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let owner_did = "did:plc:owner1";
    let target_did = "did:plc:studio1";

    let (client_key, _client_secret, dpop_key, access_token) =
        setup_linked_account(&app, owner_did, target_did).await;

    // Verify via getAccount
    let resp = dpop_get(
        &app,
        &format!(
            "/xrpc/dev.happyview.delegation.getAccount?did={}",
            target_did
        ),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_json(resp).await;
    assert_eq!(body["did"], target_did);
    assert_eq!(body["role"], "owner");
    assert_eq!(body["linkedBy"], owner_did);
}

#[tokio::test]
#[serial]
async fn link_account_already_linked() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let owner_did = "did:plc:owner2";
    let target_did = "did:plc:studio2";

    let (client_key, _client_secret, dpop_key, access_token) =
        setup_linked_account(&app, owner_did, target_did).await;

    // Try to link again
    let resp = dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.linkAccount",
        &json!({ "did": target_did }),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
#[serial]
async fn link_account_self_link_rejected() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let user_did = "did:plc:selflinker";

    let (client_key, dpop_key, access_token) = setup_dpop_session(&app, user_did).await;

    let resp = dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.linkAccount",
        &json!({ "did": user_did }),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[serial]
async fn link_account_no_session_for_target() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let owner_did = "did:plc:owner3";
    let target_did = "did:plc:nosession";

    let (client_key, dpop_key, access_token) = setup_dpop_session(&app, owner_did).await;

    // Don't register a session for target_did — should fail
    let resp = dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.linkAccount",
        &json!({ "did": target_did }),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// unlinkAccount
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn unlink_account_success() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let owner_did = "did:plc:unlink_owner";
    let target_did = "did:plc:unlink_studio";

    let (client_key, _client_secret, dpop_key, access_token) =
        setup_linked_account(&app, owner_did, target_did).await;

    let resp = dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.unlinkAccount",
        &json!({ "did": target_did }),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Verify account is gone
    let resp = dpop_get(
        &app,
        &format!(
            "/xrpc/dev.happyview.delegation.getAccount?did={}",
            target_did
        ),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[serial]
async fn unlink_account_non_owner_rejected() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let owner_did = "did:plc:unlink_owner2";
    let admin_did = "did:plc:unlink_admin2";
    let target_did = "did:plc:unlink_studio2";

    let (owner_key, owner_secret, owner_dpop, owner_token) =
        setup_linked_account(&app, owner_did, target_did).await;

    // Add an admin
    let resp = dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.addDelegate",
        &json!({ "accountDid": target_did, "userDid": admin_did, "role": "admin" }),
        &owner_key,
        &owner_dpop,
        &owner_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Admin tries to unlink — should be rejected (owner-only)
    let (admin_dpop, admin_token) =
        setup_session_for_client(&app, &owner_key, &owner_secret, admin_did).await;
    let resp = dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.unlinkAccount",
        &json!({ "did": target_did }),
        &owner_key,
        &admin_dpop,
        &admin_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// addDelegate
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn add_delegate_success() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let owner_did = "did:plc:add_owner";
    let member_did = "did:plc:add_member";
    let target_did = "did:plc:add_studio";

    let (client_key, _client_secret, dpop_key, access_token) =
        setup_linked_account(&app, owner_did, target_did).await;

    let resp = dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.addDelegate",
        &json!({ "accountDid": target_did, "userDid": member_did, "role": "member" }),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Verify via listDelegates
    let resp = dpop_get(
        &app,
        &format!(
            "/xrpc/dev.happyview.delegation.listDelegates?accountDid={}",
            target_did
        ),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_json(resp).await;
    let delegates = body["delegates"].as_array().unwrap();
    assert_eq!(delegates.len(), 2); // owner + member
    let member = delegates
        .iter()
        .find(|d| d["userDid"] == member_did)
        .unwrap();
    assert_eq!(member["role"], "member");
    assert_eq!(member["grantedBy"], owner_did);
}

#[tokio::test]
#[serial]
async fn add_delegate_owner_role_rejected() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let owner_did = "did:plc:add_owner2";
    let target_did = "did:plc:add_studio2";

    let (client_key, _client_secret, dpop_key, access_token) =
        setup_linked_account(&app, owner_did, target_did).await;

    let resp = dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.addDelegate",
        &json!({ "accountDid": target_did, "userDid": "did:plc:someone", "role": "owner" }),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[serial]
async fn add_delegate_already_exists() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let owner_did = "did:plc:add_owner3";
    let member_did = "did:plc:add_member3";
    let target_did = "did:plc:add_studio3";

    let (client_key, _client_secret, dpop_key, access_token) =
        setup_linked_account(&app, owner_did, target_did).await;

    // Add member
    let resp = dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.addDelegate",
        &json!({ "accountDid": target_did, "userDid": member_did, "role": "member" }),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Try to add again
    let resp = dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.addDelegate",
        &json!({ "accountDid": target_did, "userDid": member_did, "role": "admin" }),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
#[serial]
async fn add_delegate_member_cannot_add() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let owner_did = "did:plc:add_owner4";
    let member_did = "did:plc:add_member4";
    let target_did = "did:plc:add_studio4";

    let (owner_key, owner_secret, owner_dpop, owner_token) =
        setup_linked_account(&app, owner_did, target_did).await;

    // Add a member
    let resp = dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.addDelegate",
        &json!({ "accountDid": target_did, "userDid": member_did, "role": "member" }),
        &owner_key,
        &owner_dpop,
        &owner_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Member tries to add another delegate — should fail (members can't manage)
    let (member_dpop, member_token) =
        setup_session_for_client(&app, &owner_key, &owner_secret, member_did).await;
    let resp = dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.addDelegate",
        &json!({ "accountDid": target_did, "userDid": "did:plc:someone", "role": "member" }),
        &owner_key,
        &member_dpop,
        &member_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// removeDelegate
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn remove_delegate_success() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let owner_did = "did:plc:rm_owner";
    let member_did = "did:plc:rm_member";
    let target_did = "did:plc:rm_studio";

    let (client_key, _client_secret, dpop_key, access_token) =
        setup_linked_account(&app, owner_did, target_did).await;

    // Add then remove a member
    dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.addDelegate",
        &json!({ "accountDid": target_did, "userDid": member_did, "role": "member" }),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;

    let resp = dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.removeDelegate",
        &json!({ "accountDid": target_did, "userDid": member_did }),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Verify only owner remains
    let resp = dpop_get(
        &app,
        &format!(
            "/xrpc/dev.happyview.delegation.listDelegates?accountDid={}",
            target_did
        ),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    let body = response_json(resp).await;
    let delegates = body["delegates"].as_array().unwrap();
    assert_eq!(delegates.len(), 1);
    assert_eq!(delegates[0]["role"], "owner");
}

#[tokio::test]
#[serial]
async fn remove_delegate_cannot_remove_owner() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let owner_did = "did:plc:rm_owner2";
    let target_did = "did:plc:rm_studio2";

    let (client_key, _client_secret, dpop_key, access_token) =
        setup_linked_account(&app, owner_did, target_did).await;

    let resp = dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.removeDelegate",
        &json!({ "accountDid": target_did, "userDid": owner_did }),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
#[serial]
async fn remove_delegate_admin_cannot_remove_admin() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let owner_did = "did:plc:rm_owner3";
    let admin1_did = "did:plc:rm_admin3a";
    let admin2_did = "did:plc:rm_admin3b";
    let target_did = "did:plc:rm_studio3";

    let (owner_key, owner_secret, owner_dpop, owner_token) =
        setup_linked_account(&app, owner_did, target_did).await;

    // Add two admins
    dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.addDelegate",
        &json!({ "accountDid": target_did, "userDid": admin1_did, "role": "admin" }),
        &owner_key,
        &owner_dpop,
        &owner_token,
    )
    .await;
    dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.addDelegate",
        &json!({ "accountDid": target_did, "userDid": admin2_did, "role": "admin" }),
        &owner_key,
        &owner_dpop,
        &owner_token,
    )
    .await;

    // Admin1 tries to remove admin2
    let (a1_dpop, a1_token) =
        setup_session_for_client(&app, &owner_key, &owner_secret, admin1_did).await;
    let resp = dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.removeDelegate",
        &json!({ "accountDid": target_did, "userDid": admin2_did }),
        &owner_key,
        &a1_dpop,
        &a1_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// listAccounts
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn list_accounts_returns_linked_accounts() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let owner_did = "did:plc:list_owner";
    let studio1 = "did:plc:list_studio1";
    let studio2 = "did:plc:list_studio2";

    let (client_key, client_secret, dpop_key, access_token) =
        setup_linked_account(&app, owner_did, studio1).await;

    // Link a second account under the same API client
    register_target_session(&app, &client_key, &client_secret, studio2).await;
    let resp = dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.linkAccount",
        &json!({ "did": studio2 }),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // List accounts — should include both
    let resp = dpop_get(
        &app,
        "/xrpc/dev.happyview.delegation.listAccounts",
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_json(resp).await;
    let accounts = body["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 2);
}

#[tokio::test]
#[serial]
async fn list_accounts_empty() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let user_did = "did:plc:no_accounts";

    let (client_key, dpop_key, access_token) = setup_dpop_session(&app, user_did).await;

    let resp = dpop_get(
        &app,
        "/xrpc/dev.happyview.delegation.listAccounts",
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_json(resp).await;
    let accounts = body["accounts"].as_array().unwrap();
    assert!(accounts.is_empty());
}

// ---------------------------------------------------------------------------
// getAccount
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn get_account_not_a_delegate() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let owner_did = "did:plc:ga_owner";
    let target_did = "did:plc:ga_studio";

    let (owner_key, owner_secret, _owner_dpop, _owner_token) =
        setup_linked_account(&app, owner_did, target_did).await;

    // Different user (same app, but not a delegate) tries to get account details
    let outsider_did = "did:plc:ga_outsider";
    let (out_dpop, out_token) =
        setup_session_for_client(&app, &owner_key, &owner_secret, outsider_did).await;
    let resp = dpop_get(
        &app,
        &format!(
            "/xrpc/dev.happyview.delegation.getAccount?did={}",
            target_did
        ),
        &owner_key,
        &out_dpop,
        &out_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// listDelegates
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn list_delegates_member_cannot_list() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let owner_did = "did:plc:ld_owner";
    let member_did = "did:plc:ld_member";
    let target_did = "did:plc:ld_studio";

    let (owner_key, owner_secret, owner_dpop, owner_token) =
        setup_linked_account(&app, owner_did, target_did).await;

    // Add a member
    dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.addDelegate",
        &json!({ "accountDid": target_did, "userDid": member_did, "role": "member" }),
        &owner_key,
        &owner_dpop,
        &owner_token,
    )
    .await;

    // Member tries to list delegates (same client, but member role can't list)
    let (member_dpop, member_token) =
        setup_session_for_client(&app, &owner_key, &owner_secret, member_did).await;
    let resp = dpop_get(
        &app,
        &format!(
            "/xrpc/dev.happyview.delegation.listDelegates?accountDid={}",
            target_did
        ),
        &owner_key,
        &member_dpop,
        &member_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// DelegateRole unit tests (no TestApp needed)
// ---------------------------------------------------------------------------

#[test]
fn delegate_role_from_str_roundtrip() {
    use happyview::delegation::DelegateRole;
    for role in &[
        DelegateRole::Owner,
        DelegateRole::Admin,
        DelegateRole::Member,
    ] {
        let s = role.as_str();
        assert_eq!(DelegateRole::from_str(s), Some(*role));
    }
    assert_eq!(DelegateRole::from_str("invalid"), None);
}

#[test]
fn delegate_role_can_write() {
    use happyview::delegation::DelegateRole;
    assert!(DelegateRole::Owner.can_write());
    assert!(DelegateRole::Admin.can_write());
    assert!(!DelegateRole::Member.can_write());
}

#[test]
fn delegate_role_can_manage_members() {
    use happyview::delegation::DelegateRole;
    assert!(DelegateRole::Owner.can_manage_members());
    assert!(DelegateRole::Admin.can_manage_members());
    assert!(!DelegateRole::Member.can_manage_members());
}

// ---------------------------------------------------------------------------
// Helpers for delegated write tests
// ---------------------------------------------------------------------------

fn admin_post_request(
    uri: &str,
    cookie: (axum::http::HeaderName, axum::http::HeaderValue),
    body: &Value,
) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(cookie.0, cookie.1)
        .header("content-type", "application/json")
        .header("host", "127.0.0.1:0")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

async fn seed_procedure_lexicon(app: &common::app::TestApp) {
    let resp = app
        .router
        .clone()
        .oneshot(admin_post_request(
            "/admin/lexicons",
            app.admin_cookie(),
            &json!({
                "lexicon_json": common::fixtures::create_game_procedure_lexicon(),
                "target_collection": "games.gamesgamesgamesgames.game"
            }),
        ))
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "failed to seed procedure lexicon: {}",
        resp.status()
    );
}

async fn update_session_pds_url(app: &common::app::TestApp, user_did: &str, pds_url: &str) {
    let sql = adapt_sql(
        "UPDATE happyview_dpop_sessions SET pds_url = ? WHERE user_did = ?",
        app.state.db_backend,
    );
    sqlx::query(&sql)
        .bind(pds_url)
        .bind(user_did)
        .execute(&app.state.db)
        .await
        .expect("failed to update session pds_url");
}

// ---------------------------------------------------------------------------
// Delegated writes — auth gates
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn delegated_write_non_delegate_rejected() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    seed_procedure_lexicon(&app).await;

    let owner_did = "did:plc:dw_owner1";
    let target_did = "did:plc:dw_studio1";
    setup_linked_account(&app, owner_did, target_did).await;

    // Outsider (not a delegate) tries a delegated write
    let outsider_did = "did:plc:dw_outsider1";
    let (out_key, out_dpop, out_token) = setup_dpop_session(&app, outsider_did).await;

    let resp = dpop_post(
        &app,
        "/xrpc/games.gamesgamesgamesgames.createGame",
        &json!({ "title": "Hacked Game", "delegateDid": target_did }),
        &out_key,
        &out_dpop,
        &out_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
#[serial]
async fn delegated_write_member_rejected() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    seed_procedure_lexicon(&app).await;

    let owner_did = "did:plc:dw_owner2";
    let member_did = "did:plc:dw_member2";
    let target_did = "did:plc:dw_studio2";

    let (owner_key, owner_secret, owner_dpop, owner_token) =
        setup_linked_account(&app, owner_did, target_did).await;

    // Add a member (cannot write)
    let resp = dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.addDelegate",
        &json!({ "accountDid": target_did, "userDid": member_did, "role": "member" }),
        &owner_key,
        &owner_dpop,
        &owner_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Member tries a delegated write (same client, but member role can't write)
    let (mem_dpop, mem_token) =
        setup_session_for_client(&app, &owner_key, &owner_secret, member_did).await;
    let resp = dpop_post(
        &app,
        "/xrpc/games.gamesgamesgamesgames.createGame",
        &json!({ "title": "Member Game", "delegateDid": target_did }),
        &owner_key,
        &mem_dpop,
        &mem_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// Delegated writes — happy path
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn delegated_write_owner_success() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    seed_procedure_lexicon(&app).await;

    let owner_did = "did:plc:dw_owner3";
    let target_did = "did:plc:dw_studio3";

    let (owner_key, _owner_secret, owner_dpop, owner_token) =
        setup_linked_account(&app, owner_did, target_did).await;

    // Point the target's DPoP session at the mock server so dpop_pds_post
    // reaches wiremock instead of a real PDS.
    let mock_url = app.mock_server.uri();
    update_session_pds_url(&app, target_did, &mock_url).await;

    // Mock PDS createRecord
    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.repo.createRecord"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "uri": format!("at://{target_did}/games.gamesgamesgamesgames.game/abc123"),
            "cid": "bafytest123"
        })))
        .expect(1)
        .mount(&app.mock_server)
        .await;

    let resp = dpop_post(
        &app,
        "/xrpc/games.gamesgamesgamesgames.createGame",
        &json!({ "title": "Studio Game", "delegateDid": target_did }),
        &owner_key,
        &owner_dpop,
        &owner_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body = response_json(resp).await;
    assert_eq!(
        body["uri"],
        format!("at://{target_did}/games.gamesgamesgamesgames.game/abc123")
    );
    assert_eq!(body["cid"], "bafytest123");
}

#[tokio::test]
#[serial]
async fn delegated_write_admin_success() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    seed_procedure_lexicon(&app).await;

    let owner_did = "did:plc:dw_owner4";
    let admin_did = "did:plc:dw_admin4";
    let target_did = "did:plc:dw_studio4";

    let (owner_key, owner_secret, owner_dpop, owner_token) =
        setup_linked_account(&app, owner_did, target_did).await;

    // Add an admin
    let resp = dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.addDelegate",
        &json!({ "accountDid": target_did, "userDid": admin_did, "role": "admin" }),
        &owner_key,
        &owner_dpop,
        &owner_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Point the target's DPoP session at the mock server
    let mock_url = app.mock_server.uri();
    update_session_pds_url(&app, target_did, &mock_url).await;

    // Mock PDS createRecord
    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.repo.createRecord"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "uri": format!("at://{target_did}/games.gamesgamesgamesgames.game/def456"),
            "cid": "bafyadmin456"
        })))
        .expect(1)
        .mount(&app.mock_server)
        .await;

    // Admin does a delegated write (same client)
    let (adm_dpop, adm_token) =
        setup_session_for_client(&app, &owner_key, &owner_secret, admin_did).await;
    let resp = dpop_post(
        &app,
        "/xrpc/games.gamesgamesgamesgames.createGame",
        &json!({ "title": "Admin Game", "delegateDid": target_did }),
        &owner_key,
        &adm_dpop,
        &adm_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body = response_json(resp).await;
    assert_eq!(body["cid"], "bafyadmin456");
}

// ---------------------------------------------------------------------------
// Positive-path coverage for admin / member operations
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn admin_can_add_delegate() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;

    let owner_did = "did:plc:acd_owner";
    let admin_did = "did:plc:acd_admin";
    let new_member_did = "did:plc:acd_newmember";
    let target_did = "did:plc:acd_studio";

    let (owner_key, owner_secret, owner_dpop, owner_token) =
        setup_linked_account(&app, owner_did, target_did).await;

    // Owner adds admin
    let resp = dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.addDelegate",
        &json!({ "accountDid": target_did, "userDid": admin_did, "role": "admin" }),
        &owner_key,
        &owner_dpop,
        &owner_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Admin adds a member (same client)
    let (adm_dpop, adm_token) =
        setup_session_for_client(&app, &owner_key, &owner_secret, admin_did).await;
    let resp = dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.addDelegate",
        &json!({ "accountDid": target_did, "userDid": new_member_did, "role": "member" }),
        &owner_key,
        &adm_dpop,
        &adm_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Verify the member exists via listDelegates (as owner)
    let resp = dpop_get(
        &app,
        &format!(
            "/xrpc/dev.happyview.delegation.listDelegates?accountDid={}",
            target_did
        ),
        &owner_key,
        &owner_dpop,
        &owner_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_json(resp).await;
    let delegates = body["delegates"].as_array().unwrap();
    assert_eq!(delegates.len(), 3); // owner + admin + member
    let member = delegates
        .iter()
        .find(|d| d["userDid"] == new_member_did)
        .unwrap();
    assert_eq!(member["role"], "member");
    assert_eq!(member["grantedBy"], admin_did);
}

#[tokio::test]
#[serial]
async fn admin_can_remove_member() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;

    let owner_did = "did:plc:arm_owner";
    let admin_did = "did:plc:arm_admin";
    let member_did = "did:plc:arm_member";
    let target_did = "did:plc:arm_studio";

    let (owner_key, owner_secret, owner_dpop, owner_token) =
        setup_linked_account(&app, owner_did, target_did).await;

    // Owner adds admin and member
    dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.addDelegate",
        &json!({ "accountDid": target_did, "userDid": admin_did, "role": "admin" }),
        &owner_key,
        &owner_dpop,
        &owner_token,
    )
    .await;
    dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.addDelegate",
        &json!({ "accountDid": target_did, "userDid": member_did, "role": "member" }),
        &owner_key,
        &owner_dpop,
        &owner_token,
    )
    .await;

    // Admin removes member (same client)
    let (adm_dpop, adm_token) =
        setup_session_for_client(&app, &owner_key, &owner_secret, admin_did).await;
    let resp = dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.removeDelegate",
        &json!({ "accountDid": target_did, "userDid": member_did }),
        &owner_key,
        &adm_dpop,
        &adm_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Verify member is gone
    let resp = dpop_get(
        &app,
        &format!(
            "/xrpc/dev.happyview.delegation.listDelegates?accountDid={}",
            target_did
        ),
        &owner_key,
        &owner_dpop,
        &owner_token,
    )
    .await;
    let body = response_json(resp).await;
    let delegates = body["delegates"].as_array().unwrap();
    assert_eq!(delegates.len(), 2); // owner + admin only
    assert!(delegates.iter().all(|d| d["userDid"] != member_did));
}

#[tokio::test]
#[serial]
async fn admin_can_list_delegates() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;

    let owner_did = "did:plc:ald_owner";
    let admin_did = "did:plc:ald_admin";
    let target_did = "did:plc:ald_studio";

    let (owner_key, owner_secret, owner_dpop, owner_token) =
        setup_linked_account(&app, owner_did, target_did).await;

    // Owner adds admin
    dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.addDelegate",
        &json!({ "accountDid": target_did, "userDid": admin_did, "role": "admin" }),
        &owner_key,
        &owner_dpop,
        &owner_token,
    )
    .await;

    // Admin lists delegates (same client)
    let (adm_dpop, adm_token) =
        setup_session_for_client(&app, &owner_key, &owner_secret, admin_did).await;
    let resp = dpop_get(
        &app,
        &format!(
            "/xrpc/dev.happyview.delegation.listDelegates?accountDid={}",
            target_did
        ),
        &owner_key,
        &adm_dpop,
        &adm_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_json(resp).await;
    let delegates = body["delegates"].as_array().unwrap();
    assert_eq!(delegates.len(), 2); // owner + admin
}

#[tokio::test]
#[serial]
async fn member_can_view_account() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;

    let owner_did = "did:plc:mva_owner";
    let member_did = "did:plc:mva_member";
    let target_did = "did:plc:mva_studio";

    let (owner_key, owner_secret, owner_dpop, owner_token) =
        setup_linked_account(&app, owner_did, target_did).await;

    // Owner adds member
    dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.addDelegate",
        &json!({ "accountDid": target_did, "userDid": member_did, "role": "member" }),
        &owner_key,
        &owner_dpop,
        &owner_token,
    )
    .await;

    // Member calls getAccount (same client)
    let (mem_dpop, mem_token) =
        setup_session_for_client(&app, &owner_key, &owner_secret, member_did).await;
    let resp = dpop_get(
        &app,
        &format!(
            "/xrpc/dev.happyview.delegation.getAccount?did={}",
            target_did
        ),
        &owner_key,
        &mem_dpop,
        &mem_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_json(resp).await;
    assert_eq!(body["did"], target_did);
    assert_eq!(body["role"], "member");
}

#[tokio::test]
#[serial]
async fn owner_can_remove_admin() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;

    let owner_did = "did:plc:ora_owner";
    let admin_did = "did:plc:ora_admin";
    let target_did = "did:plc:ora_studio";

    let (owner_key, _owner_secret, owner_dpop, owner_token) =
        setup_linked_account(&app, owner_did, target_did).await;

    // Owner adds admin
    dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.addDelegate",
        &json!({ "accountDid": target_did, "userDid": admin_did, "role": "admin" }),
        &owner_key,
        &owner_dpop,
        &owner_token,
    )
    .await;

    // Owner removes admin
    let resp = dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.removeDelegate",
        &json!({ "accountDid": target_did, "userDid": admin_did }),
        &owner_key,
        &owner_dpop,
        &owner_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Verify only owner remains
    let resp = dpop_get(
        &app,
        &format!(
            "/xrpc/dev.happyview.delegation.listDelegates?accountDid={}",
            target_did
        ),
        &owner_key,
        &owner_dpop,
        &owner_token,
    )
    .await;
    let body = response_json(resp).await;
    let delegates = body["delegates"].as_array().unwrap();
    assert_eq!(delegates.len(), 1);
    assert_eq!(delegates[0]["role"], "owner");
}

// ---------------------------------------------------------------------------
// Cross-client scoping — operations from a different API client are rejected
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn cross_client_get_account_rejected() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let owner_did = "did:plc:xc_ga_owner";
    let target_did = "did:plc:xc_ga_studio";

    setup_linked_account(&app, owner_did, target_did).await;

    // Different API client tries to access the account
    let (other_key, other_dpop, other_token) = setup_dpop_session(&app, owner_did).await;
    let resp = dpop_get(
        &app,
        &format!(
            "/xrpc/dev.happyview.delegation.getAccount?did={}",
            target_did
        ),
        &other_key,
        &other_dpop,
        &other_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
#[serial]
async fn cross_client_add_delegate_rejected() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let owner_did = "did:plc:xc_ad_owner";
    let target_did = "did:plc:xc_ad_studio";

    setup_linked_account(&app, owner_did, target_did).await;

    // Different API client tries to add a delegate
    let (other_key, other_dpop, other_token) = setup_dpop_session(&app, owner_did).await;
    let resp = dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.addDelegate",
        &json!({ "accountDid": target_did, "userDid": "did:plc:xc_someone", "role": "member" }),
        &other_key,
        &other_dpop,
        &other_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
#[serial]
async fn cross_client_delegated_write_rejected() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    seed_procedure_lexicon(&app).await;

    let owner_did = "did:plc:xc_dw_owner";
    let admin_did = "did:plc:xc_dw_admin";
    let target_did = "did:plc:xc_dw_studio";

    let (owner_key, _owner_secret, owner_dpop, owner_token) =
        setup_linked_account(&app, owner_did, target_did).await;

    // Add an admin under the correct client
    dpop_post(
        &app,
        "/xrpc/dev.happyview.delegation.addDelegate",
        &json!({ "accountDid": target_did, "userDid": admin_did, "role": "admin" }),
        &owner_key,
        &owner_dpop,
        &owner_token,
    )
    .await;

    // Admin authenticates via a different API client and tries a delegated write
    let (other_key, other_dpop, other_token) = setup_dpop_session(&app, admin_did).await;
    let resp = dpop_post(
        &app,
        "/xrpc/games.gamesgamesgamesgames.createGame",
        &json!({ "title": "Cross-client Game", "delegateDid": target_did }),
        &other_key,
        &other_dpop,
        &other_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
#[serial]
async fn cross_client_list_accounts_isolated() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let owner_did = "did:plc:xc_la_owner";
    let studio1 = "did:plc:xc_la_studio1";
    let studio2 = "did:plc:xc_la_studio2";

    // Link studio1 under client A
    setup_linked_account(&app, owner_did, studio1).await;

    // Link studio2 under client B (different API client)
    let (client_b_key, _client_b_secret, client_b_dpop, client_b_token) =
        setup_linked_account(&app, owner_did, studio2).await;

    // listAccounts from client B should only show studio2
    let resp = dpop_get(
        &app,
        "/xrpc/dev.happyview.delegation.listAccounts",
        &client_b_key,
        &client_b_dpop,
        &client_b_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_json(resp).await;
    let accounts = body["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0]["did"], studio2);
}
