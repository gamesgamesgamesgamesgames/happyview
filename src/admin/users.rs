use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::Value;
use uuid::Uuid;

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};

use super::auth::UserAuth;
use super::permissions::{self, Permission};
use super::types::{CreateUserBody, TransferSuperBody, UpdatePermissionsBody, UserSummary};

/// POST /admin/users — create a new user with template or explicit permissions.
pub(super) async fn create_user(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<CreateUserBody>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    auth.require(Permission::UsersCreate).await?;

    // Determine permissions to grant
    let perms_to_grant: Vec<String> = if let Some(explicit) = &body.permissions {
        explicit.clone()
    } else if let Some(template) = &body.template {
        template
            .permissions()
            .iter()
            .map(|p| p.as_str().to_string())
            .collect()
    } else {
        // Default to viewer
        super::permissions::Template::Viewer
            .permissions()
            .iter()
            .map(|p| p.as_str().to_string())
            .collect()
    };

    // Escalation guard: actor can only grant permissions they hold
    if !auth.is_super {
        for perm_str in &perms_to_grant {
            #[allow(clippy::collapsible_if)]
            if let Ok(p) =
                serde_json::from_value::<Permission>(serde_json::Value::String(perm_str.clone()))
            {
                if !auth.permissions.contains(&p) {
                    return Err(AppError::Forbidden(format!(
                        "Cannot grant permission you don't have: {perm_str}"
                    )));
                }
            }
        }
    }

    let resolved = crate::identity::resolve_identifier(&body.did).await?;
    let did = resolved.did;

    let user_id = Uuid::new_v4().to_string();
    let now = now_rfc3339();
    let backend = state.db_backend;

    let insert_sql = adapt_sql(
        "INSERT INTO happyview_users (id, did, is_super, created_at) VALUES (?, ?, ?, ?)",
        backend,
    );

    let existing: Option<(String,)> = crate::db::query_as(&adapt_sql(
        "SELECT id FROM happyview_users WHERE did = ?",
        backend,
    ))
    .bind(&did)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("user lookup failed: {e}")))?;

    if existing.is_some() {
        return Err(AppError::Conflict(format!("user '{did}' already exists")));
    }

    crate::db::query(&insert_sql)
        .bind(&user_id)
        .bind(&did)
        .bind(0_i32)
        .bind(&now)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create user: {e}")))?;

    let perm_sql = adapt_sql(
        "INSERT INTO happyview_user_permissions (user_id, permission, granted_by, granted_at) VALUES (?, ?, ?, ?) ON CONFLICT DO NOTHING",
        backend,
    );

    for perm_str in &perms_to_grant {
        crate::db::query(&perm_sql)
            .bind(&user_id)
            .bind(perm_str)
            .bind(&auth.user_id)
            .bind(&now)
            .execute(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("failed to grant permission: {e}")))?;
    }

    let template_name = body
        .template
        .as_ref()
        .map(|t| format!("{t:?}").to_lowercase());

    log_event(
        &state.db,
        EventLog {
            event_type: "user.created".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(did.clone()),
            detail: serde_json::json!({
                "template": template_name,
                "permissions": perms_to_grant,
            }),
        },
        backend,
    )
    .await;

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "id": user_id,
            "did": did,
        })),
    ))
}

/// GET /admin/users — list all users with their permissions.
pub(super) async fn list_users(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<Vec<UserSummary>>, AppError> {
    auth.require(Permission::UsersRead).await?;

    let backend = state.db_backend;

    let select_sql = adapt_sql(
        "SELECT id, did, is_super, created_at, last_used_at FROM happyview_users ORDER BY created_at",
        backend,
    );

    let rows: Vec<(String, String, i32, String, Option<String>)> = crate::db::query_as(&select_sql)
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list users: {e}")))?;

    let perm_sql = adapt_sql(
        "SELECT permission FROM happyview_user_permissions WHERE user_id = ? ORDER BY permission",
        backend,
    );

    let mut users = Vec::new();
    for (id, did, is_super_int, created_at, last_used_at) in rows {
        let perm_rows: Vec<(String,)> = crate::db::query_as(&perm_sql)
            .bind(&id)
            .fetch_all(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("failed to load permissions: {e}")))?;

        users.push(UserSummary {
            id,
            did,
            is_super: is_super_int != 0,
            permissions: perm_rows.into_iter().map(|(p,)| p).collect(),
            created_at,
            last_used_at,
        });
    }

    Ok(Json(users))
}

/// GET /admin/users/:id — get a single user with permissions.
pub(super) async fn get_user(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<Json<UserSummary>, AppError> {
    auth.require(Permission::UsersRead).await?;

    let backend = state.db_backend;

    let select_sql = adapt_sql(
        "SELECT id, did, is_super, created_at, last_used_at FROM happyview_users WHERE id = ?",
        backend,
    );

    let found: Option<(String, String, i32, String, Option<String>)> =
        crate::db::query_as(&select_sql)
            .bind(&id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("failed to get user: {e}")))?;

    let Some((uid, did, is_super_int, created_at, last_used_at)) = found else {
        return Err(AppError::NotFound(format!("user '{id}' not found")));
    };

    let perm_sql = adapt_sql(
        "SELECT permission FROM happyview_user_permissions WHERE user_id = ? ORDER BY permission",
        backend,
    );

    let perm_rows: Vec<(String,)> = crate::db::query_as(&perm_sql)
        .bind(&uid)
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to load permissions: {e}")))?;

    Ok(Json(UserSummary {
        id: uid,
        did,
        is_super: is_super_int != 0,
        permissions: perm_rows.into_iter().map(|(p,)| p).collect(),
        created_at,
        last_used_at,
    }))
}

/// PATCH /admin/users/:id/permissions — grant/revoke permissions.
pub(super) async fn update_permissions(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
    Json(body): Json<UpdatePermissionsBody>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::UsersUpdate).await?;

    let backend = state.db_backend;

    // Self-modification guard
    if auth.user_id == id {
        return Err(AppError::Forbidden(
            "Cannot modify your own permissions".into(),
        ));
    }

    // Cannot modify super user's permissions
    let select_sql = adapt_sql("SELECT is_super FROM happyview_users WHERE id = ?", backend);
    let target: Option<(i32,)> = crate::db::query_as(&select_sql)
        .bind(&id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("user lookup failed: {e}")))?;

    let Some((target_is_super,)) = target else {
        return Err(AppError::NotFound(format!("user '{id}' not found")));
    };

    if target_is_super != 0 {
        return Err(AppError::Forbidden(
            "Cannot modify super user's permissions".into(),
        ));
    }

    // Validate all permission strings are recognized
    for perm_str in body.grant.iter().chain(body.revoke.iter()) {
        if serde_json::from_value::<Permission>(serde_json::Value::String(perm_str.clone()))
            .is_err()
        {
            return Err(AppError::BadRequest(format!(
                "Unrecognized permission: {perm_str}"
            )));
        }
    }

    // Escalation guard: can only grant permissions you hold
    if !auth.is_super {
        for perm_str in &body.grant {
            #[allow(clippy::collapsible_if)]
            if let Ok(p) =
                serde_json::from_value::<Permission>(serde_json::Value::String(perm_str.clone()))
            {
                if !auth.permissions.contains(&p) {
                    return Err(AppError::Forbidden(format!(
                        "Cannot grant permission you don't have: {perm_str}"
                    )));
                }
            }
        }
    }

    let now = now_rfc3339();

    let grant_sql = adapt_sql(
        "INSERT INTO happyview_user_permissions (user_id, permission, granted_by, granted_at) VALUES (?, ?, ?, ?) ON CONFLICT DO NOTHING",
        backend,
    );

    for perm_str in &body.grant {
        crate::db::query(&grant_sql)
            .bind(&id)
            .bind(perm_str)
            .bind(&auth.user_id)
            .bind(&now)
            .execute(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("failed to grant permission: {e}")))?;
    }

    let revoke_sql = adapt_sql(
        "DELETE FROM happyview_user_permissions WHERE user_id = ? AND permission = ?",
        backend,
    );

    for perm_str in &body.revoke {
        crate::db::query(&revoke_sql)
            .bind(&id)
            .bind(perm_str)
            .execute(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("failed to revoke permission: {e}")))?;
    }

    log_event(
        &state.db,
        EventLog {
            event_type: "user.permissions_updated".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(id.clone()),
            detail: serde_json::json!({
                "granted": body.grant,
                "revoked": body.revoke,
            }),
        },
        backend,
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

/// DELETE /admin/users/:id — remove a user.
pub(super) async fn delete_user(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::UsersDelete).await?;

    let backend = state.db_backend;

    // Self-deletion guard
    if auth.user_id == id {
        return Err(AppError::Forbidden("Cannot delete yourself".into()));
    }

    // Cannot delete super user
    let select_sql = adapt_sql("SELECT is_super FROM happyview_users WHERE id = ?", backend);
    let target: Option<(i32,)> = crate::db::query_as(&select_sql)
        .bind(&id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("user lookup failed: {e}")))?;

    let Some((is_super,)) = target else {
        return Err(AppError::NotFound(format!("user '{id}' not found")));
    };

    if is_super != 0 {
        return Err(AppError::Forbidden("Cannot delete the super user".into()));
    }

    // Revoke API keys and delete user
    let now = now_rfc3339();

    let revoke_keys_sql = adapt_sql(
        "UPDATE happyview_api_keys SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL",
        backend,
    );

    crate::db::query(&revoke_keys_sql)
        .bind(&now)
        .bind(&id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to revoke api keys: {e}")))?;

    let delete_sql = adapt_sql("DELETE FROM happyview_users WHERE id = ?", backend);

    let result = crate::db::query(&delete_sql)
        .bind(&id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete user: {e}")))?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("user '{id}' not found")));
    }

    log_event(
        &state.db,
        EventLog {
            event_type: "user.deleted".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(id),
            detail: serde_json::json!({}),
        },
        backend,
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

/// POST /admin/users/transfer-super — transfer super user status.
pub(super) async fn transfer_super(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<TransferSuperBody>,
) -> Result<StatusCode, AppError> {
    if !auth.is_super {
        return Err(AppError::Forbidden(
            "Only the super user can transfer super status".into(),
        ));
    }

    let backend = state.db_backend;
    let now = now_rfc3339();

    // Run every mutation in a single transaction so a failure or crash can never
    // leave the instance with zero super users (M6). On any early return the
    // transaction is dropped and rolled back, preserving the current super.
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| AppError::Internal(format!("failed to begin transaction: {e}")))?;

    // Promote the target first (and require it to exist) so a missing target
    // aborts before the current super is touched.
    let set_super_sql = adapt_sql(
        "UPDATE happyview_users SET is_super = ? WHERE id = ?",
        backend,
    );
    let result = crate::db::query(&set_super_sql)
        .bind(1_i32)
        .bind(&body.target_user_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(format!("failed to set super: {e}")))?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!(
            "user '{}' not found",
            body.target_user_id
        )));
    }

    // Demote the current super — unless they are transferring to themselves, in
    // which case demoting would undo the promotion above.
    if body.target_user_id != auth.user_id {
        let remove_super_sql = adapt_sql(
            "UPDATE happyview_users SET is_super = ? WHERE id = ?",
            backend,
        );
        crate::db::query(&remove_super_sql)
            .bind(0_i32)
            .bind(&auth.user_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| AppError::Internal(format!("failed to remove super: {e}")))?;
    }

    // Ensure target has all permissions
    let perm_sql = adapt_sql(
        "INSERT INTO happyview_user_permissions (user_id, permission, granted_by, granted_at) VALUES (?, ?, ?, ?) ON CONFLICT DO NOTHING",
        backend,
    );

    for perm in Permission::all() {
        crate::db::query(&perm_sql)
            .bind(&body.target_user_id)
            .bind(perm.as_str())
            .bind(&auth.user_id)
            .bind(&now)
            .execute(&mut *tx)
            .await
            .map_err(|e| AppError::Internal(format!("failed to grant permission: {e}")))?;
    }

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("failed to commit super transfer: {e}")))?;

    log_event(
        &state.db,
        EventLog {
            event_type: "user.super_transferred".to_string(),
            severity: Severity::Warn,
            actor_did: Some(auth.did.clone()),
            subject: Some(body.target_user_id.clone()),
            detail: serde_json::json!({
                "from_user_id": auth.user_id,
                "to_user_id": body.target_user_id,
            }),
        },
        backend,
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

/// GET /admin/permissions — list all available permissions and templates.
pub(super) async fn list_permissions(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<Value>, AppError> {
    auth.require(Permission::UsersRead).await?;

    let spaces_enabled = crate::feature_flags::is_enabled(
        &state.db,
        crate::feature_flags::FeatureFlag::SPACES_ENABLED,
        state.db_backend,
    )
    .await;

    let all_permissions: Vec<Value> = permissions::catalog()
        .into_iter()
        .filter(|p| spaces_enabled || p.category != "Spaces")
        .map(|p| {
            serde_json::json!({
                "key": p.key,
                "name": p.name,
                "description": p.description,
                "category": p.category,
            })
        })
        .collect();

    let templates: Vec<Value> = permissions::Template::ALL
        .iter()
        .map(|t| {
            let info = t.info();
            let perms: Vec<&str> = info
                .permissions
                .into_iter()
                .filter(|p| spaces_enabled || !p.starts_with("spaces:"))
                .collect();
            serde_json::json!({
                "key": info.key,
                "label": info.label,
                "permissions": perms,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "permissions": all_permissions,
        "templates": templates,
    })))
}
