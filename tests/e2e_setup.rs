mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;
use common::plc;

async fn json_body(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

// ---------------------------------------------------------------------------
// Setup status defaults
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn setup_status_returns_defaults_when_no_identity() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .uri("/api/setup/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["setup_complete"], false);
    assert!(body["identity_mode"].is_null());
}

// ---------------------------------------------------------------------------
// Setup identity sets mode
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn setup_identity_sets_mode() {
    common::require_db!();
    let mut app = TestApp::new().await;
    app.state.config.token_encryption_key = Some([0x42u8; 32]);
    app.rebuild_router();

    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .method("POST")
                .uri("/api/setup/identity")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"mode": "did_web"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .uri("/api/setup/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["identity_mode"], "did_web");
    assert_eq!(body["identity_configured"], true);
}

// ---------------------------------------------------------------------------
// Setup identity rejects when complete
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn setup_identity_rejects_when_complete() {
    common::require_db!();
    let mut app = TestApp::new().await;
    app.setup_did_web().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .method("POST")
                .uri("/api/setup/identity")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"mode": "did_plc"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "setup identity should reject when setup is complete"
    );
}

// ---------------------------------------------------------------------------
// Setup complete marks done
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn setup_complete_marks_done() {
    common::require_db!();
    let app = TestApp::new().await;

    // Set identity to not_exposed (no encryption key needed)
    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .method("POST")
                .uri("/api/setup/identity")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"mode": "not_exposed"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Mark complete
    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .method("POST")
                .uri("/api/setup/complete")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Verify status
    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .uri("/api/setup/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["setup_complete"], true);
}

// ---------------------------------------------------------------------------
// Rotation key export
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn rotation_key_export() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let encryption_key = [0x42u8; 32];
    app.state.config.token_encryption_key = Some(encryption_key);
    app.rebuild_router();

    // Set identity to did_plc via the endpoint (which generates both keys)
    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .method("POST")
                .uri("/api/setup/identity")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"mode": "did_plc"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Do NOT mark setup complete -- rotation key export requires setup incomplete

    // GET rotation key
    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .uri("/api/setup/rotation-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "rotation key export should succeed for did_plc"
    );

    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(
        !body_bytes.is_empty(),
        "rotation key response should have binary content"
    );
    // The decrypted key should be 32 bytes (P-256 private key)
    assert_eq!(body_bytes.len(), 32, "rotation key should be 32 bytes");
}

// ---------------------------------------------------------------------------
// Rotation key export rejects non-PLC
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn rotation_key_export_rejects_non_plc() {
    common::require_db!();
    let mut app = TestApp::new().await;
    app.state.config.token_encryption_key = Some([0x42u8; 32]);
    app.rebuild_router();

    // Set identity to did_web (not complete -- so guard passes but mode check fails)
    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .method("POST")
                .uri("/api/setup/identity")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"mode": "did_web"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // GET rotation key should fail for did_web
    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .uri("/api/setup/rotation-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "rotation key export should reject non-plc mode"
    );
}

// ---------------------------------------------------------------------------
// Resolve identity endpoint
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn resolve_identity_empty_query_returns_empty() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .uri("/api/setup/resolve?q=")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    let results = body.as_array().unwrap();
    assert!(results.is_empty(), "empty query should return empty array");
}

#[tokio::test]
#[serial]
async fn resolve_identity_with_did_returns_result() {
    common::require_db!();
    let app = TestApp::new().await;

    // Resolving a DID that doesn't exist should still return the DID as-is (fallback path)
    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .uri("/api/setup/resolve?q=did%3Aplc%3Atestresolver")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    let results = body.as_array().unwrap();
    assert_eq!(results.len(), 1, "DID input should return one result");
    assert_eq!(results[0]["did"], "did:plc:testresolver");
}

// ---------------------------------------------------------------------------
// PLC register endpoint
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn plc_register_creates_did() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let plc_store = plc::setup_mock_plc(&app.mock_server).await;

    app.state.config.token_encryption_key = Some([0x42u8; 32]);
    app.rebuild_router();

    // Set identity to did_plc via the setup endpoint (generates both keys)
    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .method("POST")
                .uri("/api/setup/identity")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"mode": "did_plc"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Register the DID via the PLC directory
    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .method("POST")
                .uri("/api/setup/plc/register")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "plc_register should return 200 with the DID"
    );
    let body = json_body(resp).await;
    let did = body["did"].as_str().unwrap();
    assert!(
        did.starts_with("did:plc:"),
        "DID should start with did:plc:"
    );

    // Verify the PLC store received the genesis document
    let store = plc_store.read().await;
    assert!(
        store.contains_key(did),
        "PLC store should contain the registered DID"
    );
    let genesis = store.get(did).unwrap();
    assert_eq!(genesis["type"], "plc_operation");
    assert!(genesis["sig"].is_string(), "genesis should be signed");
}

#[tokio::test]
#[serial]
async fn plc_register_rejects_non_plc_mode() {
    common::require_db!();
    let mut app = TestApp::new().await;
    app.state.config.token_encryption_key = Some([0x42u8; 32]);
    app.rebuild_router();

    // Set identity to did_web
    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .method("POST")
                .uri("/api/setup/identity")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"mode": "did_web"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .method("POST")
                .uri("/api/setup/plc/register")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "plc_register should reject non-plc mode"
    );
}

#[tokio::test]
#[serial]
async fn plc_register_rejects_duplicate() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let _plc_store = plc::setup_mock_plc(&app.mock_server).await;

    app.state.config.token_encryption_key = Some([0x42u8; 32]);
    app.rebuild_router();

    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .method("POST")
                .uri("/api/setup/identity")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"mode": "did_plc"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // First registration succeeds
    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .method("POST")
                .uri("/api/setup/plc/register")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    // Second registration should fail (DID already set)
    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .method("POST")
                .uri("/api/setup/plc/register")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "duplicate plc_register should return 409"
    );
}

// ---------------------------------------------------------------------------
// Attach auth confirm endpoint
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn attach_auth_confirm_restores_cookie() {
    common::require_db!();
    let app = TestApp::new().await;

    // Set up attach_account mode with a known attached DID
    let attached_did = "did:plc:attachedaccount";
    happyview::service_identity::upsert_identity(
        &app.state.db,
        app.state.db_backend,
        &happyview::service_identity::IdentityMode::AttachAccount,
        None,
        None,
        None,
        Some(attached_did),
    )
    .await
    .unwrap();

    // Build a cookie as the attached account (simulating post-OAuth state)
    let attached_cookie =
        crate::common::auth::admin_cookie_header(attached_did, &app.state.cookie_key);

    // POST attach-auth/confirm to restore the admin's cookie
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/setup/attach-auth/confirm")
                .header(attached_cookie.0, attached_cookie.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "original_did": &app.admin_did
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "attach_auth_confirm should return 204"
    );

    // Verify the Set-Cookie header is present (cookie was restored)
    assert!(
        resp.headers().contains_key("set-cookie"),
        "response should set a new cookie"
    );
}

#[tokio::test]
#[serial]
async fn attach_auth_confirm_rejects_invalid_original_did() {
    common::require_db!();
    let app = TestApp::new().await;

    let attached_did = "did:plc:attachedaccount2";
    happyview::service_identity::upsert_identity(
        &app.state.db,
        app.state.db_backend,
        &happyview::service_identity::IdentityMode::AttachAccount,
        None,
        None,
        None,
        Some(attached_did),
    )
    .await
    .unwrap();

    let attached_cookie =
        crate::common::auth::admin_cookie_header(attached_did, &app.state.cookie_key);

    // Try with empty original_did
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/setup/attach-auth/confirm")
                .header(attached_cookie.0, attached_cookie.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "original_did": "not-a-did"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "invalid original_did should be rejected"
    );
}

#[tokio::test]
#[serial]
async fn attach_auth_confirm_rejects_mismatched_session() {
    common::require_db!();
    let app = TestApp::new().await;

    let attached_did = "did:plc:attachedaccount3";
    happyview::service_identity::upsert_identity(
        &app.state.db,
        app.state.db_backend,
        &happyview::service_identity::IdentityMode::AttachAccount,
        None,
        None,
        None,
        Some(attached_did),
    )
    .await
    .unwrap();

    // Cookie is for the admin user, but attached_account_did is different
    let wrong_cookie = app.admin_cookie();

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/setup/attach-auth/confirm")
                .header(wrong_cookie.0, wrong_cookie.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "original_did": &app.admin_did
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "mismatched session should be rejected"
    );
}

// ---------------------------------------------------------------------------
// PLC request/submit — mode rejection
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn plc_request_rejects_non_attach_account_mode() {
    common::require_db!();
    let mut app = TestApp::new().await;
    app.state.config.token_encryption_key = Some([0x42u8; 32]);
    app.rebuild_router();

    // Set identity to did_web
    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .method("POST")
                .uri("/api/setup/identity")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"mode": "did_web"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .method("POST")
                .uri("/api/setup/plc/request")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "plc_request should reject non-attach_account mode"
    );
}

#[tokio::test]
#[serial]
async fn plc_submit_rejects_non_attach_account_mode() {
    common::require_db!();
    let mut app = TestApp::new().await;
    app.state.config.token_encryption_key = Some([0x42u8; 32]);
    app.rebuild_router();

    // Set identity to did_web
    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .method("POST")
                .uri("/api/setup/identity")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"mode": "did_web"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .method("POST")
                .uri("/api/setup/plc/submit")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"token": "fake-token"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "plc_submit should reject non-attach_account mode"
    );
}
