use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};

use super::auth::UserAuth;
use super::permissions::Permission;
use super::types::{AddLabelerBody, LabelerSummary, UpdateLabelerBody};

/// GET /admin/labelers — list all labeler subscriptions.
pub(super) async fn list(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<Vec<LabelerSummary>>, AppError> {
    auth.require(Permission::LabelersRead).await?;

    let backend = state.db_backend;
    let sql = adapt_sql(
        "SELECT did, status, cursor, created_at, updated_at FROM happyview_labeler_subscriptions ORDER BY created_at",
        backend,
    );
    let rows: Vec<(String, String, Option<i64>, String, String)> = sqlx::query_as(&sql)
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list labeler subscriptions: {e}")))?;

    let labelers: Vec<LabelerSummary> = rows
        .into_iter()
        .map(
            |(did, status, cursor, created_at, updated_at)| LabelerSummary {
                did,
                status,
                cursor,
                created_at,
                updated_at,
            },
        )
        .collect();

    Ok(Json(labelers))
}

/// POST /admin/labelers — add a labeler subscription.
pub(super) async fn add(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<AddLabelerBody>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::LabelersCreate).await?;

    let backend = state.db_backend;
    let now = now_rfc3339();
    let sql = adapt_sql(
        r#"
        INSERT INTO happyview_labeler_subscriptions (did, created_at)
        VALUES (?, ?)
        ON CONFLICT (did) DO UPDATE SET status = 'active', updated_at = ?
        "#,
        backend,
    );
    sqlx::query(&sql)
        .bind(&body.did)
        .bind(&now)
        .bind(&now)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to add labeler subscription: {e}")))?;

    // Notify the labeler consumer to pick up the new subscription.
    let _ = state.labeler_subscriptions_tx.send(());

    log_event(
        &state.db,
        EventLog {
            event_type: "labeler.added".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(body.did.clone()),
            detail: serde_json::json!({}),
        },
        state.db_backend,
    )
    .await;

    Ok(StatusCode::CREATED)
}

/// PATCH /admin/labelers/{did} — update labeler status (active/paused).
pub(super) async fn update(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(did): Path<String>,
    Json(body): Json<UpdateLabelerBody>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::LabelersCreate).await?;

    let backend = state.db_backend;
    let now = now_rfc3339();
    let sql = adapt_sql(
        "UPDATE happyview_labeler_subscriptions SET status = ?, updated_at = ? WHERE did = ?",
        backend,
    );
    let result = sqlx::query(&sql)
        .bind(&body.status)
        .bind(&now)
        .bind(&did)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to update labeler subscription: {e}")))?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!(
            "labeler subscription '{did}' not found"
        )));
    }

    let _ = state.labeler_subscriptions_tx.send(());

    log_event(
        &state.db,
        EventLog {
            event_type: "labeler.updated".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(did),
            detail: serde_json::json!({ "status": body.status }),
        },
        state.db_backend,
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

/// DELETE /admin/labelers/{did} — remove a labeler subscription and its labels.
pub(super) async fn delete(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(did): Path<String>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::LabelersDelete).await?;

    let backend = state.db_backend;
    let delete_sql = adapt_sql(
        "DELETE FROM happyview_labeler_subscriptions WHERE did = ?",
        backend,
    );
    let result = sqlx::query(&delete_sql)
        .bind(&did)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete labeler subscription: {e}")))?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!(
            "labeler subscription '{did}' not found"
        )));
    }

    // Also remove all labels from this labeler.
    let delete_labels_sql = adapt_sql("DELETE FROM happyview_labels WHERE src = ?", backend);
    let _ = sqlx::query(&delete_labels_sql)
        .bind(&did)
        .execute(&state.db)
        .await;

    let _ = state.labeler_subscriptions_tx.send(());

    log_event(
        &state.db,
        EventLog {
            event_type: "labeler.deleted".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(did),
            detail: serde_json::json!({}),
        },
        state.db_backend,
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}
