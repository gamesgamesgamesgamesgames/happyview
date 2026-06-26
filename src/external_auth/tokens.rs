//! External account token storage with encryption.

use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::plugin::encryption::{EncryptionError, decrypt, encrypt};

/// Row type for token query results
type TokenRow = (
    String,
    Vec<u8>,
    Option<Vec<u8>>,
    Option<String>,
    Option<String>,
    Option<String>,
);

#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("Encryption error: {0}")]
    Encryption(#[from] EncryptionError),
    #[error("Token encryption key not configured")]
    KeyNotConfigured,
    #[error("Token not found")]
    NotFound,
}

/// Stored external account token set
#[derive(Debug, Clone)]
pub struct StoredTokens {
    pub account_id: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub token_type: Option<String>,
    pub scope: Option<String>,
    pub expires_at: Option<String>,
}

/// Store tokens for an external account link
#[allow(clippy::too_many_arguments)]
pub async fn store_tokens(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    encryption_key: Option<&[u8; 32]>,
    did: &str,
    plugin_id: &str,
    account_id: &str,
    access_token: &str,
    refresh_token: Option<&str>,
    token_type: Option<&str>,
    scope: Option<&str>,
    expires_at: Option<&str>,
) -> Result<(), TokenError> {
    let key = encryption_key.ok_or(TokenError::KeyNotConfigured)?;

    let encrypted_access = encrypt(key, access_token.as_bytes())?;
    let encrypted_refresh = refresh_token
        .map(|t| encrypt(key, t.as_bytes()))
        .transpose()?;

    let id = uuid::Uuid::new_v4().to_string();
    let now = now_rfc3339();

    let sql = adapt_sql(
        "INSERT INTO happyview_external_account_tokens (id, did, plugin_id, account_id, access_token, refresh_token, token_type, scope, expires_at, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (did, plugin_id)
         DO UPDATE SET account_id = excluded.account_id, access_token = excluded.access_token, refresh_token = excluded.refresh_token, token_type = excluded.token_type, scope = excluded.scope, expires_at = excluded.expires_at, updated_at = excluded.updated_at",
        backend,
    );

    sqlx::query(&sql)
        .bind(&id)
        .bind(did)
        .bind(plugin_id)
        .bind(account_id)
        .bind(&encrypted_access)
        .bind(encrypted_refresh.as_deref())
        .bind(token_type)
        .bind(scope)
        .bind(expires_at)
        .bind(&now)
        .bind(&now)
        .execute(db)
        .await?;

    Ok(())
}

/// Retrieve decrypted tokens for an external account
pub async fn get_tokens(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    encryption_key: Option<&[u8; 32]>,
    did: &str,
    plugin_id: &str,
) -> Result<StoredTokens, TokenError> {
    let key = encryption_key.ok_or(TokenError::KeyNotConfigured)?;

    let sql = adapt_sql(
        "SELECT account_id, access_token, refresh_token, token_type, scope, expires_at FROM happyview_external_account_tokens WHERE did = ? AND plugin_id = ?",
        backend,
    );

    let row: Option<TokenRow> = sqlx::query_as(&sql)
        .bind(did)
        .bind(plugin_id)
        .fetch_optional(db)
        .await?;

    let (account_id, encrypted_access, encrypted_refresh, token_type, scope, expires_at) =
        row.ok_or(TokenError::NotFound)?;

    let access_token = String::from_utf8(decrypt(key, &encrypted_access)?)
        .map_err(|_| EncryptionError::DecryptionFailed)?;

    let refresh_token = encrypted_refresh
        .map(|enc| {
            decrypt(key, &enc).and_then(|dec| {
                String::from_utf8(dec).map_err(|_| EncryptionError::DecryptionFailed)
            })
        })
        .transpose()?;

    Ok(StoredTokens {
        account_id,
        access_token,
        refresh_token,
        token_type,
        scope,
        expires_at,
    })
}

/// Delete tokens for an external account link
pub async fn delete_tokens(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    did: &str,
    plugin_id: &str,
) -> Result<bool, TokenError> {
    let sql = adapt_sql(
        "DELETE FROM happyview_external_account_tokens WHERE did = ? AND plugin_id = ?",
        backend,
    );

    let result = sqlx::query(&sql)
        .bind(did)
        .bind(plugin_id)
        .execute(db)
        .await?;

    Ok(result.rows_affected() > 0)
}

/// Check if an external account is linked
pub async fn is_linked(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    did: &str,
    plugin_id: &str,
) -> Result<bool, TokenError> {
    let sql = adapt_sql(
        "SELECT 1 FROM happyview_external_account_tokens WHERE did = ? AND plugin_id = ?",
        backend,
    );

    let exists: Option<(i32,)> = sqlx::query_as(&sql)
        .bind(did)
        .bind(plugin_id)
        .fetch_optional(db)
        .await?;

    Ok(exists.is_some())
}

/// Get the external account ID for a linked account
pub async fn get_account_id(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    did: &str,
    plugin_id: &str,
) -> Result<Option<String>, TokenError> {
    let sql = adapt_sql(
        "SELECT account_id FROM happyview_external_account_tokens WHERE did = ? AND plugin_id = ?",
        backend,
    );

    let row: Option<(String,)> = sqlx::query_as(&sql)
        .bind(did)
        .bind(plugin_id)
        .fetch_optional(db)
        .await?;

    Ok(row.map(|(id,)| id))
}

/// Summary of a linked external account (without tokens)
#[derive(Debug, Clone, serde::Serialize)]
pub struct LinkedAccountSummary {
    pub plugin_id: String,
    pub account_id: String,
    pub created_at: String,
    pub updated_at: String,
}

/// List all linked external accounts for a user
pub async fn list_linked_accounts(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    did: &str,
) -> Result<Vec<LinkedAccountSummary>, TokenError> {
    let sql = adapt_sql(
        "SELECT plugin_id, account_id, created_at, updated_at FROM happyview_external_account_tokens WHERE did = ? ORDER BY created_at DESC",
        backend,
    );

    let rows: Vec<(String, String, String, String)> =
        sqlx::query_as(&sql).bind(did).fetch_all(db).await?;

    Ok(rows
        .into_iter()
        .map(
            |(plugin_id, account_id, created_at, updated_at)| LinkedAccountSummary {
                plugin_id,
                account_id,
                created_at,
                updated_at,
            },
        )
        .collect())
}

// Integration tests for token storage are in tests/e2e_external_auth.rs
