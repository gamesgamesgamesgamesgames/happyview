mod common;

use happyview::oauth::client_keys::{
    ClientKey, INSTANCE_OWNER, KeyStatus, ensure_instance_key, generate_client_key, insert_key,
    load_keys,
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
