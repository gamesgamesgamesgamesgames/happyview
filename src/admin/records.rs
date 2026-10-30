use std::collections::HashMap;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;

use super::auth::UserAuth;
use super::permissions::Permission;

#[derive(Deserialize)]
pub(super) struct ListRecordsParams {
    pub collection: String,
    pub limit: Option<i64>,
    pub cursor: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct DeleteRecordParams {
    pub uri: String,
}

#[derive(Serialize)]
pub(super) struct RecordLabel {
    pub src: String,
    pub val: String,
    pub cts: String,
}

#[derive(Serialize)]
pub(super) struct RecordEntry {
    pub uri: String,
    pub did: String,
    pub collection: String,
    pub rkey: String,
    pub cid: String,
    pub indexed_at: Option<String>,
    pub record: Value,
    pub labels: Vec<RecordLabel>,
}

#[derive(Serialize)]
pub(super) struct ListRecordsResponse {
    pub records: Vec<RecordEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

type RecordRow = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    String,
);

/// GET /admin/records?collection=X&limit=N&cursor=C — browse records by collection.
pub(super) async fn list_records(
    State(state): State<AppState>,
    auth: UserAuth,
    Query(params): Query<ListRecordsParams>,
) -> Result<Json<ListRecordsResponse>, AppError> {
    auth.require(Permission::RecordsRead).await?;
    let backend = state.db_backend;
    let limit = params.limit.unwrap_or(20).min(100);
    let offset: i64 = params
        .cursor
        .as_deref()
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);

    let rows: Vec<RecordRow> = state
        .records
        .list_recent(&params.collection, None, limit + 1, offset)
        .await
        .map_err(|e| record_error(&state, Some(&params.collection), "list records", e))?
        .into_iter()
        .map(|r| {
            (
                r.uri,
                r.did,
                r.collection,
                r.rkey,
                r.cid.unwrap_or_default(),
                r.indexed_at,
                r.record,
            )
        })
        .collect();

    let has_more = rows.len() as i64 > limit;
    let visible_rows: Vec<RecordRow> = rows.into_iter().take(limit as usize).collect();

    // Batch-query external labels for all visible URIs
    let uris: Vec<&str> = visible_rows
        .iter()
        .map(|(uri, _, _, _, _, _, _)| uri.as_str())
        .collect();

    let label_rows: Vec<(String, String, String, String)> = if uris.is_empty() {
        Vec::new()
    } else {
        let now = now_rfc3339();
        let ph_str = (0..uris.len()).map(|_| "?").collect::<Vec<_>>().join(", ");
        let raw_sql = format!(
            "SELECT uri, src, val, cts FROM happyview_labels WHERE uri IN ({ph_str}) AND (exp IS NULL OR exp > ?)"
        );
        let sql = adapt_sql(&raw_sql, backend);
        let mut q = crate::db::query_as(&sql);
        for uri in &uris {
            q = q.bind(*uri);
        }
        q = q.bind(&now);
        q.fetch_all(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("failed to fetch labels: {e}")))?
    };

    // Group external labels by URI
    let mut labels_by_uri: HashMap<String, Vec<RecordLabel>> = HashMap::new();
    for (uri, src, val, cts) in label_rows {
        labels_by_uri
            .entry(uri)
            .or_default()
            .push(RecordLabel { src, val, cts });
    }

    let records: Vec<RecordEntry> = visible_rows
        .into_iter()
        .map(
            |(uri, did, collection, rkey, cid, indexed_at, record_str)| {
                let record: Value = serde_json::from_str(&record_str).unwrap_or_default();
                let mut labels = labels_by_uri.remove(&uri).unwrap_or_default();

                // Extract self-labels from record JSONB
                if let Some(values) = record
                    .get("labels")
                    .and_then(|l| l.get("values"))
                    .and_then(|v| v.as_array())
                {
                    for entry in values {
                        if let Some(val) = entry.get("val").and_then(|v| v.as_str()) {
                            labels.push(RecordLabel {
                                src: did.clone(),
                                val: val.to_string(),
                                cts: String::new(),
                            });
                        }
                    }
                }

                RecordEntry {
                    uri,
                    did,
                    collection,
                    rkey,
                    cid,
                    indexed_at,
                    record,
                    labels,
                }
            },
        )
        .collect();

    let cursor = if has_more {
        Some((offset + limit).to_string())
    } else {
        None
    };

    Ok(Json(ListRecordsResponse { records, cursor }))
}

#[derive(Deserialize)]
pub(super) struct DeleteCollectionParams {
    pub collection: String,
}

/// DELETE /admin/records/collection?collection=X — enqueue deletion of every
/// record in a collection.
///
/// Returns `202` with a job id rather than deleting inline: a large collection
/// takes long enough that a synchronous request would time out, and running it
/// as a job gives progress, pause and cancel through the existing jobs UI.
pub(super) async fn delete_collection_records(
    State(state): State<AppState>,
    auth: UserAuth,
    Query(params): Query<DeleteCollectionParams>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    auth.require(Permission::RecordsDeleteCollection).await?;
    auth.require(Permission::RecordsDelete).await?;

    if params.collection.trim().is_empty() {
        return Err(AppError::BadRequest("collection is required".into()));
    }

    let job_id = crate::jobs::db::create_job(
        &state,
        "happyview.delete-collection",
        &serde_json::json!({ "collection": params.collection }),
        &auth.did,
        false,
        None,
        None,
    )
    .await?;

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "job_id": job_id })),
    ))
}

/// DELETE /admin/records?uri=at://... — delete a single record by URI.
pub(super) async fn delete_record(
    State(state): State<AppState>,
    auth: UserAuth,
    Query(params): Query<DeleteRecordParams>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::RecordsDelete).await?;
    let collection = crate::record_store::codec::parse_record_uri(&params.uri)
        .map(|(_, collection, _)| collection);
    let deleted = state
        .records
        .delete(&params.uri)
        .await
        .map_err(|e| record_error(&state, collection, "delete record", e))?;
    if !deleted {
        return Err(AppError::NotFound("record not found".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// GET /admin/records/collections — list collection names (fast, no record counting).
pub(super) async fn list_collections(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<Value>, AppError> {
    auth.require(Permission::RecordsRead).await?;

    let collections = state.lexicons.get_record_lexicon_ids().await;
    Ok(Json(serde_json::json!({ "collections": collections })))
}

/// A failed record call on `collection`: 503 when the external storage that
/// claims it is unreachable, otherwise a 500.
fn record_error(
    state: &AppState,
    collection: Option<&str>,
    what: &str,
    e: sqlx::Error,
) -> AppError {
    let storage_down = collection
        .and_then(|collection| state.records.external_route(collection))
        .is_some_and(|ext| ext.unavailable(&e));
    if storage_down {
        crate::error::storage_unavailable()
    } else {
        AppError::Internal(format!("failed to {what}: {e}"))
    }
}
