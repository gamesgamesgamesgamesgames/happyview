mod common;

use atrium_common::store::Store;
use happyview::oauth::client_keys::{
    ClientKey, INSTANCE_OWNER, KeyStatus, ensure_instance_key, generate_client_key, insert_key,
    list_keys_for_owner, load_keys, revoke_key, session_counts_by_kid,
};
use serial_test::serial;

use common::db as test_db;

#[tokio::test]
#[serial]
async fn ensure_instance_key_is_idempotent() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let first = ensure_instance_key(&pool, backend, None).await.unwrap();
    let second = ensure_instance_key(&pool, backend, None).await.unwrap();

    assert_eq!(first.kid, second.kid);
    assert_eq!(first.private_jwk, second.private_jwk);
    assert_eq!(second.status, KeyStatus::Current);
}

#[tokio::test]
#[serial]
async fn keys_round_trip_encrypted() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;
    let enc = [3u8; 32];

    let key = generate_client_key(INSTANCE_OWNER).unwrap();
    insert_key(&pool, backend, Some(&enc), &key).await.unwrap();

    let loaded = load_keys(&pool, backend, Some(&enc), INSTANCE_OWNER)
        .await
        .unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].kid, key.kid);
    assert_eq!(loaded[0].private_jwk, key.private_jwk);
}

#[tokio::test]
#[serial]
async fn keys_round_trip_unencrypted() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let key = generate_client_key(INSTANCE_OWNER).unwrap();
    insert_key(&pool, backend, None, &key).await.unwrap();

    let loaded = load_keys(&pool, backend, None, INSTANCE_OWNER)
        .await
        .unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].kid, key.kid);
    assert_eq!(loaded[0].private_jwk, key.private_jwk);
}

#[tokio::test]
#[serial]
async fn second_current_key_for_owner_is_rejected_by_the_database() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let first = generate_client_key(INSTANCE_OWNER).unwrap();
    insert_key(&pool, backend, None, &first).await.unwrap();

    let second = generate_client_key(INSTANCE_OWNER).unwrap();
    let result = insert_key(&pool, backend, None, &second).await;

    let err = result.expect_err(
        "a second `current` key for the same owner must be rejected by the unique index",
    );
    let message = err.to_string().to_lowercase();
    assert!(
        message.contains("unique") || message.contains("duplicate"),
        "expected a unique-constraint violation, got: {message}"
    );
}

#[tokio::test]
#[serial]
async fn load_keys_excludes_revoked_and_puts_current_first() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let mut retiring: ClientKey = generate_client_key("client-a").unwrap();
    retiring.status = KeyStatus::Retiring;
    let mut revoked: ClientKey = generate_client_key("client-a").unwrap();
    revoked.status = KeyStatus::Revoked;
    let current = generate_client_key("client-a").unwrap();

    insert_key(&pool, backend, None, &retiring).await.unwrap();
    insert_key(&pool, backend, None, &revoked).await.unwrap();
    insert_key(&pool, backend, None, &current).await.unwrap();

    let loaded = load_keys(&pool, backend, None, "client-a").await.unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].kid, current.kid);
    assert_eq!(loaded[1].kid, retiring.kid);
}

#[tokio::test]
#[serial]
async fn owners_are_isolated() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let a = generate_client_key("client-a").unwrap();
    let b = generate_client_key("client-b").unwrap();
    insert_key(&pool, backend, None, &a).await.unwrap();
    insert_key(&pool, backend, None, &b).await.unwrap();

    let loaded = load_keys(&pool, backend, None, "client-a").await.unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].kid, a.kid);
}

#[tokio::test]
#[serial]
async fn a_stored_dpop_session_remembers_its_signing_kid() {
    common::require_db!();
    let app = common::app::TestApp::new().await;
    let pool = &app.state.db;
    let backend = app.state.db_backend;
    let enc = [9u8; 32];

    let (_client_key, _client_secret, api_client_id) =
        app.create_api_client("confidential", None).await;

    let key = generate_client_key(&api_client_id).unwrap();
    insert_key(pool, backend, Some(&enc), &key).await.unwrap();

    let dpop_keypair = happyview::oauth::keys::generate_dpop_keypair().unwrap();
    happyview::oauth::keys::store_dpop_key(
        pool,
        backend,
        &enc,
        "dpop-key-1",
        "provision-1",
        &api_client_id,
        &dpop_keypair,
        None,
    )
    .await
    .unwrap();

    happyview::oauth::sessions::store_dpop_session(
        pool,
        backend,
        &enc,
        "session-1",
        &api_client_id,
        "dpop-key-1",
        "did:plc:example",
        "access",
        Some("refresh"),
        None,
        "atproto",
        Some("https://pds.example.com"),
        Some("https://pds.example.com"),
        Some(&key.kid),
    )
    .await
    .unwrap();

    let loaded = happyview::oauth::sessions::get_dpop_session(
        pool,
        backend,
        &enc,
        &api_client_id,
        "did:plc:example",
        "dpop-key-1",
    )
    .await
    .unwrap();

    assert_eq!(loaded.signing_kid.as_deref(), Some(key.kid.as_str()));
}

#[tokio::test]
#[serial]
async fn refreshing_a_session_does_not_change_its_signing_kid() {
    common::require_db!();
    // Same FK requirement as the round-trip test above — see its comment.
    let app = common::app::TestApp::new().await;
    let pool = &app.state.db;
    let backend = app.state.db_backend;
    let enc = [9u8; 32];

    let (_client_key, _client_secret, api_client_id) =
        app.create_api_client("confidential", None).await;

    let dpop_keypair = happyview::oauth::keys::generate_dpop_keypair().unwrap();
    happyview::oauth::keys::store_dpop_key(
        pool,
        backend,
        &enc,
        "dpop-key-1",
        "provision-2",
        &api_client_id,
        &dpop_keypair,
        None,
    )
    .await
    .unwrap();

    happyview::oauth::sessions::store_dpop_session(
        pool,
        backend,
        &enc,
        "session-1",
        &api_client_id,
        "dpop-key-1",
        "did:plc:example",
        "access-v1",
        Some("refresh-v1"),
        None,
        "atproto",
        Some("https://pds.example.com"),
        Some("https://pds.example.com"),
        Some("kid-that-established-the-session"),
    )
    .await
    .unwrap();

    happyview::oauth::sessions::store_dpop_session(
        pool,
        backend,
        &enc,
        "session-1",
        &api_client_id,
        "dpop-key-1",
        "did:plc:example",
        "access-v2",
        Some("refresh-v2"),
        None,
        "atproto",
        Some("https://pds.example.com"),
        Some("https://pds.example.com"),
        Some("a-different-current-kid"),
    )
    .await
    .unwrap();

    let loaded = happyview::oauth::sessions::get_dpop_session(
        pool,
        backend,
        &enc,
        &api_client_id,
        "did:plc:example",
        "dpop-key-1",
    )
    .await
    .unwrap();

    assert_eq!(
        loaded.access_token, "access-v2",
        "tokens must still refresh"
    );
    assert_eq!(
        loaded.signing_kid.as_deref(),
        Some("kid-that-established-the-session"),
        "a refresh must never change the key a session is pinned to"
    );
}

#[tokio::test]
#[serial]
async fn re_registering_a_dpop_session_after_rotation_moves_its_signing_kid() {
    common::require_db!();
    let app = common::app::TestApp::new().await;
    let pool = &app.state.db;
    let backend = app.state.db_backend;
    let enc = [7u8; 32];

    let (_client_key, _client_secret, api_client_id) =
        app.create_api_client("confidential", None).await;

    let key_a = generate_client_key(&api_client_id).unwrap();
    insert_key(pool, backend, Some(&enc), &key_a).await.unwrap();

    let dpop_keypair = happyview::oauth::keys::generate_dpop_keypair().unwrap();
    happyview::oauth::keys::store_dpop_key(
        pool,
        backend,
        &enc,
        "dpop-key-1",
        "provision-3",
        &api_client_id,
        &dpop_keypair,
        None,
    )
    .await
    .unwrap();

    // Original registration, before any rotation: pinned to A.
    happyview::oauth::sessions::store_dpop_session(
        pool,
        backend,
        &enc,
        "session-1",
        &api_client_id,
        "dpop-key-1",
        "did:plc:example",
        "access-v1",
        Some("refresh-v1"),
        None,
        "atproto",
        Some("https://pds.example.com"),
        Some("https://pds.example.com"),
        Some(&key_a.kid),
    )
    .await
    .unwrap();

    let key_b =
        happyview::oauth::client_keys::rotate_key(pool, backend, Some(&enc), &api_client_id)
            .await
            .unwrap();
    assert_ne!(key_a.kid, key_b.kid, "rotation must mint a distinct key");

    happyview::oauth::sessions::store_dpop_session(
        pool,
        backend,
        &enc,
        "session-2",
        &api_client_id,
        "dpop-key-1",
        "did:plc:example",
        "access-v2",
        Some("refresh-v2"),
        None,
        "atproto",
        Some("https://pds.example.com"),
        Some("https://pds.example.com"),
        Some(&key_b.kid),
    )
    .await
    .unwrap();

    let stale = happyview::oauth::sessions::get_dpop_session(
        pool,
        backend,
        &enc,
        &api_client_id,
        "did:plc:example",
        "dpop-key-1",
    )
    .await
    .unwrap();
    assert_eq!(
        stale.signing_kid.as_deref(),
        Some(key_a.kid.as_str()),
        "store_dpop_session's ON CONFLICT must not move the pin by itself — reproduces the bug this task fixes"
    );

    happyview::oauth::sessions::repin_dpop_session_signing_kid(
        pool,
        backend,
        &api_client_id,
        "dpop-key-1",
        "did:plc:example",
        Some(&key_b.kid),
    )
    .await
    .unwrap();

    let fixed = happyview::oauth::sessions::get_dpop_session(
        pool,
        backend,
        &enc,
        &api_client_id,
        "did:plc:example",
        "dpop-key-1",
    )
    .await
    .unwrap();
    assert_eq!(
        fixed.signing_kid.as_deref(),
        Some(key_b.kid.as_str()),
        "a re-registration after rotation must move the pin to the key that performed it"
    );
}

#[tokio::test]
#[serial]
async fn rotation_retires_the_old_key_and_keeps_it_loadable() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let first = happyview::oauth::client_keys::ensure_instance_key(&pool, backend, None)
        .await
        .unwrap();
    let second = happyview::oauth::client_keys::rotate_key(
        &pool,
        backend,
        None,
        happyview::oauth::client_keys::INSTANCE_OWNER,
    )
    .await
    .unwrap();

    assert_ne!(first.kid, second.kid);

    let keys = happyview::oauth::client_keys::load_keys(
        &pool,
        backend,
        None,
        happyview::oauth::client_keys::INSTANCE_OWNER,
    )
    .await
    .unwrap();

    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0].kid, second.kid);
    assert_eq!(
        keys[0].status,
        happyview::oauth::client_keys::KeyStatus::Current
    );
    assert_eq!(keys[1].kid, first.kid);
    assert_eq!(
        keys[1].status,
        happyview::oauth::client_keys::KeyStatus::Retiring
    );
}

#[tokio::test]
#[serial]
async fn retirement_sweep_leaves_a_key_with_live_sessions_alone() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let first = happyview::oauth::client_keys::ensure_instance_key(&pool, backend, None)
        .await
        .unwrap();
    common::insert_oauth_session(&pool, backend, "did:plc:example", Some(&first.kid)).await;
    happyview::oauth::client_keys::rotate_key(
        &pool,
        backend,
        None,
        happyview::oauth::client_keys::INSTANCE_OWNER,
    )
    .await
    .unwrap();

    let revoked = happyview::oauth::client_keys::retire_unused_keys(&pool, backend)
        .await
        .unwrap();
    assert_eq!(revoked, 0);
}

#[tokio::test]
#[serial]
async fn retirement_sweep_revokes_a_key_no_session_references() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    happyview::oauth::client_keys::ensure_instance_key(&pool, backend, None)
        .await
        .unwrap();
    happyview::oauth::client_keys::rotate_key(
        &pool,
        backend,
        None,
        happyview::oauth::client_keys::INSTANCE_OWNER,
    )
    .await
    .unwrap();

    let revoked = happyview::oauth::client_keys::retire_unused_keys(&pool, backend)
        .await
        .unwrap();
    assert_eq!(revoked, 1);

    let keys = happyview::oauth::client_keys::load_keys(
        &pool,
        backend,
        None,
        happyview::oauth::client_keys::INSTANCE_OWNER,
    )
    .await
    .unwrap();
    assert_eq!(keys.len(), 1);
}

#[tokio::test]
#[serial]
async fn rotate_key_reports_sessions_it_cannot_protect() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    happyview::oauth::client_keys::ensure_instance_key(&pool, backend, None)
        .await
        .unwrap();

    common::insert_oauth_session(&pool, backend, "did:plc:pinned", Some("some-kid")).await;
    common::insert_oauth_session(&pool, backend, "did:plc:unpinned", None).await;

    let orphaned = happyview::oauth::client_keys::count_unstamped_sessions(
        &pool,
        backend,
        happyview::oauth::client_keys::INSTANCE_OWNER,
    )
    .await
    .unwrap();
    assert_eq!(orphaned, 1);
}

#[tokio::test]
#[serial]
async fn count_unstamped_sessions_is_zero_once_every_row_is_pinned() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    happyview::oauth::client_keys::ensure_instance_key(&pool, backend, None)
        .await
        .unwrap();
    common::insert_oauth_session(&pool, backend, "did:plc:pinned", Some("some-kid")).await;

    let orphaned = happyview::oauth::client_keys::count_unstamped_sessions(
        &pool,
        backend,
        happyview::oauth::client_keys::INSTANCE_OWNER,
    )
    .await
    .unwrap();
    assert_eq!(orphaned, 0);
}

#[tokio::test]
#[serial]
async fn rotation_moves_the_default_linked_repos_client_to_the_new_key() {
    common::require_db!();
    let app = common::app::TestApp::new().await;
    let pool = &app.state.db;
    let backend = app.state.db_backend;

    let key_a = happyview::oauth::client_keys::ensure_instance_key(pool, backend, None)
        .await
        .unwrap();

    assert!(
        app.state.oauth.linked_repos_primary().is_none(),
        "before any rotation there is no live entry — the boot-time value in AppState is still correct"
    );

    let (key_b, _orphaned) = happyview::oauth::rotation::rotate_instance_key(&app.state)
        .await
        .unwrap();
    assert_ne!(key_a.kid, key_b.kid, "rotation must mint a distinct key");

    let (_client, kid) = app
        .state
        .oauth
        .linked_repos_primary()
        .expect("rotation must install a live default linked-repos client");
    assert_eq!(
        kid.as_deref(),
        Some(key_b.kid.as_str()),
        "the default linked-repos client must move to the new current key. If it keeps the \
         old one, revoking that key — which is exactly what the dashboard tells an operator \
         to do after rotating — leaves it signing with a kid absent from the published JWKS, \
         failing every new linked-repo authorization with an opaque invalid_client until the \
         process is restarted"
    );
}

#[tokio::test]
#[serial]
async fn rotation_survives_for_a_session_pinned_to_the_old_key() {
    common::require_db!();
    let app = common::app::TestApp::new().await;
    let pool = &app.state.db;
    let backend = app.state.db_backend;
    let key_a = happyview::oauth::client_keys::ensure_instance_key(pool, backend, None)
        .await
        .unwrap();

    let instance_client_id_url = app.state.config.instance_client_id_url();
    let is_loopback =
        happyview::auth::client_registry::is_loopback_url(&app.state.config.public_url);
    let callback_url = format!(
        "{}/auth/callback",
        app.state
            .config
            .effective_public_url()
            .trim_end_matches('/')
    );
    let scopes = vec![
        atrium_oauth::Scope::Known(atrium_oauth::KnownScope::Atproto),
        atrium_oauth::Scope::Unknown("identity:*".to_string()),
    ];

    let client_a = happyview::auth::client_registry::build_instance_client(
        &app.state.config.plc_url,
        &instance_client_id_url,
        &app.state.config.effective_public_url(),
        vec![callback_url],
        is_loopback,
        scopes,
        app.state.oauth_state_store.clone(),
        pool.clone(),
        backend,
        &key_a,
    )
    .unwrap();
    app.state.oauth.register_for_kid(
        &instance_client_id_url,
        &key_a.kid,
        std::sync::Arc::new(client_a),
    );

    // A session established under A, before any rotation.
    common::insert_oauth_session(pool, backend, "did:plc:pre-rotation", Some(&key_a.kid)).await;

    let (key_b, _orphaned) = happyview::oauth::rotation::rotate_instance_key(&app.state)
        .await
        .unwrap();
    assert_ne!(key_a.kid, key_b.kid, "rotation must mint a distinct key");

    // A session established after rotation, pinned to the new current key.
    common::insert_oauth_session(pool, backend, "did:plc:post-rotation", Some(&key_b.kid)).await;

    let keys = happyview::oauth::client_keys::load_keys(
        pool,
        backend,
        None,
        happyview::oauth::client_keys::INSTANCE_OWNER,
    )
    .await
    .unwrap();

    let resolved_pre = happyview::oauth::client_keys::resolve_signing_key(&keys, Some(&key_a.kid))
        .unwrap()
        .expect("a session pinned to A must still resolve to a key after rotation");
    assert_eq!(
        resolved_pre.kid, key_a.kid,
        "a session established under the old key must keep resolving to it, not to the new current key"
    );

    let resolved_post = happyview::oauth::client_keys::resolve_signing_key(&keys, Some(&key_b.kid))
        .unwrap()
        .expect("a session pinned to B must resolve to a key");
    assert_eq!(
        resolved_post.kid, key_b.kid,
        "a session established after rotation must resolve to the new current key"
    );
    assert!(
        app.state
            .oauth
            .get_for_kid(&instance_client_id_url, &key_a.kid)
            .is_some(),
        "the registry must keep serving the retired key's client, not just the new one"
    );
    assert!(
        app.state
            .oauth
            .get_for_kid(&instance_client_id_url, &key_b.kid)
            .is_some(),
        "rotation must register the new key's client immediately, without a restart"
    );
}

#[tokio::test]
#[serial]
async fn a_second_login_after_rotation_moves_the_pin_to_the_new_key() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let key_a = ensure_instance_key(&pool, backend, None).await.unwrap();

    // Original login, before any rotation: row pinned to A.
    let did = "did:plc:secondloginafterrotation";
    common::insert_oauth_session(&pool, backend, did, Some(&key_a.kid)).await;

    let key_b = happyview::oauth::client_keys::rotate_key(&pool, backend, None, INSTANCE_OWNER)
        .await
        .unwrap();
    assert_ne!(key_a.kid, key_b.kid, "rotation must mint a distinct key");

    let store_for_b = happyview::auth::oauth_store::DbSessionStore::new(pool.clone(), backend)
        .with_signing_kid(Some(key_b.kid.clone()));
    let session_did = atrium_api::types::string::Did::new(did.to_string()).unwrap();
    store_for_b
        .set(session_did.clone(), sample_session())
        .await
        .unwrap();

    let stale_kid: (Option<String>,) = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT signing_kid FROM happyview_oauth_sessions WHERE did = ?",
        backend,
    ))
    .bind(did)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        stale_kid.0,
        Some(key_a.kid.clone()),
        "atrium's own set() must not move the pin by itself — reproduces the bug this task fixes"
    );

    happyview::auth::oauth_store::repin_signing_kid(
        &pool,
        backend,
        "happyview_oauth_sessions",
        "did",
        did,
        Some(&key_b.kid),
    )
    .await
    .unwrap();

    let fixed_kid: (Option<String>,) = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT signing_kid FROM happyview_oauth_sessions WHERE did = ?",
        backend,
    ))
    .bind(did)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        fixed_kid.0,
        Some(key_b.kid),
        "a second login after rotation must move the pin to the key that performed it"
    );
}

fn sample_session() -> atrium_oauth::store::session::Session {
    let key = generate_client_key("test-owner").unwrap();
    let dpop_key = happyview::oauth::client_keys::to_atrium_jwk(&key)
        .unwrap()
        .key;
    let token_set: atrium_oauth::TokenSet = serde_json::from_value(serde_json::json!({
        "iss": "https://issuer.example",
        "sub": "did:plc:secondloginafterrotation",
        "aud": "https://aud.example",
        "scope": null,
        "refresh_token": null,
        "access_token": "tok-b",
        "token_type": "DPoP",
        "expires_at": null,
    }))
    .unwrap();
    atrium_oauth::store::session::Session {
        dpop_key,
        token_set,
    }
}

// ---------------------------------------------------------------------------
// Single-key revoke (Task 22a)
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn revoke_key_marks_a_retiring_key_revoked_and_hides_it_from_load_keys() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let current = generate_client_key(INSTANCE_OWNER).unwrap();
    insert_key(&pool, backend, None, &current).await.unwrap();

    let mut retiring = generate_client_key(INSTANCE_OWNER).unwrap();
    retiring.status = KeyStatus::Retiring;
    insert_key(&pool, backend, None, &retiring).await.unwrap();

    let affected = revoke_key(&pool, backend, INSTANCE_OWNER, &retiring.kid)
        .await
        .unwrap();
    assert!(
        affected,
        "revoking an existing retiring key must affect a row"
    );

    let loaded = load_keys(&pool, backend, None, INSTANCE_OWNER)
        .await
        .unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].kid, current.kid);

    let listed = list_keys_for_owner(&pool, backend, INSTANCE_OWNER)
        .await
        .unwrap();
    let retired_row = listed
        .iter()
        .find(|k| k.kid == retiring.kid)
        .expect("revoked key must still appear in list_keys_for_owner");
    assert_eq!(retired_row.status, KeyStatus::Revoked);
}

#[tokio::test]
#[serial]
async fn revoke_key_does_not_touch_a_different_owners_key() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let other = generate_client_key("some-other-client").unwrap();
    insert_key(&pool, backend, None, &other).await.unwrap();

    let affected = revoke_key(&pool, backend, INSTANCE_OWNER, &other.kid)
        .await
        .unwrap();
    assert!(
        !affected,
        "revoking under the wrong owner must affect no rows"
    );

    let listed = list_keys_for_owner(&pool, backend, "some-other-client")
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0].status,
        KeyStatus::Current,
        "the other owner's key must be untouched"
    );
}

#[tokio::test]
#[serial]
async fn revoke_key_returns_false_for_an_unknown_kid() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let current = generate_client_key(INSTANCE_OWNER).unwrap();
    insert_key(&pool, backend, None, &current).await.unwrap();

    let affected = revoke_key(&pool, backend, INSTANCE_OWNER, "not-a-real-kid")
        .await
        .unwrap();
    assert!(!affected);
}

#[tokio::test]
#[serial]
async fn list_keys_for_owner_includes_revoked_ordered_current_then_retiring_then_revoked() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let current = generate_client_key(INSTANCE_OWNER).unwrap();
    insert_key(&pool, backend, None, &current).await.unwrap();

    let mut retiring = generate_client_key(INSTANCE_OWNER).unwrap();
    retiring.status = KeyStatus::Retiring;
    insert_key(&pool, backend, None, &retiring).await.unwrap();

    let mut revoked = generate_client_key(INSTANCE_OWNER).unwrap();
    revoked.status = KeyStatus::Revoked;
    insert_key(&pool, backend, None, &revoked).await.unwrap();

    let listed = list_keys_for_owner(&pool, backend, INSTANCE_OWNER)
        .await
        .unwrap();
    assert_eq!(listed.len(), 3);
    assert_eq!(listed[0].kid, current.kid);
    assert_eq!(listed[0].status, KeyStatus::Current);
    assert_eq!(listed[1].kid, retiring.kid);
    assert_eq!(listed[1].status, KeyStatus::Retiring);
    assert_eq!(listed[2].kid, revoked.kid);
    assert_eq!(listed[2].status, KeyStatus::Revoked);
}

async fn seed_api_client_for_dpop_fixture(
    pool: &sqlx::AnyPool,
    backend: happyview::db::DatabaseBackend,
    id: &str,
) {
    let now = happyview::db::now_rfc3339();
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_api_clients (id, client_key, name, client_id_url, client_uri, redirect_uris, scopes, is_active, created_by, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?)",
        backend,
    );
    happyview::db::query(&sql)
        .bind(id)
        .bind(format!("hvc_{id}"))
        .bind("dpop fixture client")
        .bind(format!(
            "https://example.test/{id}/oauth-client-metadata.json"
        ))
        .bind("https://example.test")
        .bind("[]")
        .bind("atproto")
        .bind("did:plc:testadmin")
        .bind(&now)
        .bind(&now)
        .execute(pool)
        .await
        .expect("failed to insert dpop fixture api client");
}

async fn seed_dpop_key_for_fixture(
    pool: &sqlx::AnyPool,
    backend: happyview::db::DatabaseBackend,
    id: &str,
    api_client_id: &str,
) {
    let now = happyview::db::now_rfc3339();
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_dpop_keys (id, provision_id, api_client_id, private_key_enc, jwk_thumbprint, created_at) VALUES (?, ?, ?, ?, ?, ?)",
        backend,
    );
    happyview::db::query(&sql)
        .bind(id)
        .bind(format!("prov_{id}"))
        .bind(api_client_id)
        .bind(vec![0u8; 4])
        .bind("thumbprint")
        .bind(&now)
        .execute(pool)
        .await
        .expect("failed to insert dpop fixture key");
}

async fn seed_dpop_session(
    pool: &sqlx::AnyPool,
    backend: happyview::db::DatabaseBackend,
    id: &str,
    api_client_id: &str,
    dpop_key_id: &str,
    user_did: &str,
    signing_kid: &str,
) {
    let now = happyview::db::now_rfc3339();
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_dpop_sessions (id, api_client_id, dpop_key_id, user_did, access_token_enc, scopes, created_at, updated_at, signing_kid) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        backend,
    );
    happyview::db::query(&sql)
        .bind(id)
        .bind(api_client_id)
        .bind(dpop_key_id)
        .bind(user_did)
        .bind(vec![0u8; 4])
        .bind("atproto")
        .bind(&now)
        .bind(&now)
        .bind(signing_kid)
        .execute(pool)
        .await
        .expect("failed to insert dpop fixture session");
}

#[tokio::test]
#[serial]
async fn session_counts_by_kid_counts_across_all_three_session_tables_including_zero() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let kid_a = "kid-a-session-count-fixture";
    let kid_b = "kid-b-session-count-fixture";
    let kid_c = "kid-c-session-count-fixture"; // no sessions anywhere

    // Two `happyview_oauth_sessions` rows pinned to kid A.
    common::insert_oauth_session(&pool, backend, "did:plc:sc-a1", Some(kid_a)).await;
    common::insert_oauth_session(&pool, backend, "did:plc:sc-a2", Some(kid_a)).await;

    // One `happyview_linked_repo_sessions` row pinned to kid B.
    let now = happyview::db::now_rfc3339();
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_linked_repo_sessions (did, session_data, updated_at, signing_kid) VALUES (?, ?, ?, ?)",
        backend,
    );
    happyview::db::query(&sql)
        .bind("did:plc:sc-b1")
        .bind("{}")
        .bind(&now)
        .bind(kid_b)
        .execute(&pool)
        .await
        .expect("failed to insert linked-repo session fixture");

    // One `happyview_dpop_sessions` row also pinned to kid B, so kid B's
    // total spans two of the three tables.
    seed_api_client_for_dpop_fixture(&pool, backend, "sc-client").await;
    seed_dpop_key_for_fixture(&pool, backend, "sc-dpop-key", "sc-client").await;
    seed_dpop_session(
        &pool,
        backend,
        "sc-dpop-session",
        "sc-client",
        "sc-dpop-key",
        "did:plc:sc-b2",
        kid_b,
    )
    .await;

    let counts = session_counts_by_kid(&pool, backend).await.unwrap();

    assert_eq!(counts.get(kid_a).copied(), Some(2));
    assert_eq!(counts.get(kid_b).copied(), Some(2));
    assert_eq!(
        counts.get(kid_c).copied().unwrap_or(0),
        0,
        "a kid with no sessions must count as zero, not be missing in a way that panics a caller"
    );
}
