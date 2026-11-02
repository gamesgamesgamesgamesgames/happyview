//! HappyView's own key for signing space delegation tokens.
//!
//! A PDS that serves spaces mints delegation tokens with the account's key;
//! for accounts on one that does not, HappyView mints them with this key and
//! verifies them against it. It is a random secp256k1 key, generated on first
//! use and stored sealed with `TOKEN_ENCRYPTION_KEY` in
//! `happyview_delegation_key`, so nothing is derived from the encryption key
//! and the encryption key alone cannot mint a token. Its public key is
//! published nowhere: only this instance verifies what it signs.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use k256::ecdsa::SigningKey;
use rand::Rng;
use sqlx::AnyPool;

use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::plugin::encryption::{decrypt, encrypt};

/// The delegation key, generating and storing one if the instance has none.
///
/// Replicas sharing a database converge on one key: the insert does nothing
/// when another got there first, and the key is read back after it.
pub async fn load_or_create(
    pool: &AnyPool,
    backend: DatabaseBackend,
    encryption_key: &[u8; 32],
) -> Result<SigningKey, AppError> {
    if let Some(key) = load(pool, backend, encryption_key).await? {
        return Ok(key);
    }

    let sealed = seal(encryption_key, &generate()?)?;
    let sql = adapt_sql(
        "INSERT INTO happyview_delegation_key (id, private_key, created_at) \
         VALUES (1, ?, ?) ON CONFLICT (id) DO NOTHING",
        backend,
    );
    crate::db::query(&sql)
        .bind(sealed)
        .bind(now_rfc3339())
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to store delegation key: {e}")))?;

    load(pool, backend, encryption_key)
        .await?
        .ok_or_else(|| AppError::Internal("delegation key missing after insert".into()))
}

/// The delegation key, or `None` when the instance has never minted a token.
pub async fn load(
    pool: &AnyPool,
    backend: DatabaseBackend,
    encryption_key: &[u8; 32],
) -> Result<Option<SigningKey>, AppError> {
    let sql = adapt_sql(
        "SELECT private_key FROM happyview_delegation_key WHERE id = 1",
        backend,
    );
    let row: Option<(String,)> = crate::db::query_as(&sql)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to read delegation key: {e}")))?;
    row.map(|(sealed,)| open(encryption_key, &sealed))
        .transpose()
}

fn generate() -> Result<SigningKey, AppError> {
    // A random 32 bytes is a valid scalar unless it is zero or at least the
    // curve order, which is vanishingly rare; draw again rather than fail.
    for _ in 0..8 {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        if let Ok(key) = SigningKey::from_slice(&bytes) {
            return Ok(key);
        }
    }
    Err(AppError::Internal(
        "failed to generate a delegation key".into(),
    ))
}

fn seal(encryption_key: &[u8; 32], key: &SigningKey) -> Result<String, AppError> {
    let sealed = encrypt(encryption_key, key.to_bytes().as_slice())
        .map_err(|e| AppError::Internal(format!("failed to seal delegation key: {e}")))?;
    Ok(STANDARD.encode(sealed))
}

fn open(encryption_key: &[u8; 32], sealed: &str) -> Result<SigningKey, AppError> {
    let bytes = STANDARD
        .decode(sealed)
        .map_err(|e| AppError::Internal(format!("failed to decode delegation key: {e}")))?;
    let plain = decrypt(encryption_key, &bytes)
        .map_err(|e| AppError::Internal(format!("failed to open delegation key: {e}")))?;
    SigningKey::from_slice(&plain)
        .map_err(|e| AppError::Internal(format!("invalid delegation key: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_pools;

    const KEY: [u8; 32] = [0x42; 32];

    #[tokio::test]
    async fn generated_once_and_reused() {
        for (pool, backend) in test_pools().await {
            crate::db::query("DELETE FROM happyview_delegation_key")
                .execute(&pool)
                .await
                .unwrap();
            assert!(load(&pool, backend, &KEY).await.unwrap().is_none());

            let first = load_or_create(&pool, backend, &KEY).await.unwrap();
            let again = load_or_create(&pool, backend, &KEY).await.unwrap();
            assert_eq!(first.to_bytes(), again.to_bytes());
            assert_eq!(
                load(&pool, backend, &KEY)
                    .await
                    .unwrap()
                    .unwrap()
                    .to_bytes(),
                first.to_bytes()
            );
        }
    }

    #[tokio::test]
    async fn not_the_encryption_key_and_never_stored_unsealed() {
        for (pool, backend) in test_pools().await {
            crate::db::query("DELETE FROM happyview_delegation_key")
                .execute(&pool)
                .await
                .unwrap();
            let key = load_or_create(&pool, backend, &KEY).await.unwrap();
            assert_ne!(key.to_bytes().as_slice(), KEY.as_slice());

            let (stored,): (String,) =
                crate::db::query_as("SELECT private_key FROM happyview_delegation_key")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            let raw = STANDARD.decode(&stored).unwrap();
            assert!(
                !raw.windows(32).any(|w| w == key.to_bytes().as_slice()),
                "the stored row must not hold the key in the clear"
            );
            assert!(
                open(&[0x43; 32], &stored).is_err(),
                "another key cannot open it"
            );
        }
    }
}
