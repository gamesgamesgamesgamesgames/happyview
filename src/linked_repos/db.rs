use sqlx::Row;
use uuid::Uuid;

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;

use super::types::{LinkedRepo, STATUS_ACTIVE, STATUS_NEEDS_REAUTH, STATUS_PENDING};

const COLUMNS: &str = "id, did, handle, reason, scopes, status, last_error, \
                       last_refreshed_at, authorized_at, created_by, created_at";

fn row_to_grant(row: &sqlx::any::AnyRow) -> LinkedRepo {
    LinkedRepo {
        id: row.get("id"),
        did: row.get("did"),
        handle: row.get("handle"),
        reason: row.get("reason"),
        scopes: row.get("scopes"),
        status: row.get("status"),
        last_error: row.get("last_error"),
        last_refreshed_at: row.get("last_refreshed_at"),
        authorized_at: row.get("authorized_at"),
        created_by: row.get("created_by"),
        created_at: row.get("created_at"),
    }
}

pub async fn create(
    state: &AppState,
    did: Option<&str>,
    handle: Option<&str>,
    reason: Option<&str>,
    scopes: &str,
    created_by: &str,
) -> Result<LinkedRepo, AppError> {
    let id = Uuid::new_v4().to_string();
    let now = now_rfc3339();

    let sql = adapt_sql(
        "INSERT INTO happyview_linked_repos \
         (id, did, handle, reason, scopes, status, created_by, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        state.db_backend,
    );

    crate::db::query(&sql)
        .bind(&id)
        .bind(did)
        .bind(handle)
        .bind(reason)
        .bind(scopes)
        .bind(STATUS_PENDING)
        .bind(created_by)
        .bind(&now)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create linked repo: {e}")))?;

    get(state, &id)
        .await?
        .ok_or_else(|| AppError::Internal("linked repo vanished after insert".into()))
}

pub async fn list(state: &AppState) -> Result<Vec<LinkedRepo>, AppError> {
    let sql = adapt_sql(
        &format!("SELECT {COLUMNS} FROM happyview_linked_repos ORDER BY created_at DESC"),
        state.db_backend,
    );
    let rows = crate::db::query(&sql)
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list linked repos: {e}")))?;
    Ok(rows.iter().map(row_to_grant).collect())
}

pub async fn get(state: &AppState, id: &str) -> Result<Option<LinkedRepo>, AppError> {
    let sql = adapt_sql(
        &format!("SELECT {COLUMNS} FROM happyview_linked_repos WHERE id = ?"),
        state.db_backend,
    );
    let row = crate::db::query(&sql)
        .bind(id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to load linked repo: {e}")))?;
    Ok(row.as_ref().map(row_to_grant))
}

pub async fn get_by_did(state: &AppState, did: &str) -> Result<Option<LinkedRepo>, AppError> {
    let sql = adapt_sql(
        &format!("SELECT {COLUMNS} FROM happyview_linked_repos WHERE did = ?"),
        state.db_backend,
    );
    let row = crate::db::query(&sql)
        .bind(did)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to load linked repo by did: {e}")))?;
    Ok(row.as_ref().map(row_to_grant))
}

pub async fn delete(state: &AppState, id: &str) -> Result<bool, AppError> {
    let Some(grant) = get(state, id).await? else {
        return Ok(false);
    };

    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| AppError::Internal(format!("failed to begin transaction: {e}")))?;

    if let Some(ref did) = grant.did {
        let sql = adapt_sql(
            "DELETE FROM happyview_linked_repo_sessions WHERE did = ?",
            state.db_backend,
        );
        crate::db::query(&sql)
            .bind(did)
            .execute(&mut *tx)
            .await
            .map_err(|e| AppError::Internal(format!("failed to delete linked session: {e}")))?;
    }

    let state_sql = adapt_sql(
        "DELETE FROM happyview_linked_repo_auth_state \
         WHERE grant_id = ? AND token_hash IS NOT NULL",
        state.db_backend,
    );
    crate::db::query(&state_sql)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete linked auth state: {e}")))?;

    let sql = adapt_sql(
        "DELETE FROM happyview_linked_repos WHERE id = ?",
        state.db_backend,
    );
    let result = crate::db::query(&sql)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete linked repo: {e}")))?;

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("failed to commit transaction: {e}")))?;

    Ok(result.rows_affected() > 0)
}

pub async fn bind_did(
    state: &AppState,
    id: &str,
    did: &str,
    handle: Option<&str>,
) -> Result<(), AppError> {
    let now = now_rfc3339();
    let sql = adapt_sql(
        "UPDATE happyview_linked_repos \
         SET did = ?, handle = COALESCE(?, handle), status = ?, \
             authorized_at = ?, last_refreshed_at = ?, last_error = NULL \
         WHERE id = ?",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(did)
        .bind(handle)
        .bind(STATUS_ACTIVE)
        .bind(&now)
        .bind(&now)
        .bind(id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to bind linked repo did: {e}")))?;
    Ok(())
}

pub async fn mark_needs_reauth(state: &AppState, id: &str, error: &str) -> Result<(), AppError> {
    let sql = adapt_sql(
        "UPDATE happyview_linked_repos SET status = ?, last_error = ? WHERE id = ?",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(STATUS_NEEDS_REAUTH)
        .bind(error)
        .bind(id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to mark needs_reauth: {e}")))?;
    Ok(())
}

pub async fn touch_refreshed(state: &AppState, id: &str) -> Result<(), AppError> {
    let sql = adapt_sql(
        "UPDATE happyview_linked_repos SET last_refreshed_at = ?, last_error = NULL WHERE id = ?",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(now_rfc3339())
        .bind(id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to touch linked repo: {e}")))?;
    Ok(())
}

pub async fn all_scopes(state: &AppState) -> Result<Vec<String>, AppError> {
    let sql = adapt_sql(
        "SELECT scopes FROM happyview_linked_repos",
        state.db_backend,
    );
    let rows: Vec<(String,)> = crate::db::query_as(&sql)
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to load linked repo scopes: {e}")))?;
    Ok(rows.into_iter().map(|(s,)| s).collect())
}
