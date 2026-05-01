//! Admin surface for dead-lettered events.
//!
//! Two tables back this:
//!
//! - **`dead_letter_hooks`** (legacy) — written by the pre-trigger-keyed
//!   indexer when a hook script exhausted retries. Columns are
//!   per-event-field (lexicon_id, uri, did, collection, rkey, action,
//!   record). UUID / TEXT primary keys.
//! - **`dead_letter_scripts`** (current) — written by the trigger-keyed
//!   dispatcher in `crate::lua::scripts`. The event-specific fields are
//!   inside `payload` (JSON). INTEGER primary keys. Carries both record
//!   and label dead letters via the `host_kind` discriminator.
//!
//! Both tables are kept readable + manageable through this admin
//! surface. Per-id operations route by id format: an id that parses as
//! an integer routes to `dead_letter_scripts`, anything else (UUIDs,
//! sqlite NULL-stringified primary keys) routes to `dead_letter_hooks`.
//! The two id namespaces are disjoint so this dispatch is unambiguous.

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
use crate::lua::{RecordEventPayload, resolve_record_event, run_record_event_once};
use crate::record_handler::RecordEvent;

// ---------------------------------------------------------------------------
// Source enum — which table backs a given dead-letter id
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeadLetterSource {
    /// Pre-trigger-keyed legacy hooks table.
    LegacyHooks,
    /// New trigger-keyed scripts table.
    Scripts,
}

impl DeadLetterSource {
    /// Pick the table by id format. Integer-parseable → Scripts;
    /// anything else → LegacyHooks.
    fn from_id(id: &str) -> Self {
        if id.parse::<i64>().is_ok() {
            Self::Scripts
        } else {
            Self::LegacyHooks
        }
    }

    fn table(self) -> &'static str {
        match self {
            Self::LegacyHooks => "dead_letter_hooks",
            Self::Scripts => "dead_letter_scripts",
        }
    }
}

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
/// Populated from either table by `fetch_dead_letter_for_action`.
struct DeadLetterRow {
    id: String,
    source: DeadLetterSource,
    /// Discriminator for new-table rows: `"record"` or `"label"`.
    /// Always `"record"` for legacy rows. Retries are only supported
    /// for record dead letters.
    host_kind: String,
    uri: String,
    did: String,
    collection: String,
    rkey: String,
    action: String,
    record: Option<String>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /admin/dead-letters` — list rows from both tables, merge by
/// created_at, paginate via cursor.
pub(super) async fn list(
    auth: UserAuth,
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<ListResponse>, AppError> {
    auth.require(Permission::DeadLettersRead).await?;
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let resolved = query.resolved.as_deref().unwrap_or("false");

    // Fetch up to `limit` rows from each table, then merge + slice.
    // Two queries instead of a SQL UNION because the schemas differ
    // (legacy has columns; scripts has payload JSON we parse in Rust).
    let mut rows = list_legacy(
        &state,
        resolved,
        query.collection.as_deref(),
        &query.cursor,
        limit,
    )
    .await?;
    rows.extend(
        list_scripts(
            &state,
            resolved,
            query.collection.as_deref(),
            &query.cursor,
            limit,
        )
        .await?,
    );

    // Newest first, then truncate.
    rows.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let truncated = rows.len() as i64 > limit;
    rows.truncate(limit as usize);

    let cursor = if truncated {
        rows.last().map(|r| r.created_at.to_rfc3339())
    } else {
        None
    };

    Ok(Json(ListResponse {
        dead_letters: rows,
        cursor,
    }))
}

/// `GET /admin/dead-letters/count` — sum of unresolved across both tables.
pub(super) async fn count(
    auth: UserAuth,
    State(state): State<AppState>,
    Query(query): Query<CountQuery>,
) -> Result<Json<CountResponse>, AppError> {
    auth.require(Permission::DeadLettersRead).await?;
    let backend = state.db_backend;
    let resolved = query.resolved.as_deref().unwrap_or("false");
    let resolved_clause = match resolved {
        "false" => " AND resolved_at IS NULL",
        "true" => " AND resolved_at IS NOT NULL",
        _ => "",
    };

    let mut total: i64 = 0;
    for table in [
        DeadLetterSource::LegacyHooks.table(),
        DeadLetterSource::Scripts.table(),
    ] {
        let sql = adapt_sql(
            &format!("SELECT COUNT(*) FROM {table} WHERE 1=1{resolved_clause}"),
            backend,
        );
        let (n,): (i64,) = sqlx::query_as(&sql)
            .fetch_one(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("failed to count dead letters: {e}")))?;
        total += n;
    }

    Ok(Json(CountResponse { count: total }))
}

/// `GET /admin/dead-letters/{id}` — detail view. Routes by id format.
pub(super) async fn detail(
    auth: UserAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<DeadLetterDetail>, AppError> {
    auth.require(Permission::DeadLettersRead).await?;
    match DeadLetterSource::from_id(&id) {
        DeadLetterSource::LegacyHooks => detail_legacy(&state, &id).await.map(Json),
        DeadLetterSource::Scripts => detail_scripts(&state, &id).await.map(Json),
    }
}

/// `POST /admin/dead-letters/{id}/dismiss`
pub(super) async fn dismiss(
    auth: UserAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    auth.require(Permission::DeadLettersManage).await?;
    let source = DeadLetterSource::from_id(&id);
    mark_resolved(&state, &id, source).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /admin/dead-letters/{id}/retry`
pub(super) async fn retry(
    auth: UserAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    auth.require(Permission::DeadLettersManage).await?;
    retry_single(&state, &id).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /admin/dead-letters/{id}/reindex`
pub(super) async fn reindex(
    auth: UserAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    auth.require(Permission::DeadLettersManage).await?;
    reindex_single(&state, &id).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /admin/dead-letters/bulk/dismiss`
pub(super) async fn bulk_dismiss(
    auth: UserAuth,
    State(state): State<AppState>,
    Json(body): Json<BulkRequest>,
) -> Result<Json<Value>, AppError> {
    auth.require(Permission::DeadLettersManage).await?;
    let backend = state.db_backend;
    let now = now_rfc3339();

    if body.all == Some(true) {
        // Operate against both tables. Collection filter only applies
        // to legacy rows (the new `dead_letter_scripts` doesn't have a
        // collection column — to filter by collection there we'd need
        // to JSON-parse `payload`, which is portable-SQL pain. Good
        // enough: legacy table is the one with bulk-by-collection
        // history anyway.).
        for source in [DeadLetterSource::LegacyHooks, DeadLetterSource::Scripts] {
            let table = source.table();
            let mut sql = format!("UPDATE {table} SET resolved_at = ? WHERE resolved_at IS NULL");
            let collection_filter =
                source == DeadLetterSource::LegacyHooks && body.collection.is_some();
            if collection_filter {
                sql.push_str(" AND collection = ?");
            }
            let sql = adapt_sql(&sql, backend);
            let mut q = sqlx::query(&sql).bind(&now);
            if collection_filter && let Some(ref c) = body.collection {
                q = q.bind(c);
            }
            q.execute(&state.db)
                .await
                .map_err(|e| AppError::Internal(format!("bulk dismiss failed: {e}")))?;
        }
    } else if let Some(ref ids) = body.ids {
        for id in ids {
            mark_resolved(&state, id, DeadLetterSource::from_id(id)).await?;
        }
    } else {
        return Err(AppError::BadRequest(
            "must provide 'ids' or 'all: true'".into(),
        ));
    }

    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /admin/dead-letters/bulk/retry`
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

/// `POST /admin/dead-letters/bulk/reindex`
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
// Per-table list / detail
// ---------------------------------------------------------------------------

async fn list_legacy(
    state: &AppState,
    resolved: &str,
    collection: Option<&str>,
    cursor: &Option<String>,
    limit: i64,
) -> Result<Vec<DeadLetterSummary>, AppError> {
    let backend = state.db_backend;
    let mut sql = String::from(
        "SELECT id, lexicon_id, uri, did, collection, rkey, action, error, attempts, created_at, resolved_at
         FROM dead_letter_hooks WHERE 1=1",
    );
    match resolved {
        "false" => sql.push_str(" AND resolved_at IS NULL"),
        "true" => sql.push_str(" AND resolved_at IS NOT NULL"),
        _ => {}
    }
    if collection.is_some() {
        sql.push_str(" AND collection = ?");
    }
    if cursor.is_some() {
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
    if let Some(c) = collection {
        q = q.bind(c);
    }
    if let Some(cur) = cursor {
        q = q.bind(cur);
    }
    q = q.bind(limit);

    let rows = q
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to query legacy dead letters: {e}")))?;

    Ok(rows
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
        .collect())
}

async fn list_scripts(
    state: &AppState,
    resolved: &str,
    collection: Option<&str>,
    cursor: &Option<String>,
    limit: i64,
) -> Result<Vec<DeadLetterSummary>, AppError> {
    let backend = state.db_backend;
    let mut sql = String::from(
        "SELECT id, script_ref, host_kind, host_id, payload, error, attempts, created_at, resolved_at
         FROM dead_letter_scripts WHERE 1=1",
    );
    match resolved {
        "false" => sql.push_str(" AND resolved_at IS NULL"),
        "true" => sql.push_str(" AND resolved_at IS NOT NULL"),
        _ => {}
    }
    if cursor.is_some() {
        sql.push_str(" AND created_at < ?");
    }
    sql.push_str(" ORDER BY created_at DESC LIMIT ?");

    let sql = adapt_sql(&sql, backend);
    #[allow(clippy::type_complexity)]
    let mut q = sqlx::query_as::<
        _,
        (
            i64,
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
    if let Some(cur) = cursor {
        q = q.bind(cur);
    }
    q = q.bind(limit);

    let rows = q
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to query scripts dead letters: {e}")))?;

    let summaries: Vec<DeadLetterSummary> = rows
        .into_iter()
        .map(|row| {
            summary_from_scripts_row(
                &row.0.to_string(),
                &row.1,
                &row.2,
                &row.3,
                &row.4,
                &row.5,
                row.6,
                &row.7,
                row.8.as_deref(),
            )
        })
        .collect();

    // Filter by collection in Rust since the column lives inside
    // `payload`. Negligible cost — already client-side after the
    // unfiltered DB fetch.
    Ok(if let Some(want) = collection {
        summaries
            .into_iter()
            .filter(|s| s.collection == want)
            .collect()
    } else {
        summaries
    })
}

#[allow(clippy::too_many_arguments)]
fn summary_from_scripts_row(
    id: &str,
    script_ref: &str,
    host_kind: &str,
    host_id: &str,
    payload: &str,
    error: &str,
    attempts: i64,
    created_at: &str,
    resolved_at: Option<&str>,
) -> DeadLetterSummary {
    let payload_v: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
    let s = |key: &str| {
        payload_v
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };

    // Map the trigger-keyed payload onto the legacy DeadLetterSummary
    // shape. Record-event dead letters fit cleanly. Label-arrival dead
    // letters reuse the slots: did = labeler DID (host_id), action =
    // "label", rkey = label.val, collection extracted from the trigger
    // suffix when possible.
    let (lexicon_id, collection, did, rkey, action) = match host_kind {
        "label" => {
            let collection_from_trigger = script_ref
                .split_once(':')
                .map(|(_, suf)| suf.to_string())
                .unwrap_or_default();
            (
                script_ref.to_string(),
                collection_from_trigger,
                host_id.to_string(),
                s("val"),
                "label".to_string(),
            )
        }
        _ => (
            s("collection"),
            s("collection"),
            s("did"),
            s("rkey"),
            s("action"),
        ),
    };

    DeadLetterSummary {
        id: id.to_string(),
        lexicon_id,
        uri: s("uri"),
        did,
        collection,
        rkey,
        action,
        error: error.to_string(),
        attempts,
        created_at: parse_dt(created_at),
        resolved_at: resolved_at.map(parse_dt),
    }
}

async fn detail_legacy(state: &AppState, id: &str) -> Result<DeadLetterDetail, AppError> {
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
        .bind(id)
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
    Ok(DeadLetterDetail { summary, record })
}

async fn detail_scripts(state: &AppState, id: &str) -> Result<DeadLetterDetail, AppError> {
    let backend = state.db_backend;
    let sql = adapt_sql(
        "SELECT id, script_ref, host_kind, host_id, payload, error, attempts, created_at, resolved_at
         FROM dead_letter_scripts WHERE id = ?",
        backend,
    );
    let id_int: i64 = id
        .parse()
        .map_err(|_| AppError::NotFound(format!("dead letter {id} not found")))?;

    #[allow(clippy::type_complexity)]
    let row: (
        i64,
        String,
        String,
        String,
        String,
        String,
        i64,
        String,
        Option<String>,
    ) = sqlx::query_as(&sql)
        .bind(id_int)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to fetch dead letter: {e}")))?
        .ok_or_else(|| AppError::NotFound(format!("dead letter {id} not found")))?;

    let summary = summary_from_scripts_row(
        &row.0.to_string(),
        &row.1,
        &row.2,
        &row.3,
        &row.4,
        &row.5,
        row.6,
        &row.7,
        row.8.as_deref(),
    );

    let payload_v: Value = serde_json::from_str(&row.4).unwrap_or(Value::Null);
    // For record events the original record body lives at `payload.record`.
    // For label events the entire payload is the event; surface it whole.
    let record = if row.2 == "label" {
        Some(payload_v.clone())
    } else {
        payload_v.get("record").cloned()
    };

    Ok(DeadLetterDetail { summary, record })
}

// ---------------------------------------------------------------------------
// Per-row helpers (retry, reindex, mark resolved, update error)
// ---------------------------------------------------------------------------

/// Fetch an unresolved dead letter from whichever table holds it.
async fn fetch_dead_letter_for_action(
    state: &AppState,
    id: &str,
) -> Result<DeadLetterRow, AppError> {
    let source = DeadLetterSource::from_id(id);
    match source {
        DeadLetterSource::LegacyHooks => {
            let backend = state.db_backend;
            let sql = adapt_sql(
                "SELECT id, lexicon_id, uri, did, collection, rkey, action, record, error, attempts
                 FROM dead_letter_hooks WHERE id = ? AND resolved_at IS NULL",
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
                source,
                host_kind: "record".to_string(),
                uri: row.2,
                did: row.3,
                collection: row.4,
                rkey: row.5,
                action: row.6,
                record: row.7,
            })
        }
        DeadLetterSource::Scripts => {
            let backend = state.db_backend;
            let id_int: i64 = id.parse().unwrap_or_default();
            let sql = adapt_sql(
                "SELECT id, host_kind, payload FROM dead_letter_scripts
                 WHERE id = ? AND resolved_at IS NULL",
                backend,
            );
            let row: (i64, String, String) = sqlx::query_as(&sql)
                .bind(id_int)
                .fetch_optional(&state.db)
                .await
                .map_err(|e| AppError::Internal(format!("failed to fetch dead letter: {e}")))?
                .ok_or_else(|| {
                    AppError::NotFound(format!("dead letter {id} not found or already resolved"))
                })?;
            let payload_v: Value = serde_json::from_str(&row.2).unwrap_or(Value::Null);
            let s = |k: &str| {
                payload_v
                    .get(k)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            // The record body is nested under `payload.record` for
            // record events; serialize it back to a string for the
            // retry call site.
            let record = payload_v
                .get("record")
                .filter(|v| !v.is_null())
                .map(|v| v.to_string());
            Ok(DeadLetterRow {
                id: row.0.to_string(),
                source,
                host_kind: row.1.clone(),
                uri: s("uri"),
                did: s("did"),
                collection: s("collection"),
                rkey: s("rkey"),
                action: s("action"),
                record,
            })
        }
    }
}

async fn mark_resolved(
    state: &AppState,
    id: &str,
    source: DeadLetterSource,
) -> Result<(), AppError> {
    let backend = state.db_backend;
    let now = now_rfc3339();
    let table = source.table();
    let sql = adapt_sql(
        &format!("UPDATE {table} SET resolved_at = ? WHERE id = ?"),
        backend,
    );
    let q = sqlx::query(&sql).bind(&now);
    let q = match source {
        // Scripts table has INTEGER ids; bind as i64 to avoid sqlite's
        // implicit-conversion quirks.
        DeadLetterSource::Scripts => q.bind(id.parse::<i64>().unwrap_or(0)),
        DeadLetterSource::LegacyHooks => q.bind(id),
    };
    q.execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to mark dead letter resolved: {e}")))?;
    Ok(())
}

async fn update_error(
    state: &AppState,
    id: &str,
    source: DeadLetterSource,
    error: &str,
) -> Result<(), AppError> {
    let backend = state.db_backend;
    let table = source.table();
    let sql = adapt_sql(
        &format!("UPDATE {table} SET error = ?, attempts = attempts + 1 WHERE id = ?"),
        backend,
    );
    let q = sqlx::query(&sql).bind(error);
    let q = match source {
        DeadLetterSource::Scripts => q.bind(id.parse::<i64>().unwrap_or(0)),
        DeadLetterSource::LegacyHooks => q.bind(id),
    };
    q.execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to update dead letter error: {e}")))?;
    Ok(())
}

async fn resolve_bulk_ids(state: &AppState, body: &BulkRequest) -> Result<Vec<String>, AppError> {
    if body.all == Some(true) {
        let backend = state.db_backend;
        let mut ids: Vec<String> = Vec::new();

        // Legacy table — supports the optional collection filter.
        let mut sql = String::from("SELECT id FROM dead_letter_hooks WHERE resolved_at IS NULL");
        if body.collection.is_some() {
            sql.push_str(" AND collection = ?");
        }
        let sql = adapt_sql(&sql, backend);
        let mut q = sqlx::query_as::<_, (String,)>(&sql);
        if let Some(ref c) = body.collection {
            q = q.bind(c);
        }
        let rows = q
            .fetch_all(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("failed to resolve bulk ids: {e}")))?;
        ids.extend(rows.into_iter().map(|r| r.0));

        // New table — collection isn't a column; if a filter was
        // requested, only include rows whose payload.collection matches.
        // Simpler to just include all unresolved when no collection
        // filter is set, and skip the new table entirely when one is
        // (the legacy table is the one historically pinned to a
        // collection anyway).
        if body.collection.is_none() {
            let sql = adapt_sql(
                "SELECT id FROM dead_letter_scripts WHERE resolved_at IS NULL",
                backend,
            );
            let rows = sqlx::query_as::<_, (i64,)>(&sql)
                .fetch_all(&state.db)
                .await
                .map_err(|e| AppError::Internal(format!("failed to resolve bulk ids: {e}")))?;
            ids.extend(rows.into_iter().map(|r| r.0.to_string()));
        }

        Ok(ids)
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
/// (`record.<action>:<nsid>` → `record.index:<nsid>`). Label-arrival
/// dead letters are not retried (the upstream label is gone; there's
/// nothing to feed back into the runner). Caller gets a 400.
async fn retry_single(state: &AppState, id: &str) -> Result<(), AppError> {
    let dl = fetch_dead_letter_for_action(state, id).await?;

    if dl.host_kind == "label" {
        return Err(AppError::BadRequest(
            "label-arrival dead letters can't be retried — the upstream label \
             event is gone. Dismiss this row and let the labeler subscription \
             redeliver if needed."
                .into(),
        ));
    }

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
        RecordEventPayload {
            nsid: &dl.collection,
            action: &dl.action,
            uri: &dl.uri,
            did: &dl.did,
            rkey: &dl.rkey,
            record: record.as_ref(),
        },
    )
    .await
    {
        Ok(_) => {
            mark_resolved(state, &dl.id, dl.source).await?;
            Ok(())
        }
        Err(e) => {
            update_error(state, &dl.id, dl.source, &e).await?;
            Err(AppError::Internal(format!(
                "retry failed for dead letter {id}: {e}"
            )))
        }
    }
}

/// Reindex by fetching the record fresh from the PDS. Only applies to
/// record-event dead letters.
async fn reindex_single(state: &AppState, id: &str) -> Result<(), AppError> {
    let dl = fetch_dead_letter_for_action(state, id).await?;

    if dl.host_kind == "label" {
        return Err(AppError::BadRequest(
            "label-arrival dead letters can't be reindexed — they don't have \
             a record to fetch."
                .into(),
        ));
    }

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
    mark_resolved(state, &dl.id, dl.source).await?;

    Ok(())
}
