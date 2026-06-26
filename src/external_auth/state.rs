//! OAuth state management for external auth flows.
//!
//! Stores state -> (user_did, plugin_id, redirect_uri) mappings
//! to validate callbacks and associate external accounts with users.

use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("State not found or expired")]
    NotFound,
}

/// Stored OAuth state
#[derive(Debug, Clone)]
pub struct StoredState {
    pub did: String,
    pub plugin_id: String,
    pub redirect_uri: String,
}

/// Store OAuth state for an auth flow.
///
/// State expires after 10 minutes.
pub async fn store_state(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    state: &str,
    did: &str,
    plugin_id: &str,
    redirect_uri: &str,
) -> Result<(), StateError> {
    let now = now_rfc3339();

    // Expire in 10 minutes
    let expires_at = chrono::Utc::now() + chrono::Duration::minutes(10);
    let expires_str = expires_at.to_rfc3339();

    let sql = adapt_sql(
        "INSERT INTO happyview_external_auth_state (state, did, plugin_id, redirect_uri, created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?)",
        backend,
    );

    sqlx::query(&sql)
        .bind(state)
        .bind(did)
        .bind(plugin_id)
        .bind(redirect_uri)
        .bind(&now)
        .bind(&expires_str)
        .execute(db)
        .await?;

    Ok(())
}

/// Retrieve and consume OAuth state.
///
/// Returns the stored state if found and not expired, then deletes it.
pub async fn consume_state(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    state: &str,
) -> Result<StoredState, StateError> {
    let now = now_rfc3339();

    // Get state if not expired
    let sql = adapt_sql(
        "SELECT did, plugin_id, redirect_uri FROM happyview_external_auth_state WHERE state = ? AND expires_at > ?",
        backend,
    );

    let row: Option<(String, String, String)> = sqlx::query_as(&sql)
        .bind(state)
        .bind(&now)
        .fetch_optional(db)
        .await?;

    let (did, plugin_id, redirect_uri) = row.ok_or(StateError::NotFound)?;

    // Delete the state (one-time use)
    let delete_sql = adapt_sql(
        "DELETE FROM happyview_external_auth_state WHERE state = ?",
        backend,
    );
    sqlx::query(&delete_sql).bind(state).execute(db).await?;

    Ok(StoredState {
        did,
        plugin_id,
        redirect_uri,
    })
}

/// Clean up expired state entries.
pub async fn cleanup_expired(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
) -> Result<u64, StateError> {
    let now = now_rfc3339();

    let sql = adapt_sql(
        "DELETE FROM happyview_external_auth_state WHERE expires_at <= ?",
        backend,
    );
    let result = sqlx::query(&sql).bind(&now).execute(db).await?;

    Ok(result.rows_affected())
}
