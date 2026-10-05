//! Endpoints reserved for the managed-hosting platform principal
//! (`PLATFORM_API_KEY_HASH`). Nothing here is reachable by users or their keys.

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};

use super::auth::AllowPlatform;
use super::permissions::Permission;

#[derive(Deserialize)]
pub(super) struct SetSuperUserBody {
    did: String,
}

#[derive(Serialize)]
pub(super) struct SetSuperUserResponse {
    user_id: String,
    did: String,
}

/// PUT /admin/platform/super-user
///
/// Make `did` the instance's only super user, creating the user if needed.
/// Called by the provisioner before an instance is publicly routable, which
/// closes the first-login bootstrap race; also used for ownership transfer.
/// Idempotent.
pub(super) async fn set_super_user(
    State(state): State<AppState>,
    AllowPlatform(auth): AllowPlatform,
    Json(body): Json<SetSuperUserBody>,
) -> Result<Json<SetSuperUserResponse>, AppError> {
    if !auth.is_platform {
        return Err(AppError::Forbidden(
            "only the platform principal can set the super user".into(),
        ));
    }

    let did = body.did.trim().to_string();
    if !did.starts_with("did:") {
        return Err(AppError::BadRequest("did must be a DID".into()));
    }

    let backend = state.db_backend;
    let now = now_rfc3339();

    // One transaction, so a failure part-way through can never leave zero super
    // users or two. It does not guard against concurrent calls (Postgres runs at
    // READ COMMITTED), so callers must not issue them; the platform calls this
    // sequentially.
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| AppError::Internal(format!("failed to begin transaction: {e}")))?;

    let existing: Option<(String,)> = crate::db::query_as(&adapt_sql(
        "SELECT id FROM happyview_users WHERE did = ?",
        backend,
    ))
    .bind(&did)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| AppError::Internal(format!("failed to look up user: {e}")))?;

    let user_id = match existing {
        Some((id,)) => {
            crate::db::query(&adapt_sql(
                "UPDATE happyview_users SET is_super = ? WHERE id = ?",
                backend,
            ))
            .bind(1_i32)
            .bind(&id)
            .execute(&mut *tx)
            .await
            .map_err(|e| AppError::Internal(format!("failed to promote user: {e}")))?;
            id
        }
        None => {
            let id = uuid::Uuid::new_v4().to_string();
            crate::db::query(&adapt_sql(
                "INSERT INTO happyview_users (id, did, is_super, created_at) VALUES (?, ?, ?, ?)",
                backend,
            ))
            .bind(&id)
            .bind(&did)
            .bind(1_i32)
            .bind(&now)
            .execute(&mut *tx)
            .await
            .map_err(|e| AppError::Internal(format!("failed to create user: {e}")))?;
            id
        }
    };

    crate::db::query(&adapt_sql(
        "UPDATE happyview_users SET is_super = ? WHERE id <> ?",
        backend,
    ))
    .bind(0_i32)
    .bind(&user_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| AppError::Internal(format!("failed to demote previous super: {e}")))?;

    let perm_sql = adapt_sql(
        "INSERT INTO happyview_user_permissions (user_id, permission, granted_at) VALUES (?, ?, ?) ON CONFLICT DO NOTHING",
        backend,
    );
    for perm in Permission::all() {
        crate::db::query(&perm_sql)
            .bind(&user_id)
            .bind(perm.as_str())
            .bind(&now)
            .execute(&mut *tx)
            .await
            .map_err(|e| AppError::Internal(format!("failed to grant permission: {e}")))?;
    }

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("failed to commit super-user change: {e}")))?;

    log_event(
        &state.db,
        EventLog {
            event_type: "user.super_set_by_platform".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(did.clone()),
            detail: serde_json::json!({ "user_id": user_id }),
        },
        backend,
    )
    .await;

    Ok(Json(SetSuperUserResponse { user_id, did }))
}
