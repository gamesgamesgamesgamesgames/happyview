//! A confidential API client's secrets. A client holds any number, each
//! labelled and revocable on its own, so a secret is rotated by adding one,
//! moving deployments onto it and revoking the old one, rather than by
//! recreating the client.
//!
//! Only a secret's SHA-256 is stored. The plaintext exists once, in the
//! response that creates it, and is never logged.

use rand::Rng;
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::rate_limit::SecretHash;

/// How often a secret's `last_used_at` is written at most. The column answers
/// "is anything still using this?", which a minute's resolution answers as
/// well as a write on every request would.
pub const USE_RECORD_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// The label a client's first secret gets: the one created with the client,
/// or the one it held before clients could have several.
pub const ORIGINAL_LABEL: &str = "Original";

pub const MAX_LABEL_LEN: usize = 100;

/// A secret as the admin API lists it. There is no field for the secret or
/// its hash.
#[derive(Debug, Clone, Serialize)]
pub struct ClientSecret {
    pub id: String,
    pub label: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

pub fn hash(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

fn generate() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    format!("hvs_{}", hex::encode(bytes))
}

/// A label as stored: trimmed, and refused when empty or too long.
pub fn checked_label(label: &str) -> Result<String, AppError> {
    let label = label.trim();
    if label.is_empty() {
        return Err(AppError::BadRequest("a secret needs a label".into()));
    }
    if label.chars().count() > MAX_LABEL_LEN {
        return Err(AppError::BadRequest(format!(
            "a secret's label is at most {MAX_LABEL_LEN} characters"
        )));
    }
    Ok(label.to_string())
}

/// Create a secret for `client_id` and return it with its plaintext, which
/// nothing can recover afterwards.
pub async fn create(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    client_id: &str,
    label: &str,
) -> Result<(ClientSecret, String), AppError> {
    let secret = generate();
    let row = ClientSecret {
        id: Uuid::new_v4().to_string(),
        label: label.to_string(),
        created_at: now_rfc3339(),
        last_used_at: None,
        revoked_at: None,
    };
    let sql = adapt_sql(
        "INSERT INTO happyview_api_client_secrets (id, client_id, label, secret_hash, created_at) VALUES (?, ?, ?, ?, ?)",
        backend,
    );
    crate::db::query(&sql)
        .bind(&row.id)
        .bind(client_id)
        .bind(&row.label)
        .bind(hash(&secret))
        .bind(&row.created_at)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create client secret: {e}")))?;
    Ok((row, secret))
}

/// Every secret `client_id` has held, revoked ones included, oldest first.
pub async fn list(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    client_id: &str,
) -> Result<Vec<ClientSecret>, AppError> {
    let sql = adapt_sql(
        "SELECT id, label, created_at, last_used_at, revoked_at FROM happyview_api_client_secrets WHERE client_id = ? ORDER BY created_at, id",
        backend,
    );
    type Row = (String, String, String, Option<String>, Option<String>);
    let rows: Vec<Row> = crate::db::query_as(&sql)
        .bind(client_id)
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list client secrets: {e}")))?;
    Ok(rows
        .into_iter()
        .map(
            |(id, label, created_at, last_used_at, revoked_at)| ClientSecret {
                id,
                label,
                created_at,
                last_used_at,
                revoked_at,
            },
        )
        .collect())
}

/// The hashes of `client_id`'s secrets that have not been revoked.
pub async fn active_hashes(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    client_id: &str,
) -> Result<Vec<SecretHash>, AppError> {
    let sql = adapt_sql(
        "SELECT id, secret_hash FROM happyview_api_client_secrets WHERE client_id = ? AND revoked_at IS NULL",
        backend,
    );
    let rows: Vec<(String, String)> = crate::db::query_as(&sql)
        .bind(client_id)
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to read client secrets: {e}")))?;
    Ok(rows
        .into_iter()
        .map(|(id, hash)| SecretHash { id, hash })
        .collect())
}

/// Register `client_key` for request validation with the secrets
/// `client_id` holds now. Every path that registers a client goes through
/// here, so the cached identity is always built from the database.
pub async fn register_identity(
    rate_limiter: &crate::rate_limit::RateLimiter,
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    client_id: &str,
    client_key: &str,
    client_uri: &str,
) -> Result<(), AppError> {
    let secrets = active_hashes(pool, backend, client_id).await?;
    rate_limiter.register_client_identity(
        client_key.to_string(),
        crate::rate_limit::ClientIdentity {
            secrets,
            client_uri: client_uri.to_string(),
        },
    );
    Ok(())
}

/// The id of the secret among `candidates` that `secret` is, compared in
/// constant time against every one so the answer's timing says nothing
/// about which, or how many, there are.
pub fn matching(candidates: &[SecretHash], secret: &str) -> Option<String> {
    let presented = hash(secret);
    let mut found = None;
    for candidate in candidates {
        if crate::constant_time::ct_eq_str(&presented, &candidate.hash) && found.is_none() {
            found = Some(candidate.id.clone());
        }
    }
    found
}

pub enum Revoked {
    /// The secret was live and is now revoked.
    Now,
    /// It had been revoked already; nothing changed.
    Already,
    /// `client_id` holds no secret with that id.
    NotFound,
}

/// Revoke one of `client_id`'s secrets, leaving the others as they are.
pub async fn revoke(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    client_id: &str,
    secret_id: &str,
) -> Result<Revoked, AppError> {
    let sql = adapt_sql(
        "UPDATE happyview_api_client_secrets SET revoked_at = ? WHERE id = ? AND client_id = ? AND revoked_at IS NULL",
        backend,
    );
    let changed = crate::db::query(&sql)
        .bind(now_rfc3339())
        .bind(secret_id)
        .bind(client_id)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to revoke client secret: {e}")))?
        .rows_affected();
    if changed > 0 {
        return Ok(Revoked::Now);
    }
    let exists_sql = adapt_sql(
        "SELECT id FROM happyview_api_client_secrets WHERE id = ? AND client_id = ?",
        backend,
    );
    let exists: Option<(String,)> = crate::db::query_as(&exists_sql)
        .bind(secret_id)
        .bind(client_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to read client secret: {e}")))?;
    Ok(if exists.is_some() {
        Revoked::Already
    } else {
        Revoked::NotFound
    })
}

/// Note that `secret_id` authenticated a request, unless that was noted
/// within the last [`USE_RECORD_INTERVAL`]. A failure is logged and dropped:
/// it must never fail the request it describes.
pub async fn record_use(pool: &sqlx::AnyPool, backend: DatabaseBackend, secret_id: &str) {
    let now = chrono::Utc::now();
    let cutoff = (now - USE_RECORD_INTERVAL).to_rfc3339();
    let sql = adapt_sql(
        "UPDATE happyview_api_client_secrets SET last_used_at = ? WHERE id = ? AND (last_used_at IS NULL OR last_used_at < ?)",
        backend,
    );
    if let Err(e) = crate::db::query(&sql)
        .bind(now.to_rfc3339())
        .bind(secret_id)
        .bind(cutoff)
        .execute(pool)
        .await
    {
        tracing::warn!(secret_id, error = %e, "could not record a client secret's use");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, secret: &str) -> SecretHash {
        SecretHash {
            id: id.into(),
            hash: hash(secret),
        }
    }

    #[test]
    fn a_secret_matches_whichever_credential_it_is() {
        let candidates = [candidate("a", "hvs_one"), candidate("b", "hvs_two")];
        assert_eq!(matching(&candidates, "hvs_one").as_deref(), Some("a"));
        assert_eq!(matching(&candidates, "hvs_two").as_deref(), Some("b"));
        assert_eq!(matching(&candidates, "hvs_three"), None);
        assert_eq!(matching(&[], "hvs_one"), None);
    }

    #[test]
    fn a_label_is_trimmed_and_must_say_something() {
        assert_eq!(checked_label("  staging ").unwrap(), "staging");
        assert!(checked_label("   ").is_err());
        assert!(checked_label(&"x".repeat(MAX_LABEL_LEN + 1)).is_err());
        assert!(checked_label(&"x".repeat(MAX_LABEL_LEN)).is_ok());
    }
}
