use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::plugin::encryption::{decrypt, encrypt};

/// Stored DPoP session data (decrypted).
pub struct DpopSession {
    pub id: String,
    pub api_client_id: String,
    pub dpop_key_id: String,
    pub user_did: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub token_expires_at: Option<String>,
    pub scopes: String,
    pub pds_url: Option<String>,
    pub issuer: Option<String>,
    pub signing_kid: Option<String>,
}

/// Session metadata returned by list_dpop_sessions (no decrypted tokens).
pub struct DpopSessionInfo {
    pub id: String,
    pub dpop_key_id: String,
    pub scopes: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Store or update a DPoP session.
///
/// Uses ON CONFLICT to upsert — if a session already exists for this
/// (api_client_id, user_did, dpop_key_id), it updates the token data.
#[allow(clippy::too_many_arguments)]
pub async fn store_dpop_session(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    encryption_key: &[u8; 32],
    id: &str,
    api_client_id: &str,
    dpop_key_id: &str,
    user_did: &str,
    access_token: &str,
    refresh_token: Option<&str>,
    token_expires_at: Option<&str>,
    scopes: &str,
    pds_url: Option<&str>,
    issuer: Option<&str>,
    signing_kid: Option<&str>,
) -> Result<(), AppError> {
    let access_enc = encrypt(encryption_key, access_token.as_bytes())
        .map_err(|e| AppError::Internal(format!("failed to encrypt access token: {e}")))?;

    let refresh_enc = refresh_token
        .map(|t| {
            encrypt(encryption_key, t.as_bytes())
                .map_err(|e| AppError::Internal(format!("failed to encrypt refresh token: {e}")))
        })
        .transpose()?;

    let now = now_rfc3339();
    let sql = adapt_sql(
        r#"INSERT INTO happyview_dpop_sessions (id, api_client_id, dpop_key_id, user_did, access_token_enc, refresh_token_enc, token_expires_at, scopes, pds_url, issuer, signing_kid, created_at, updated_at)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
           ON CONFLICT (api_client_id, user_did, dpop_key_id) DO UPDATE SET
               access_token_enc = EXCLUDED.access_token_enc,
               refresh_token_enc = EXCLUDED.refresh_token_enc,
               token_expires_at = EXCLUDED.token_expires_at,
               scopes = EXCLUDED.scopes,
               pds_url = EXCLUDED.pds_url,
               issuer = EXCLUDED.issuer,
               updated_at = EXCLUDED.updated_at"#,
        backend,
    );

    crate::db::query(&sql)
        .bind(id)
        .bind(api_client_id)
        .bind(dpop_key_id)
        .bind(user_did)
        .bind(&access_enc)
        .bind(&refresh_enc)
        .bind(token_expires_at)
        .bind(scopes)
        .bind(pds_url)
        .bind(issuer)
        .bind(signing_kid)
        .bind(&now)
        .bind(&now)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to store DPoP session: {e}")))?;

    Ok(())
}

pub async fn repin_dpop_session_signing_kid(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    api_client_id: &str,
    dpop_key_id: &str,
    user_did: &str,
    signing_kid: Option<&str>,
) -> Result<(), AppError> {
    let sql = adapt_sql(
        "UPDATE happyview_dpop_sessions SET signing_kid = ? \
         WHERE api_client_id = ? AND dpop_key_id = ? AND user_did = ?",
        backend,
    );
    crate::db::query(&sql)
        .bind(signing_kid)
        .bind(api_client_id)
        .bind(dpop_key_id)
        .bind(user_did)
        .execute(pool)
        .await
        .map_err(|e| {
            AppError::Internal(format!("failed to re-pin DPoP session signing_kid: {e}"))
        })?;
    Ok(())
}

/// Look up a DPoP session by api_client_id, user_did, and dpop_key_id, decrypting tokens.
pub async fn get_dpop_session(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    encryption_key: &[u8; 32],
    api_client_id: &str,
    user_did: &str,
    dpop_key_id: &str,
) -> Result<DpopSession, AppError> {
    let sql = adapt_sql(
        "SELECT id, access_token_enc, refresh_token_enc, token_expires_at, scopes, pds_url, issuer, signing_kid FROM happyview_dpop_sessions WHERE api_client_id = ? AND user_did = ? AND dpop_key_id = ?",
        backend,
    );

    #[allow(clippy::type_complexity)]
    let row: Option<(
        String,
        Vec<u8>,
        Option<Vec<u8>>,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = crate::db::query_as(&sql)
        .bind(api_client_id)
        .bind(user_did)
        .bind(dpop_key_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up DPoP session: {e}")))?;

    let (id, access_enc, refresh_enc, token_expires_at, scopes, pds_url, issuer, signing_kid) =
        row.ok_or_else(|| AppError::NotFound("DPoP session not found".into()))?;

    let access_token = String::from_utf8(
        decrypt(encryption_key, &access_enc)
            .map_err(|e| AppError::Internal(format!("failed to decrypt access token: {e}")))?,
    )
    .map_err(|e| AppError::Internal(format!("invalid access token bytes: {e}")))?;

    let refresh_token = refresh_enc
        .map(|enc| {
            let bytes = decrypt(encryption_key, &enc)
                .map_err(|e| AppError::Internal(format!("failed to decrypt refresh token: {e}")))?;
            String::from_utf8(bytes)
                .map_err(|e| AppError::Internal(format!("invalid refresh token bytes: {e}")))
        })
        .transpose()?;

    Ok(DpopSession {
        id,
        api_client_id: api_client_id.to_string(),
        dpop_key_id: dpop_key_id.to_string(),
        user_did: user_did.to_string(),
        access_token,
        refresh_token,
        token_expires_at,
        scopes,
        pds_url,
        issuer,
        signing_kid,
    })
}

/// Look up a DPoP session by api_client_id and dpop_key_id, decrypting tokens.
/// Used by the auth middleware where the key ID is derived from the DPoP proof thumbprint.
/// Look up just the granted scopes for a session.
///
/// Deliberately does not decrypt the tokens: a scope check has no business
/// touching them, and this runs on every forwarded request.
pub async fn get_dpop_session_scopes(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    api_client_id: &str,
    dpop_key_id: &str,
) -> Result<Option<String>, AppError> {
    let sql = adapt_sql(
        "SELECT scopes FROM happyview_dpop_sessions WHERE api_client_id = ? AND dpop_key_id = ?",
        backend,
    );

    let row: Option<(String,)> = crate::db::query_as(&sql)
        .bind(api_client_id)
        .bind(dpop_key_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up DPoP session scopes: {e}")))?;

    Ok(row.map(|(scopes,)| scopes))
}

pub async fn get_dpop_session_by_key_id(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    encryption_key: &[u8; 32],
    api_client_id: &str,
    dpop_key_id: &str,
) -> Result<DpopSession, AppError> {
    let sql = adapt_sql(
        "SELECT id, user_did, access_token_enc, refresh_token_enc, token_expires_at, scopes, pds_url, issuer, signing_kid FROM happyview_dpop_sessions WHERE api_client_id = ? AND dpop_key_id = ?",
        backend,
    );

    #[allow(clippy::type_complexity)]
    let row: Option<(
        String,
        String,
        Vec<u8>,
        Option<Vec<u8>>,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = crate::db::query_as(&sql)
        .bind(api_client_id)
        .bind(dpop_key_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up DPoP session: {e}")))?;

    let (
        id,
        user_did,
        access_enc,
        refresh_enc,
        token_expires_at,
        scopes,
        pds_url,
        issuer,
        signing_kid,
    ) = row.ok_or_else(|| AppError::Auth("no matching DPoP session".into()))?;

    let access_token = String::from_utf8(
        decrypt(encryption_key, &access_enc)
            .map_err(|e| AppError::Internal(format!("failed to decrypt access token: {e}")))?,
    )
    .map_err(|e| AppError::Internal(format!("invalid access token bytes: {e}")))?;

    let refresh_token = refresh_enc
        .map(|enc| {
            let bytes = decrypt(encryption_key, &enc)
                .map_err(|e| AppError::Internal(format!("failed to decrypt refresh token: {e}")))?;
            String::from_utf8(bytes)
                .map_err(|e| AppError::Internal(format!("invalid refresh token bytes: {e}")))
        })
        .transpose()?;

    Ok(DpopSession {
        id,
        api_client_id: api_client_id.to_string(),
        dpop_key_id: dpop_key_id.to_string(),
        user_did,
        access_token,
        refresh_token,
        token_expires_at,
        scopes,
        pds_url,
        issuer,
        signing_kid,
    })
}

/// Every DPoP key id this user holds a session under with this client.
pub async fn dpop_key_ids_for_user(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    api_client_id: &str,
    user_did: &str,
) -> Result<Vec<String>, AppError> {
    let sql = adapt_sql(
        "SELECT dpop_key_id FROM happyview_dpop_sessions WHERE api_client_id = ? AND user_did = ?",
        backend,
    );
    let rows: Vec<(String,)> = crate::db::query_as(&sql)
        .bind(api_client_id)
        .bind(user_did)
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list DPoP session keys: {e}")))?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// The DPoP key id backing one session row, checked against its owner.
pub async fn dpop_key_id_for_session(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    session_id: &str,
    api_client_id: &str,
    user_did: &str,
) -> Result<Option<String>, AppError> {
    let sql = adapt_sql(
        "SELECT dpop_key_id FROM happyview_dpop_sessions WHERE id = ? AND api_client_id = ? AND user_did = ?",
        backend,
    );
    let row: Option<(String,)> = crate::db::query_as(&sql)
        .bind(session_id)
        .bind(api_client_id)
        .bind(user_did)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up DPoP session: {e}")))?;
    Ok(row.map(|(id,)| id))
}

/// Delete a DPoP session by api_client_id, user_did, and dpop_key_id (device-specific).
pub async fn delete_dpop_session(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    api_client_id: &str,
    user_did: &str,
    dpop_key_id: &str,
) -> Result<String, AppError> {
    let del_session_sql = adapt_sql(
        "DELETE FROM happyview_dpop_sessions WHERE api_client_id = ? AND user_did = ? AND dpop_key_id = ?",
        backend,
    );
    crate::db::query(&del_session_sql)
        .bind(api_client_id)
        .bind(user_did)
        .bind(dpop_key_id)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete DPoP session: {e}")))?;

    let del_key_sql = adapt_sql("DELETE FROM happyview_dpop_keys WHERE id = ?", backend);
    crate::db::query(&del_key_sql)
        .bind(dpop_key_id)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete DPoP key: {e}")))?;

    Ok(dpop_key_id.to_string())
}

/// Delete all DPoP sessions for a user+client pair (e.g. on account unlink).
pub async fn delete_all_dpop_sessions(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    api_client_id: &str,
    user_did: &str,
) -> Result<(), AppError> {
    let key_ids_sql = adapt_sql(
        "SELECT dpop_key_id FROM happyview_dpop_sessions WHERE api_client_id = ? AND user_did = ?",
        backend,
    );
    let key_ids: Vec<(String,)> = crate::db::query_as(&key_ids_sql)
        .bind(api_client_id)
        .bind(user_did)
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list DPoP sessions: {e}")))?;

    let del_sessions_sql = adapt_sql(
        "DELETE FROM happyview_dpop_sessions WHERE api_client_id = ? AND user_did = ?",
        backend,
    );
    crate::db::query(&del_sessions_sql)
        .bind(api_client_id)
        .bind(user_did)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete DPoP sessions: {e}")))?;

    let del_key_sql = adapt_sql("DELETE FROM happyview_dpop_keys WHERE id = ?", backend);
    for (key_id,) in key_ids {
        let _ = crate::db::query(&del_key_sql)
            .bind(&key_id)
            .execute(pool)
            .await;
    }

    Ok(())
}

/// List all DPoP sessions for a user+client pair (metadata only, no decrypted tokens).
pub async fn list_dpop_sessions(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    api_client_id: &str,
    user_did: &str,
) -> Result<Vec<DpopSessionInfo>, AppError> {
    let sql = adapt_sql(
        "SELECT id, dpop_key_id, scopes, created_at, updated_at FROM happyview_dpop_sessions WHERE api_client_id = ? AND user_did = ?",
        backend,
    );

    let rows: Vec<(String, String, String, String, String)> = crate::db::query_as(&sql)
        .bind(api_client_id)
        .bind(user_did)
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list DPoP sessions: {e}")))?;

    Ok(rows
        .into_iter()
        .map(
            |(id, dpop_key_id, scopes, created_at, updated_at)| DpopSessionInfo {
                id,
                dpop_key_id,
                scopes,
                created_at,
                updated_at,
            },
        )
        .collect())
}

/// Look up a DPoP session by api_client_id and user_did only (without dpop_key_id).
/// Used when the caller doesn't know the specific device key — delegation writes
/// and confidential client session lookups. Returns the first matching session.
pub async fn get_dpop_session_for_user(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    encryption_key: &[u8; 32],
    api_client_id: &str,
    user_did: &str,
) -> Result<DpopSession, AppError> {
    let sql = adapt_sql(
        "SELECT id, dpop_key_id, access_token_enc, refresh_token_enc, token_expires_at, scopes, pds_url, issuer, signing_kid FROM happyview_dpop_sessions WHERE api_client_id = ? AND user_did = ? LIMIT 1",
        backend,
    );

    #[allow(clippy::type_complexity)]
    let row: Option<(
        String,
        String,
        Vec<u8>,
        Option<Vec<u8>>,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = crate::db::query_as(&sql)
        .bind(api_client_id)
        .bind(user_did)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up DPoP session: {e}")))?;

    let (
        id,
        dpop_key_id,
        access_enc,
        refresh_enc,
        token_expires_at,
        scopes,
        pds_url,
        issuer,
        signing_kid,
    ) = row.ok_or_else(|| AppError::NotFound("DPoP session not found".into()))?;

    let access_token = String::from_utf8(
        decrypt(encryption_key, &access_enc)
            .map_err(|e| AppError::Internal(format!("failed to decrypt access token: {e}")))?,
    )
    .map_err(|e| AppError::Internal(format!("invalid access token bytes: {e}")))?;

    let refresh_token = refresh_enc
        .map(|enc| {
            let bytes = decrypt(encryption_key, &enc)
                .map_err(|e| AppError::Internal(format!("failed to decrypt refresh token: {e}")))?;
            String::from_utf8(bytes)
                .map_err(|e| AppError::Internal(format!("invalid refresh token bytes: {e}")))
        })
        .transpose()?;

    Ok(DpopSession {
        id,
        api_client_id: api_client_id.to_string(),
        dpop_key_id,
        user_did: user_did.to_string(),
        access_token,
        refresh_token,
        token_expires_at,
        scopes,
        pds_url,
        issuer,
        signing_kid,
    })
}

/// Delete a specific DPoP session by its ID, verifying it belongs to the given client and user.
pub async fn delete_dpop_session_by_id(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    session_id: &str,
    api_client_id: &str,
    user_did: &str,
) -> Result<String, AppError> {
    let lookup_sql = adapt_sql(
        "SELECT dpop_key_id FROM happyview_dpop_sessions WHERE id = ? AND api_client_id = ? AND user_did = ?",
        backend,
    );
    let row: Option<(String,)> = crate::db::query_as(&lookup_sql)
        .bind(session_id)
        .bind(api_client_id)
        .bind(user_did)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up DPoP session: {e}")))?;

    let (dpop_key_id,) = row.ok_or_else(|| AppError::NotFound("DPoP session not found".into()))?;

    let del_session_sql = adapt_sql("DELETE FROM happyview_dpop_sessions WHERE id = ?", backend);
    crate::db::query(&del_session_sql)
        .bind(session_id)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete DPoP session: {e}")))?;

    let del_key_sql = adapt_sql("DELETE FROM happyview_dpop_keys WHERE id = ?", backend);
    crate::db::query(&del_key_sql)
        .bind(&dpop_key_id)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete DPoP key: {e}")))?;

    Ok(dpop_key_id)
}
