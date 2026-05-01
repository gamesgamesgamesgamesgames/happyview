use axum::{
    Json,
    extract::{Path, Query, State},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::auth::UserAuth;
use super::permissions::Permission;
use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339, parse_dt};
use crate::error::AppError;
use crate::lua::{resolve_record_event, run_record_event_once};
use crate::record_handler::RecordEvent;

// ---------------------------------------------------------------------------
// Query / request / response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ListQuery {
    pub collection: Option<String>,
    pub resolved: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct CountQuery {
    pub resolved: Option<String>,
}

#[derive(Serialize)]
pub struct DeadLetterSummary {
    pub id: String,
    pub lexicon_id: String,
    pub uri: String,
    pub did: String,
    pub collection: String,
    pub rkey: String,
    pub action: String,
    pub error: String,
    pub attempts: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Serialize)]
pub struct DeadLetterDetail {
    #[serde(flatten)]
    pub summary: DeadLetterSummary,
    pub record: Option<Value>,
}

#[derive(Serialize)]
pub struct ListResponse {
    pub dead_letters: Vec<DeadLetterSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Serialize)]
pub struct CountResponse {
    pub count: i64,
}

#[derive(Deserialize)]
pub struct BulkRequest {
    pub ids: Option<Vec<String>>,
    pub all: Option<bool>,
    pub collection: Option<String>,
}

/// Internal row type for fetching action data needed by retry/reindex.
#[allow(dead_code)]
struct DeadLetterRow {
    id: String,
    lexicon_id: String,
    uri: String,
    did: String,
    collection: String,
    rkey: String,
    action: String,
    record: Option<String>,
    error: String,
    attempts: i64,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /admin/dead-letters
pub(super) async fn list(
    auth: UserAuth,
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<ListResponse>, AppError> {
    auth.require(Permission::DeadLettersRead).await?;
    let backend = state.db_backend;
    let limit = query.limit.unwrap_or(50).clamp(1, 100);

    let mut sql = String::from(
        "SELECT id, lexicon_id, uri, did, collection, rkey, action, error, attempts, created_at, resolved_at
         FROM dead_letter_hooks WHERE 1=1",
    );

    let resolved_filter = query.resolved.as_deref().unwrap_or("false");
    match resolved_filter {
        "false" => sql.push_str(" AND resolved_at IS NULL"),
        "true" => sql.push_str(" AND resolved_at IS NOT NULL"),
        _ => {} // no filter
    }

    if query.collection.is_some() {
        sql.push_str(" AND collection = ?");
    }
    if query.cursor.is_some() {
        sql.push_str(" AND created_at < ?");
    }

    sql.push_str(" ORDER BY created_at DESC LIMIT ?");

    let sql = adapt_sql(&sql, backend);

    #[allow(clippy::type_complexity)]
    let mut q = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            i64,
            String,
            Option<String>,
        ),
    >(&sql);

    if let Some(ref collection) = query.collection {
        q = q.bind(collection);
    }
    if let Some(ref cursor) = query.cursor {
        q = q.bind(cursor);
    }
    q = q.bind(limit);

    let rows = q
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to query dead letters: {e}")))?;

    let dead_letters: Vec<DeadLetterSummary> = rows
        .into_iter()
        .map(|row| DeadLetterSummary {
            id: row.0,
            lexicon_id: row.1,
            uri: row.2,
            did: row.3,
            collection: row.4,
            rkey: row.5,
            action: row.6,
            error: row.7,
            attempts: row.8,
            created_at: parse_dt(&row.9),
            resolved_at: row.10.as_deref().map(parse_dt),
        })
        .collect();

    let cursor = if dead_letters.len() as i64 >= limit {
        dead_letters.last().map(|dl| dl.created_at.to_rfc3339())
    } else {
        None
    };

    Ok(Json(ListResponse {
        dead_letters,
        cursor,
    }))
}

/// GET /admin/dead-letters/count
pub(super) async fn count(
    auth: UserAuth,
    State(state): State<AppState>,
    Query(query): Query<CountQuery>,
) -> Result<Json<CountResponse>, AppError> {
    auth.require(Permission::DeadLettersRead).await?;
    let backend = state.db_backend;

    let mut sql = String::from("SELECT COUNT(*) FROM dead_letter_hooks WHERE 1=1");

    let resolved_filter = query.resolved.as_deref().unwrap_or("false");
    match resolved_filter {
        "false" => sql.push_str(" AND resolved_at IS NULL"),
        "true" => sql.push_str(" AND resolved_at IS NOT NULL"),
        _ => {}
    }

    let sql = adapt_sql(&sql, backend);
    let (count,): (i64,) = sqlx::query_as(&sql)
        .fetch_one(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to count dead letters: {e}")))?;

    Ok(Json(CountResponse { count }))
}

/// GET /admin/dead-letters/{id}
pub(super) async fn detail(
    auth: UserAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<DeadLetterDetail>, AppError> {
    auth.require(Permission::DeadLettersRead).await?;
    let backend = state.db_backend;

    let sql = adapt_sql(
        "SELECT id, lexicon_id, uri, did, collection, rkey, action, error, attempts, created_at, resolved_at, record
         FROM dead_letter_hooks WHERE id = ?",
        backend,
    );

    #[allow(clippy::type_complexity)]
    let row: (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        i64,
        String,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(&sql)
        .bind(&id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to fetch dead letter: {e}")))?
        .ok_or_else(|| AppError::NotFound(format!("dead letter {id} not found")))?;

    let summary = DeadLetterSummary {
        id: row.0,
        lexicon_id: row.1,
        uri: row.2,
        did: row.3,
        collection: row.4,
        rkey: row.5,
        action: row.6,
        error: row.7,
        attempts: row.8,
        created_at: parse_dt(&row.9),
        resolved_at: row.10.as_deref().map(parse_dt),
    };

    let record = row.11.as_deref().and_then(|r| serde_json::from_str(r).ok());

    Ok(Json(DeadLetterDetail { summary, record }))
}

/// POST /admin/dead-letters/{id}/dismiss
pub(super) async fn dismiss(
    auth: UserAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    auth.require(Permission::DeadLettersManage).await?;
    let dl = fetch_dead_letter_for_action(&state, &id).await?;
    if dl.id.is_empty() {
        return Err(AppError::NotFound(format!("dead letter {id} not found")));
    }
    mark_resolved(&state, &id).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// POST /admin/dead-letters/{id}/retry
pub(super) async fn retry(
    auth: UserAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    auth.require(Permission::DeadLettersManage).await?;
    retry_single(&state, &id).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// POST /admin/dead-letters/{id}/reindex
pub(super) async fn reindex(
    auth: UserAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    auth.require(Permission::DeadLettersManage).await?;
    reindex_single(&state, &id).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// POST /admin/dead-letters/bulk/dismiss
pub(super) async fn bulk_dismiss(
    auth: UserAuth,
    State(state): State<AppState>,
    Json(body): Json<BulkRequest>,
) -> Result<Json<Value>, AppError> {
    auth.require(Permission::DeadLettersManage).await?;
    let backend = state.db_backend;
    let now = now_rfc3339();

    if body.all == Some(true) {
        let mut sql =
            String::from("UPDATE dead_letter_hooks SET resolved_at = ? WHERE resolved_at IS NULL");
        if body.collection.is_some() {
            sql.push_str(" AND collection = ?");
        }
        let sql = adapt_sql(&sql, backend);
        let mut q = sqlx::query(&sql).bind(&now);
        if let Some(ref collection) = body.collection {
            q = q.bind(collection);
        }
        q.execute(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("bulk dismiss failed: {e}")))?;
    } else if let Some(ref ids) = body.ids {
        for id in ids {
            let sql = adapt_sql(
                "UPDATE dead_letter_hooks SET resolved_at = ? WHERE id = ? AND resolved_at IS NULL",
                backend,
            );
            sqlx::query(&sql)
                .bind(&now)
                .bind(id)
                .execute(&state.db)
                .await
                .map_err(|e| AppError::Internal(format!("bulk dismiss failed for {id}: {e}")))?;
        }
    } else {
        return Err(AppError::BadRequest(
            "must provide 'ids' or 'all: true'".into(),
        ));
    }

    Ok(Json(serde_json::json!({ "ok": true })))
}

/// POST /admin/dead-letters/bulk/retry
pub(super) async fn bulk_retry(
    auth: UserAuth,
    State(state): State<AppState>,
    Json(body): Json<BulkRequest>,
) -> Result<Json<Value>, AppError> {
    auth.require(Permission::DeadLettersManage).await?;
    let ids = resolve_bulk_ids(&state, &body).await?;
    for id in &ids {
        retry_single(&state, id).await?;
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// POST /admin/dead-letters/bulk/reindex
pub(super) async fn bulk_reindex(
    auth: UserAuth,
    State(state): State<AppState>,
    Json(body): Json<BulkRequest>,
) -> Result<Json<Value>, AppError> {
    auth.require(Permission::DeadLettersManage).await?;
    let ids = resolve_bulk_ids(&state, &body).await?;
    for id in &ids {
        reindex_single(&state, id).await?;
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Fetch an unresolved dead letter by ID, returning an error if not found or already resolved.
async fn fetch_dead_letter_for_action(
    state: &AppState,
    id: &str,
) -> Result<DeadLetterRow, AppError> {
    let backend = state.db_backend;
    let sql = adapt_sql(
        "SELECT id, lexicon_id, uri, did, collection, rkey, action, record, error, attempts
         FROM dead_letter_hooks WHERE id = ? AND resolved_at IS NULL",
        backend,
    );

    let row: (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        String,
        i64,
    ) = sqlx::query_as(&sql)
        .bind(id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to fetch dead letter: {e}")))?
        .ok_or_else(|| {
            AppError::NotFound(format!("dead letter {id} not found or already resolved"))
        })?;

    Ok(DeadLetterRow {
        id: row.0,
        lexicon_id: row.1,
        uri: row.2,
        did: row.3,
        collection: row.4,
        rkey: row.5,
        action: row.6,
        record: row.7,
        error: row.8,
        attempts: row.9,
    })
}

/// Mark a dead letter as resolved.
async fn mark_resolved(state: &AppState, id: &str) -> Result<(), AppError> {
    let backend = state.db_backend;
    let now = now_rfc3339();
    let sql = adapt_sql(
        "UPDATE dead_letter_hooks SET resolved_at = ? WHERE id = ?",
        backend,
    );
    sqlx::query(&sql)
        .bind(&now)
        .bind(id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to mark dead letter resolved: {e}")))?;
    Ok(())
}

/// Update the error message and increment attempts.
async fn update_error(state: &AppState, id: &str, error: &str) -> Result<(), AppError> {
    let backend = state.db_backend;
    let sql = adapt_sql(
        "UPDATE dead_letter_hooks SET error = ?, attempts = attempts + 1 WHERE id = ?",
        backend,
    );
    sqlx::query(&sql)
        .bind(error)
        .bind(id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to update dead letter error: {e}")))?;
    Ok(())
}

/// Resolve a BulkRequest into a list of dead letter IDs.
async fn resolve_bulk_ids(state: &AppState, body: &BulkRequest) -> Result<Vec<String>, AppError> {
    if body.all == Some(true) {
        let backend = state.db_backend;
        let mut sql = String::from("SELECT id FROM dead_letter_hooks WHERE resolved_at IS NULL");
        if body.collection.is_some() {
            sql.push_str(" AND collection = ?");
        }
        let sql = adapt_sql(&sql, backend);
        let mut q = sqlx::query_as::<_, (String,)>(&sql);
        if let Some(ref collection) = body.collection {
            q = q.bind(collection);
        }
        let rows = q
            .fetch_all(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("failed to resolve bulk ids: {e}")))?;
        Ok(rows.into_iter().map(|r| r.0).collect())
    } else if let Some(ref ids) = body.ids {
        Ok(ids.clone())
    } else {
        Err(AppError::BadRequest(
            "must provide 'ids' or 'all: true'".into(),
        ))
    }
}

/// Retry a single dead letter by re-running its trigger-keyed script.
///
/// Resolves the script via the new dispatcher's cascade
/// (`record.<action>:<nsid>` → `record.index:<nsid>`). If no script is
/// bound for the cascade now, returns 404 — the operator either deleted
/// the script or never re-bound it under the new naming.
async fn retry_single(state: &AppState, id: &str) -> Result<(), AppError> {
    let dl = fetch_dead_letter_for_action(state, id).await?;

    let resolved = resolve_record_event(state, &dl.collection, &dl.action)
        .await
        .ok_or_else(|| {
            AppError::NotFound(format!(
                "no script bound for record.{}:{} (or record.index:{})",
                dl.action, dl.collection, dl.collection
            ))
        })?;

    let record: Option<Value> = dl
        .record
        .as_deref()
        .and_then(|r| serde_json::from_str(r).ok());

    match run_record_event_once(
        state,
        &resolved,
        &dl.action,
        &dl.uri,
        &dl.did,
        &dl.collection,
        &dl.rkey,
        record.as_ref(),
    )
    .await
    {
        Ok(_) => {
            mark_resolved(state, id).await?;
            Ok(())
        }
        Err(e) => {
            update_error(state, id, &e).await?;
            Err(AppError::Internal(format!(
                "retry failed for dead letter {id}: {e}"
            )))
        }
    }
}

/// Reindex a single dead letter by fetching the record fresh from the PDS.
async fn reindex_single(state: &AppState, id: &str) -> Result<(), AppError> {
    let dl = fetch_dead_letter_for_action(state, id).await?;

    let pds_endpoint =
        crate::profile::resolve_pds_endpoint(&state.http, &state.config.plc_url, &dl.did).await?;

    let url = format!(
        "{}/xrpc/com.atproto.repo.getRecord?repo={}&collection={}&rkey={}",
        pds_endpoint, dl.did, dl.collection, dl.rkey
    );

    let resp = state
        .http
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("failed to fetch record from PDS: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_else(|_| "unknown error".into());
        return Err(AppError::Internal(format!(
            "PDS returned {status} fetching record: {body}"
        )));
    }

    let body: Value = resp
        .json()
        .await
        .map_err(|e| AppError::Internal(format!("failed to parse PDS response: {e}")))?;

    let record = body.get("value").cloned();
    let cid = body
        .get("cid")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let event = RecordEvent {
        did: dl.did.clone(),
        collection: dl.collection.clone(),
        rkey: dl.rkey.clone(),
        action: dl.action.clone(),
        record,
        cid,
    };

    crate::record_handler::handle_record_event(state, &event).await;
    mark_resolved(state, id).await?;

    Ok(())
}
