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
// Service entry CRUD via admin endpoints
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn create_list_update_delete_service_entry() {
    common::require_db!();
    let app = TestApp::new().await;
    let cookie = app.admin_cookie();

    // CREATE
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/service-entries")
                .header(cookie.0.clone(), cookie.1.clone())
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "fragment_id": "#chess",
                        "service_type": "ChessAppView"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);
    let created = json_body(resp).await;
    let entry_id = created["id"].as_i64().unwrap();
    assert_eq!(created["fragment_id"], "#chess");
    assert_eq!(created["service_type"], "ChessAppView");
    assert_eq!(created["access_mode"], "all");

    // LIST
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/service-entries")
                .header(cookie.0.clone(), cookie.1.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let list = json_body(resp).await;
    let entries = list.as_array().unwrap();
    assert!(entries.iter().any(|e| e["id"].as_i64() == Some(entry_id)));

    // UPDATE
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/admin/service-entries/{}", entry_id))
                .header(cookie.0.clone(), cookie.1.clone())
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "access_mode": "specific"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // DELETE
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/admin/service-entries/{}", entry_id))
                .header(cookie.0.clone(), cookie.1.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Verify deletion
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/service-entries")
                .header(cookie.0.clone(), cookie.1.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let list = json_body(resp).await;
    let entries = list.as_array().unwrap();
    assert!(!entries.iter().any(|e| e["id"].as_i64() == Some(entry_id)));
}

#[tokio::test]
#[serial]
async fn delete_nonexistent_entry_returns_404() {
    common::require_db!();
    let app = TestApp::new().await;
    let cookie = app.admin_cookie();

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/admin/service-entries/99999")
                .header(cookie.0, cookie.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[serial]
async fn update_entry_with_invalid_access_mode_returns_400() {
    common::require_db!();
    let app = TestApp::new().await;
    let cookie = app.admin_cookie();

    let entry_id = app
        .create_service_entry("#chess", "ChessAppView", "all")
        .await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/admin/service-entries/{}", entry_id))
                .header(cookie.0, cookie.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "access_mode": "invalid_mode"
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
        "invalid access_mode should return 400"
    );
}

// ---------------------------------------------------------------------------
// XRPC junction table via admin endpoints
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn add_list_remove_entry_xrpcs() {
    common::require_db!();
    let app = TestApp::new().await;
    let cookie = app.admin_cookie();

    let entry_id = app
        .create_service_entry("#chess", "ChessAppView", "specific")
        .await;

    // ADD xrpcs
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/admin/service-entries/{}/xrpcs", entry_id))
                .header(cookie.0.clone(), cookie.1.clone())
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "lexicon_ids": ["games.example.listGames", "games.example.getGame"]
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // LIST xrpcs
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/admin/service-entries/{}/xrpcs", entry_id))
                .header(cookie.0.clone(), cookie.1.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let xrpcs = json_body(resp).await;
    let list = xrpcs.as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert!(list.contains(&json!("games.example.getGame")));
    assert!(list.contains(&json!("games.example.listGames")));

    // REMOVE one xrpc
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/admin/service-entries/{}/xrpcs", entry_id))
                .header(cookie.0.clone(), cookie.1.clone())
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "lexicon_ids": ["games.example.getGame"]
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Verify removal
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/admin/service-entries/{}/xrpcs", entry_id))
                .header(cookie.0.clone(), cookie.1.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let xrpcs = json_body(resp).await;
    let list = xrpcs.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0], "games.example.listGames");
}

// ---------------------------------------------------------------------------
// Reverse lookup: services_for_lexicon
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn lexicon_services_reverse_lookup() {
    common::require_db!();
    let app = TestApp::new().await;
    let cookie = app.admin_cookie();

    let id_all = app
        .create_service_entry("#chess", "ChessAppView", "all")
        .await;
    let id_specific = app
        .create_service_entry("#checkers", "CheckersAppView", "specific")
        .await;
    app.add_entry_xrpcs(id_specific, &["games.example.listGames"])
        .await;

    // Both should appear for games.example.listGames (one via access_mode=all, one via junction)
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/lexicons/games.example.listGames/services")
                .header(cookie.0.clone(), cookie.1.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let services = json_body(resp).await;
    let list = services.as_array().unwrap();
    assert_eq!(list.len(), 2);
    let ids: Vec<i64> = list.iter().filter_map(|e| e["id"].as_i64()).collect();
    assert!(ids.contains(&id_all));
    assert!(ids.contains(&id_specific));

    // Only #chess (access_mode=all) should appear for a random XRPC
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/lexicons/games.example.unrelated/services")
                .header(cookie.0.clone(), cookie.1.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let services = json_body(resp).await;
    let list = services.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"].as_i64().unwrap(), id_all);
}

// ---------------------------------------------------------------------------
// Update entry with empty body — short-circuit path
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn update_entry_with_empty_body() {
    common::require_db!();
    let app = TestApp::new().await;
    let cookie = app.admin_cookie();

    let entry_id = app
        .create_service_entry("#empty", "EmptyUpdate", "all")
        .await;

    // Send an update with no fields — should succeed (no-op update)
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/admin/service-entries/{}", entry_id))
                .header(cookie.0.clone(), cookie.1.clone())
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&json!({})).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "empty update body should succeed"
    );

    // Verify entry is unchanged
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/service-entries")
                .header(cookie.0, cookie.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let list = json_body(resp).await;
    let entries = list.as_array().unwrap();
    let entry = entries
        .iter()
        .find(|e| e["id"].as_i64() == Some(entry_id))
        .unwrap();
    assert_eq!(entry["service_type"], "EmptyUpdate");
    assert_eq!(entry["access_mode"], "all");
}

// ---------------------------------------------------------------------------
// Non-admin permission checks
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn unauthenticated_list_entries_rejected() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/service-entries")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        resp.status().is_client_error(),
        "unauthenticated request to admin endpoint should be rejected, got {}",
        resp.status()
    );
}

#[tokio::test]
#[serial]
async fn unauthenticated_create_entry_rejected() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/service-entries")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "fragment_id": "#noauth",
                        "service_type": "NoAuth"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        resp.status().is_client_error(),
        "unauthenticated POST to admin endpoint should be rejected, got {}",
        resp.status()
    );
}

#[tokio::test]
#[serial]
async fn unauthenticated_delete_entry_rejected() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/admin/service-entries/1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        resp.status().is_client_error(),
        "unauthenticated DELETE to admin endpoint should be rejected, got {}",
        resp.status()
    );
}

#[tokio::test]
#[serial]
async fn unauthenticated_sync_plc_rejected() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/service-entries/sync-plc")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        resp.status().is_client_error(),
        "unauthenticated POST to sync-plc should be rejected, got {}",
        resp.status()
    );
}

// ---------------------------------------------------------------------------
// Update nonexistent entry returns 404
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn update_nonexistent_entry_returns_404() {
    common::require_db!();
    let app = TestApp::new().await;
    let cookie = app.admin_cookie();

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/admin/service-entries/99999")
                .header(cookie.0, cookie.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "access_mode": "specific"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "updating a nonexistent entry should return 404"
    );
}

// ---------------------------------------------------------------------------
// XRPC idempotency — adding the same NSID twice
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn add_entry_xrpcs_idempotent() {
    common::require_db!();
    let app = TestApp::new().await;
    let cookie = app.admin_cookie();

    let entry_id = app
        .create_service_entry("#chess", "ChessAppView", "specific")
        .await;

    let add_body = serde_json::to_vec(&json!({
        "lexicon_ids": ["games.example.listGames"]
    }))
    .unwrap();

    // Add once
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/admin/service-entries/{}/xrpcs", entry_id))
                .header(cookie.0.clone(), cookie.1.clone())
                .header("content-type", "application/json")
                .body(Body::from(add_body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Add the same NSID again — should succeed (ON CONFLICT DO NOTHING)
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/admin/service-entries/{}/xrpcs", entry_id))
                .header(cookie.0.clone(), cookie.1.clone())
                .header("content-type", "application/json")
                .body(Body::from(add_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "adding the same xrpc twice should succeed idempotently"
    );

    // Verify only one entry exists
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/admin/service-entries/{}/xrpcs", entry_id))
                .header(cookie.0, cookie.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let xrpcs = json_body(resp).await;
    assert_eq!(
        xrpcs.as_array().unwrap().len(),
        1,
        "should have exactly one entry after duplicate add"
    );
}

// ---------------------------------------------------------------------------
// Unauthenticated access to remaining endpoints
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn unauthenticated_update_entry_rejected() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/admin/service-entries/1")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"access_mode": "all"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        resp.status().is_client_error(),
        "unauthenticated PUT should be rejected, got {}",
        resp.status()
    );
}

#[tokio::test]
#[serial]
async fn unauthenticated_list_xrpcs_rejected() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/service-entries/1/xrpcs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        resp.status().is_client_error(),
        "unauthenticated GET xrpcs should be rejected, got {}",
        resp.status()
    );
}

#[tokio::test]
#[serial]
async fn unauthenticated_add_xrpcs_rejected() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/service-entries/1/xrpcs")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"lexicon_ids": ["test.foo.bar"]})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        resp.status().is_client_error(),
        "unauthenticated POST xrpcs should be rejected, got {}",
        resp.status()
    );
}

#[tokio::test]
#[serial]
async fn unauthenticated_remove_xrpcs_rejected() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/admin/service-entries/1/xrpcs")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"lexicon_ids": ["test.foo.bar"]})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        resp.status().is_client_error(),
        "unauthenticated DELETE xrpcs should be rejected, got {}",
        resp.status()
    );
}

#[tokio::test]
#[serial]
async fn unauthenticated_lexicon_services_rejected() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/lexicons/test.foo.bar/services")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        resp.status().is_client_error(),
        "unauthenticated lexicon services lookup should be rejected, got {}",
        resp.status()
    );
}

// ---------------------------------------------------------------------------
// PLC sync — did_plc mode
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn sync_plc_updates_did_document() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let plc_store = plc::setup_mock_plc(&app.mock_server).await;
    let did = app.setup_did_plc().await;

    // Generate and store a rotation key (setup_did_plc only stores a signing key)
    let encryption_key = [0x42u8; 32];

    let mut rotation_key_bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut rotation_key_bytes);
    let _rotation_key =
        p256::ecdsa::SigningKey::from_bytes((&rotation_key_bytes[..]).into()).unwrap();
    let encrypted = happyview::plugin::encryption::encrypt(&encryption_key, &rotation_key_bytes)
        .expect("encryption failed");
    let rotation_key_enc =
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &encrypted);

    let sql = happyview::db::adapt_sql(
        "UPDATE happyview_service_identity SET rotation_key_enc = ? WHERE id = 1",
        app.state.db_backend,
    );
    sqlx::query(&sql)
        .bind(&rotation_key_enc)
        .execute(&app.state.db)
        .await
        .expect("failed to store rotation key");

    // Build a genesis-like PLC document for the mock
    let rotation_did_key = happyview::plc::private_key_to_did_key(&rotation_key_bytes).unwrap();

    // Compute the signing key's did:key from the stored identity
    let identity = happyview::service_identity::get_identity(&app.state.db, app.state.db_backend)
        .await
        .unwrap()
        .unwrap();
    let signing_key_bytes =
        happyview::plc::decrypt_key(identity.signing_key_enc.as_ref().unwrap(), &encryption_key)
            .unwrap();
    let signing_did_key = happyview::plc::private_key_to_did_key(&signing_key_bytes).unwrap();

    let genesis_doc = json!({
        "type": "plc_operation",
        "rotationKeys": [&rotation_did_key],
        "verificationMethods": {
            "atproto": &signing_did_key,
        },
        "alsoKnownAs": [],
        "services": {},
        "prev": null,
        "cid": "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    });

    plc_store.write().await.insert(did.clone(), genesis_doc);

    // Create a service entry
    app.create_service_entry("#chess", "ChessAppView", "all")
        .await;

    // POST sync-plc
    let cookie = app.admin_cookie();
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/service-entries/sync-plc")
                .header(cookie.0, cookie.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "sync-plc should return 204"
    );

    // Verify the PLC mock received the updated document
    let store = plc_store.read().await;
    let updated = store.get(&did).expect("PLC store should have the DID");
    let services = updated["services"]
        .as_object()
        .expect("services should exist");
    assert!(
        services.contains_key("chess"),
        "services should contain the chess entry"
    );
    assert_eq!(updated["services"]["chess"]["type"], "ChessAppView");
}

#[tokio::test]
#[serial]
async fn sync_plc_rejects_non_plc_mode() {
    common::require_db!();
    let mut app = TestApp::new().await;
    app.setup_did_web().await;

    let cookie = app.admin_cookie();
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/service-entries/sync-plc")
                .header(cookie.0, cookie.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "sync-plc should reject non-plc mode"
    );
}
