use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use hex;
use rand::Rng;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};

use super::auth::UserAuth;
use super::permissions::Permission;
use super::types::{ApiKeySummary, CreateApiKeyBody, CreateApiKeyResponse};

/// POST /admin/api-keys — create a new API key for the authenticated admin.
pub(super) async fn create_api_key(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<CreateApiKeyBody>,
) -> Result<(StatusCode, Json<CreateApiKeyResponse>), AppError> {
    auth.require(Permission::ApiKeysCreate).await?;

    // Validate requested permissions are a subset of the user's permissions
    if !auth.is_super {
        for perm_str in &body.permissions {
            #[allow(clippy::collapsible_if)]
            if let Ok(p) =
                serde_json::from_value::<Permission>(serde_json::Value::String(perm_str.clone()))
            {
                if !auth.permissions.contains(&p) {
                    return Err(AppError::Forbidden(format!(
                        "Cannot grant API key permission you don't have: {perm_str}"
                    )));
                }
            }
        }
    }

    // Generate the raw key: "hv_" + 32 random hex chars.
    let mut random_bytes = [0u8; 16];
    rand::rng().fill(&mut random_bytes);
    let raw_key = format!("hv_{}", hex::encode(random_bytes));

    // SHA-256 hash for storage.
    let hash = hex::encode(Sha256::digest(raw_key.as_bytes()));

    // First 8 chars for display (e.g., "hv_a1b2c3d4").
    let key_prefix = raw_key[..11].to_string(); // "hv_" + 8 hex chars

    let id = Uuid::new_v4().to_string();
    let now = now_rfc3339();
    let permissions_json =
        serde_json::to_string(&body.permissions).unwrap_or_else(|_| "[]".to_string());

    let insert_sql = adapt_sql(
        "INSERT INTO happyview_api_keys (id, user_id, name, key_hash, key_prefix, permissions, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        state.db_backend,
    );

    sqlx::query(&insert_sql)
        .bind(&id)
        .bind(&auth.user_id)
        .bind(&body.name)
        .bind(&hash)
        .bind(&key_prefix)
        .bind(&permissions_json)
        .bind(&now)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create api key: {e}")))?;

    log_event(
        &state.db,
        EventLog {
            event_type: "api_key.created".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(body.name.clone()),
            detail: serde_json::json!({ "key_prefix": key_prefix, "permissions": &body.permissions }),
        },
        state.db_backend,
    )
    .await;

    Ok((
        StatusCode::CREATED,
        Json(CreateApiKeyResponse {
            id,
            name: body.name,
            key: raw_key,
            key_prefix,
            permissions: body.permissions,
        }),
    ))
}

/// GET /admin/api-keys — list API keys for the authenticated admin.
pub(super) async fn list_api_keys(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<Vec<ApiKeySummary>>, AppError> {
    auth.require(Permission::ApiKeysRead).await?;

    let select_sql = adapt_sql(
        "SELECT k.id, k.name, k.key_prefix, k.permissions, k.created_at, k.last_used_at, k.revoked_at FROM happyview_api_keys k JOIN happyview_users u ON u.id = k.user_id WHERE u.did = ? ORDER BY k.created_at DESC",
        state.db_backend,
    );

    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(&select_sql)
        .bind(&auth.did)
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list api keys: {e}")))?;

    let keys: Vec<ApiKeySummary> = rows
        .into_iter()
        .map(
            |(id, name, key_prefix, permissions_json, created_at, last_used_at, revoked_at)| {
                let permissions: Vec<String> =
                    serde_json::from_str(&permissions_json).unwrap_or_default();
                ApiKeySummary {
                    id,
                    name,
                    key_prefix,
                    permissions,
                    created_at,
                    last_used_at,
                    revoked_at,
                }
            },
        )
        .collect();

    Ok(Json(keys))
}

/// DELETE /admin/api-keys/:id — revoke an API key (soft delete).
pub(super) async fn revoke_api_key(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::ApiKeysDelete).await?;

    let now = now_rfc3339();
    let update_sql = adapt_sql(
        "UPDATE happyview_api_keys SET revoked_at = ? WHERE id = ? AND user_id = (SELECT id FROM happyview_users WHERE did = ?) AND revoked_at IS NULL",
        state.db_backend,
    );

    let result = sqlx::query(&update_sql)
        .bind(&now)
        .bind(&id)
        .bind(&auth.did)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to revoke api key: {e}")))?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("api key '{id}' not found")));
    }

    log_event(
        &state.db,
        EventLog {
            event_type: "api_key.revoked".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(id.to_string()),
            detail: serde_json::json!({}),
        },
        state.db_backend,
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}
