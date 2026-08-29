//! ES256 client-authentication keys for confidential OAuth clients.
//!
//! Distinct from `oauth::keys`, which provisions per-session DPoP keypairs.
//! A DPoP key binds one token to one session; a client-authentication key
//! identifies HappyView (or an API client) to every authorization server it
//! talks to, and is what buys 2-year sessions instead of 2-week ones.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::ecdsa::SigningKey;
use rand::Rng;

use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::plugin::encryption::{decrypt, encrypt};

/// `owner` value for the instance's own key, as opposed to an API client's.
pub const INSTANCE_OWNER: &str = "instance";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStatus {
    /// Signs all new sessions.
    Current,
    /// Still signs sessions that were established with it; signs no new ones.
    Retiring,
    /// Removed from the JWKS. Any session pinned to it is dead.
    Revoked,
}

impl KeyStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            KeyStatus::Current => "current",
            KeyStatus::Retiring => "retiring",
            KeyStatus::Revoked => "revoked",
        }
    }

    pub fn parse(s: &str) -> Option<KeyStatus> {
        match s {
            "current" => Some(KeyStatus::Current),
            "retiring" => Some(KeyStatus::Retiring),
            "revoked" => Some(KeyStatus::Revoked),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClientKey {
    pub kid: String,
    pub owner: String,
    pub alg: String,
    pub private_jwk: serde_json::Value,
    pub public_jwk: serde_json::Value,
    pub status: KeyStatus,
}

/// Generate a fresh ES256 (P-256) client-authentication key.
///
/// The `kid` is a UUID rather than a thumbprint: it must stay stable and
/// unique across rotations, and Stage 3 uses it as the join key from a session
/// to the key that established it.
pub fn generate_client_key(owner: &str) -> Result<ClientKey, AppError> {
    let mut d_bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut d_bytes);

    let signing_key = SigningKey::from_slice(&d_bytes[..])
        .map_err(|e| AppError::Internal(format!("failed to generate client key: {e}")))?;
    // Bind the verifying key before taking the point: inlining the call leaves
    // `point` borrowing a temporary, which does not compile. `oauth::keys`
    // splits it the same way for the same reason.
    let verifying_key = signing_key.verifying_key();
    let point = verifying_key.to_sec1_point(false);
    let x = point
        .x()
        .ok_or_else(|| AppError::Internal("missing x coordinate".into()))?;
    let y = point
        .y()
        .ok_or_else(|| AppError::Internal("missing y coordinate".into()))?;

    let kid = uuid::Uuid::new_v4().to_string();
    let x_b64 = URL_SAFE_NO_PAD.encode(x);
    let y_b64 = URL_SAFE_NO_PAD.encode(y);

    let public_jwk = serde_json::json!({
        "kty": "EC",
        "crv": "P-256",
        "x": x_b64,
        "y": y_b64,
        "kid": kid,
        "alg": "ES256",
        "use": "sig",
    });

    let mut private_jwk = public_jwk.clone();
    private_jwk["d"] = serde_json::Value::String(URL_SAFE_NO_PAD.encode(d_bytes));

    Ok(ClientKey {
        kid,
        owner: owner.to_string(),
        alg: "ES256".to_string(),
        private_jwk,
        public_jwk,
        status: KeyStatus::Current,
    })
}

/// Serialise a private JWK for storage, encrypting it when a key is available.
///
/// Returns the bytes and whether they are encrypted. `TOKEN_ENCRYPTION_KEY` is
/// optional and dashboard login works without it, so requiring it here would
/// turn a free upgrade into an operator action. Plaintext is no worse than
/// `happyview_oauth_sessions.session_data`, which already stores refresh tokens
/// and DPoP private keys in the clear.
pub fn seal_private_jwk(
    enc: Option<&[u8; 32]>,
    jwk: &serde_json::Value,
) -> Result<(Vec<u8>, bool), AppError> {
    let bytes = serde_json::to_vec(jwk)
        .map_err(|e| AppError::Internal(format!("failed to serialise client key: {e}")))?;
    match enc {
        Some(key) => {
            let sealed = encrypt(key, &bytes)
                .map_err(|e| AppError::Internal(format!("failed to encrypt client key: {e}")))?;
            Ok((sealed, true))
        }
        None => Ok((bytes, false)),
    }
}

/// Inverse of [`seal_private_jwk`].
pub fn unseal_private_jwk(
    enc: Option<&[u8; 32]>,
    blob: &[u8],
    encrypted: bool,
) -> Result<serde_json::Value, AppError> {
    let bytes = if encrypted {
        let key = enc.ok_or_else(|| {
            AppError::Internal("client key is encrypted but TOKEN_ENCRYPTION_KEY is not set".into())
        })?;
        decrypt(key, blob)
            .map_err(|e| AppError::Internal(format!("failed to decrypt client key: {e}")))?
    } else {
        blob.to_vec()
    };
    serde_json::from_slice(&bytes)
        .map_err(|e| AppError::Internal(format!("failed to parse client key: {e}")))
}

/// Convert to the JWK type atrium's `Keyset` consumes.
pub fn to_atrium_jwk(key: &ClientKey) -> Result<jose_jwk::Jwk, AppError> {
    serde_json::from_value(key.private_jwk.clone())
        .map_err(|e| AppError::Internal(format!("client key is not a valid JWK: {e}")))
}

/// Render a public JWKS document. Private material is never included, because
/// `public_jwk` never holds it.
pub fn render_jwks(keys: &[ClientKey]) -> serde_json::Value {
    serde_json::json!({
        "keys": keys.iter().map(|k| k.public_jwk.clone()).collect::<Vec<_>>(),
    })
}

/// Insert a key. `private_jwk` is sealed with `enc` when one is supplied.
pub async fn insert_key(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    enc: Option<&[u8; 32]>,
    key: &ClientKey,
) -> Result<(), AppError> {
    let (blob, encrypted) = seal_private_jwk(enc, &key.private_jwk)?;
    let public = serde_json::to_string(&key.public_jwk)
        .map_err(|e| AppError::Internal(format!("failed to serialise public JWK: {e}")))?;

    let sql = adapt_sql(
        "INSERT INTO happyview_oauth_client_keys (kid, owner, alg, private_jwk, public_jwk, encrypted, status, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        backend,
    );

    crate::db::query(&sql)
        .bind(&key.kid)
        .bind(&key.owner)
        .bind(&key.alg)
        .bind(&blob)
        .bind(&public)
        .bind(i32::from(encrypted))
        .bind(key.status.as_str())
        .bind(now_rfc3339())
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to store client key: {e}")))?;

    Ok(())
}

/// Load an owner's usable keys, `current` first then `retiring`. Revoked keys
/// are never returned — they must not be published or signed with.
pub async fn load_keys(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    enc: Option<&[u8; 32]>,
    owner: &str,
) -> Result<Vec<ClientKey>, AppError> {
    let sql = adapt_sql(
        "SELECT kid, owner, alg, private_jwk, public_jwk, encrypted, status FROM happyview_oauth_client_keys WHERE owner = ? AND status != 'revoked' ORDER BY CASE status WHEN 'current' THEN 0 ELSE 1 END, created_at",
        backend,
    );

    #[allow(clippy::type_complexity)]
    let rows: Vec<(String, String, String, Vec<u8>, String, i32, String)> =
        crate::db::query_as(&sql)
            .bind(owner)
            .fetch_all(pool)
            .await
            .map_err(|e| AppError::Internal(format!("failed to load client keys: {e}")))?;

    rows.into_iter()
        .map(|(kid, owner, alg, blob, public, encrypted, status)| {
            Ok(ClientKey {
                kid,
                owner,
                alg,
                private_jwk: unseal_private_jwk(enc, &blob, encrypted != 0)?,
                public_jwk: serde_json::from_str(&public).map_err(|e| {
                    AppError::Internal(format!("failed to parse stored public JWK: {e}"))
                })?,
                status: KeyStatus::parse(&status).ok_or_else(|| {
                    AppError::Internal(format!("unknown client key status: {status}"))
                })?,
            })
        })
        .collect()
}

/// Mark every key an owner holds as revoked.
pub async fn revoke_keys_for_owner(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    owner: &str,
) -> Result<(), AppError> {
    let sql = adapt_sql(
        "UPDATE happyview_oauth_client_keys SET status = 'revoked', retired_at = ? WHERE owner = ? AND status != 'revoked'",
        backend,
    );
    crate::db::query(&sql)
        .bind(now_rfc3339())
        .bind(owner)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to revoke client keys: {e}")))?;
    Ok(())
}

/// Revoke exactly one key belonging to `owner`.
pub async fn revoke_key(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    owner: &str,
    kid: &str,
) -> Result<bool, AppError> {
    let sql = adapt_sql(
        "UPDATE happyview_oauth_client_keys SET status = 'revoked', retired_at = ? WHERE owner = ? AND kid = ? AND status != 'revoked'",
        backend,
    );
    let result = crate::db::query(&sql)
        .bind(now_rfc3339())
        .bind(owner)
        .bind(kid)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to revoke client key: {e}")))?;
    Ok(result.rows_affected() > 0)
}

/// One key's metadata for the admin listing, with no private material.
pub struct ClientKeySummary {
    pub kid: String,
    pub status: KeyStatus,
    pub created_at: String,
}

/// List every key an owner holds — `current` first, then `retiring`, then
/// `revoked` — for the admin listing.
pub async fn list_keys_for_owner(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    owner: &str,
) -> Result<Vec<ClientKeySummary>, AppError> {
    let sql = adapt_sql(
        "SELECT kid, status, created_at FROM happyview_oauth_client_keys WHERE owner = ? \
         ORDER BY CASE status WHEN 'current' THEN 0 WHEN 'retiring' THEN 1 ELSE 2 END, created_at",
        backend,
    );
    let rows: Vec<(String, String, String)> = crate::db::query_as(&sql)
        .bind(owner)
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list client keys: {e}")))?;

    rows.into_iter()
        .map(|(kid, status, created_at)| {
            Ok(ClientKeySummary {
                kid,
                status: KeyStatus::parse(&status).ok_or_else(|| {
                    AppError::Internal(format!("unknown client key status: {status}"))
                })?,
                created_at,
            })
        })
        .collect()
}

/// Count live sessions pinned to each `kid`, across all three tables that
/// carry `signing_kid` — `happyview_oauth_sessions`,
/// `happyview_linked_repo_sessions`, `happyview_dpop_sessions` (see
/// `migrations/.../20260818000002_add_signing_kid.sql`).
pub async fn session_counts_by_kid(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
) -> Result<std::collections::HashMap<String, u64>, AppError> {
    let mut counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();

    for table in [
        "happyview_oauth_sessions",
        "happyview_linked_repo_sessions",
        "happyview_dpop_sessions",
    ] {
        let sql = adapt_sql(
            &format!(
                "SELECT signing_kid, COUNT(*) FROM {table} WHERE signing_kid IS NOT NULL GROUP BY signing_kid"
            ),
            backend,
        );
        let rows: Vec<(String, i64)> = crate::db::query_as(&sql)
            .fetch_all(pool)
            .await
            .map_err(|e| AppError::Internal(format!("failed to count sessions in {table}: {e}")))?;
        for (kid, n) in rows {
            *counts.entry(kid).or_insert(0) += n.max(0) as u64;
        }
    }

    Ok(counts)
}

/// Pick the key a session must be signed with.
pub fn find_by_kid<'a>(keys: &'a [ClientKey], kid: Option<&str>) -> Option<&'a ClientKey> {
    match kid {
        Some(kid) => keys.iter().find(|k| k.kid == kid),
        None => keys.iter().find(|k| k.status == KeyStatus::Current),
    }
}

/// Turn `find_by_kid`'s result into the decision a refresh call site must
/// act on: sign with a key, sign with nothing (a public refresh), or refuse
/// outright.
pub fn resolve_signing_key<'a>(
    keys: &'a [ClientKey],
    signing_kid: Option<&str>,
) -> Result<Option<&'a ClientKey>, AppError> {
    match find_by_kid(keys, signing_kid) {
        Some(key) => Ok(Some(key)),
        None if signing_kid.is_some() => Err(AppError::Auth(
            "the key this session was established with is no longer available; the user must re-authenticate".into(),
        )),
        None => Ok(None),
    }
}

/// Generate a new current key, demoting the existing one to `retiring`.
pub async fn rotate_key(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    enc: Option<&[u8; 32]>,
    owner: &str,
) -> Result<ClientKey, AppError> {
    let key = generate_client_key(owner)?;
    let (blob, encrypted) = seal_private_jwk(enc, &key.private_jwk)?;
    let public = serde_json::to_string(&key.public_jwk)
        .map_err(|e| AppError::Internal(format!("failed to serialise public JWK: {e}")))?;

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| AppError::Internal(format!("failed to begin transaction: {e}")))?;

    let demote = adapt_sql(
        "UPDATE happyview_oauth_client_keys SET status = 'retiring', retired_at = ? WHERE owner = ? AND status = 'current'",
        backend,
    );
    crate::db::query(&demote)
        .bind(now_rfc3339())
        .bind(owner)
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(format!("failed to demote current key: {e}")))?;

    let insert = adapt_sql(
        "INSERT INTO happyview_oauth_client_keys (kid, owner, alg, private_jwk, public_jwk, encrypted, status, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        backend,
    );
    crate::db::query(&insert)
        .bind(&key.kid)
        .bind(&key.owner)
        .bind(&key.alg)
        .bind(&blob)
        .bind(&public)
        .bind(i32::from(encrypted))
        .bind(key.status.as_str())
        .bind(now_rfc3339())
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(format!("failed to store client key: {e}")))?;

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("failed to commit key rotation: {e}")))?;

    Ok(key)
}

/// Revoke `retiring` keys that no session is pinned to.
pub async fn retire_unused_keys(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
) -> Result<u64, AppError> {
    let sql = adapt_sql(
        "UPDATE happyview_oauth_client_keys SET status = 'revoked', retired_at = ? \
         WHERE status = 'retiring' \
           AND kid NOT IN (SELECT signing_kid FROM happyview_oauth_sessions WHERE signing_kid IS NOT NULL) \
           AND kid NOT IN (SELECT signing_kid FROM happyview_linked_repo_sessions WHERE signing_kid IS NOT NULL) \
           AND kid NOT IN (SELECT signing_kid FROM happyview_dpop_sessions WHERE signing_kid IS NOT NULL)",
        backend,
    );

    let result = crate::db::query(&sql)
        .bind(now_rfc3339())
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to retire client keys: {e}")))?;

    Ok(result.rows_affected())
}

pub async fn count_unstamped_sessions(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    owner: &str,
) -> Result<u64, AppError> {
    let count: i64 = if owner == INSTANCE_OWNER {
        let sql = adapt_sql(
            "SELECT \
                (SELECT COUNT(*) FROM happyview_oauth_sessions WHERE signing_kid IS NULL) + \
                (SELECT COUNT(*) FROM happyview_linked_repo_sessions WHERE signing_kid IS NULL)",
            backend,
        );
        let (n,): (i64,) = crate::db::query_as(&sql)
            .fetch_one(pool)
            .await
            .map_err(|e| AppError::Internal(format!("failed to count unstamped sessions: {e}")))?;
        n
    } else {
        let sql = adapt_sql(
            "SELECT COUNT(*) FROM happyview_dpop_sessions WHERE api_client_id = ? AND signing_kid IS NULL",
            backend,
        );
        let (n,): (i64,) = crate::db::query_as(&sql)
            .bind(owner)
            .fetch_one(pool)
            .await
            .map_err(|e| AppError::Internal(format!("failed to count unstamped sessions: {e}")))?;
        n
    };

    Ok(count.max(0) as u64)
}

/// Return the instance's current key, generating and storing one on first call.
pub async fn ensure_instance_key(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    enc: Option<&[u8; 32]>,
) -> Result<ClientKey, AppError> {
    if let Some(existing) = load_keys(pool, backend, enc, INSTANCE_OWNER)
        .await?
        .into_iter()
        .next()
    {
        return Ok(existing);
    }
    let key = generate_client_key(INSTANCE_OWNER)?;
    insert_key(pool, backend, enc, &key).await?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_key_has_kid_and_both_halves() {
        let key = generate_client_key(INSTANCE_OWNER).unwrap();
        assert_eq!(key.owner, INSTANCE_OWNER);
        assert_eq!(key.alg, "ES256");
        assert_eq!(key.status, KeyStatus::Current);
        assert!(!key.kid.is_empty());
        assert!(key.private_jwk["d"].is_string());
        assert!(key.public_jwk["d"].is_null());
        assert_eq!(key.public_jwk["kid"], key.kid);
        assert_eq!(key.public_jwk["alg"], "ES256");
        assert_eq!(key.public_jwk["use"], "sig");
    }

    #[test]
    fn generated_keys_are_unique() {
        let a = generate_client_key(INSTANCE_OWNER).unwrap();
        let b = generate_client_key(INSTANCE_OWNER).unwrap();
        assert_ne!(a.kid, b.kid);
        assert_ne!(a.private_jwk["d"], b.private_jwk["d"]);
    }

    #[test]
    fn seal_round_trips_with_a_key() {
        let enc = [7u8; 32];
        let key = generate_client_key(INSTANCE_OWNER).unwrap();
        let (blob, encrypted) = seal_private_jwk(Some(&enc), &key.private_jwk).unwrap();
        assert!(encrypted);
        assert_ne!(blob, serde_json::to_vec(&key.private_jwk).unwrap());
        let back = unseal_private_jwk(Some(&enc), &blob, true).unwrap();
        assert_eq!(back, key.private_jwk);
    }

    #[test]
    fn seal_round_trips_without_a_key() {
        let key = generate_client_key(INSTANCE_OWNER).unwrap();
        let (blob, encrypted) = seal_private_jwk(None, &key.private_jwk).unwrap();
        assert!(!encrypted);
        let back = unseal_private_jwk(None, &blob, false).unwrap();
        assert_eq!(back, key.private_jwk);
    }

    #[test]
    fn unsealing_an_encrypted_row_without_a_key_is_an_error() {
        let enc = [7u8; 32];
        let key = generate_client_key(INSTANCE_OWNER).unwrap();
        let (blob, _) = seal_private_jwk(Some(&enc), &key.private_jwk).unwrap();
        assert!(unseal_private_jwk(None, &blob, true).is_err());
    }

    #[test]
    fn atrium_jwk_carries_the_kid_and_accepts_into_a_keyset() {
        let key = generate_client_key(INSTANCE_OWNER).unwrap();
        let jwk = to_atrium_jwk(&key).unwrap();
        assert_eq!(jwk.prm.kid.as_deref(), Some(key.kid.as_str()));
    }

    #[test]
    fn atrium_jwk_carries_the_private_scalar() {
        // Signing needs `d`, not just the public point. If a future refactor
        // dropped it, the failure would surface far from here, as an
        // authorization error from someone else's PDS.
        let key = generate_client_key(INSTANCE_OWNER).unwrap();
        let jwk = to_atrium_jwk(&key).unwrap();
        assert!(matches!(&jwk.key, jose_jwk::Key::Ec(ec) if ec.d.is_some()));
    }

    #[test]
    fn rendered_jwks_omits_private_material() {
        let a = generate_client_key(INSTANCE_OWNER).unwrap();
        let b = generate_client_key(INSTANCE_OWNER).unwrap();
        let jwks = render_jwks(&[a.clone(), b.clone()]);
        let keys = jwks["keys"].as_array().unwrap();
        assert_eq!(keys.len(), 2);
        for k in keys {
            assert!(k["d"].is_null());
            assert!(k["kid"].is_string());
        }
        assert_eq!(keys[0]["kid"], a.kid);
        assert_eq!(keys[1]["kid"], b.kid);
    }

    #[test]
    fn key_status_round_trips() {
        for s in [KeyStatus::Current, KeyStatus::Retiring, KeyStatus::Revoked] {
            assert_eq!(KeyStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(KeyStatus::parse("nonsense"), None);
    }

    #[test]
    fn find_by_kid_prefers_the_named_key_over_current() {
        let mut retiring = generate_client_key("client-a").unwrap();
        retiring.status = KeyStatus::Retiring;
        let current = generate_client_key("client-a").unwrap();
        let keys = vec![current.clone(), retiring.clone()];

        let found = find_by_kid(&keys, Some(&retiring.kid)).unwrap();
        assert_eq!(found.kid, retiring.kid);
    }

    #[test]
    fn find_by_kid_falls_back_to_current_when_unpinned() {
        let mut retiring = generate_client_key("client-a").unwrap();
        retiring.status = KeyStatus::Retiring;
        let current = generate_client_key("client-a").unwrap();
        let keys = vec![current.clone(), retiring.clone()];

        let found = find_by_kid(&keys, None).unwrap();
        assert_eq!(found.kid, current.kid);
    }

    #[test]
    fn find_by_kid_returns_none_for_a_revoked_or_missing_kid() {
        let current = generate_client_key("client-a").unwrap();
        assert!(find_by_kid(&[current], Some("gone")).is_none());
    }

    #[test]
    fn resolve_signing_key_signs_with_the_pinned_key() {
        let mut retiring = generate_client_key("client-a").unwrap();
        retiring.status = KeyStatus::Retiring;
        let current = generate_client_key("client-a").unwrap();
        let keys = vec![current, retiring.clone()];

        let resolved = resolve_signing_key(&keys, Some(&retiring.kid)).unwrap();
        assert_eq!(resolved.unwrap().kid, retiring.kid);
    }

    #[test]
    fn resolve_signing_key_stays_public_when_unpinned_and_no_key_exists() {
        let resolved = resolve_signing_key(&[], None).unwrap();
        assert!(resolved.is_none());
    }

    #[test]
    fn resolve_signing_key_refuses_when_the_pinned_kid_is_gone() {
        let current = generate_client_key("client-a").unwrap();
        let keys = vec![current];

        let err = resolve_signing_key(&keys, Some("gone")).unwrap_err();
        assert!(matches!(err, AppError::Auth(_)));
    }
}
