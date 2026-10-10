use axum::Json;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::collections::HashMap;

use crate::AppState;
use crate::auth::Claims;
use crate::db::adapt_sql;
use crate::error::AppError;

/// The collection listing without a `did`, served from
/// `idx_records_collection_indexed_at` in order.
pub(crate) const LIST_RECORDS_SQL: &str = "SELECT uri, did, record FROM happyview_records WHERE collection = ? ORDER BY indexed_at DESC LIMIT ? OFFSET ?";

pub(crate) async fn handle_query(
    state: &AppState,
    method: &str,
    params: &HashMap<String, Value>,
    lexicon: &crate::lexicon::ParsedLexicon,
    claims: Option<&Claims>,
) -> Result<Response, AppError> {
    // Trigger-keyed dispatch: a script bound at `xrpc.query:<id>`
    // overrides the default list / get-record flow.
    let trigger = crate::lua::ParsedTrigger::new(crate::lua::TriggerKind::XrpcQuery, &lexicon.id);
    if let Some(resolved) = crate::lua::resolve(state, &trigger).await {
        return crate::lua::execute_query_script(
            state, method, params, lexicon, &resolved, claims, None,
        )
        .await;
    }

    // Single-record query: has a `uri` parameter
    if let Some(uri) = params.get("uri").and_then(|v| v.as_str()) {
        return handle_get_record(state, uri).await;
    }

    // List query: needs a target collection to know what to query
    let collection = lexicon.target_collection.as_deref().ok_or_else(|| {
        AppError::BadRequest(format!(
            "{method} has no target_collection configured for list queries"
        ))
    })?;

    let limit: i64 = params
        .get("limit")
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(20)
        .min(100);

    let offset: i64 = params
        .get("cursor")
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(0);

    let did = params.get("did").and_then(|v| v.as_str());

    let backend = state.db_backend;

    let rows: Vec<(String, String, String)> = if let Some(did) = did {
        let sql = adapt_sql(
            "SELECT uri, did, record FROM happyview_records WHERE collection = ? AND did = ? ORDER BY indexed_at DESC LIMIT ? OFFSET ?",
            backend,
        );
        crate::db::query_as(&sql)
            .bind(collection)
            .bind(did)
            .bind(limit)
            .bind(offset)
            .fetch_all(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("DB query failed: {e}")))?
    } else {
        let sql = adapt_sql(LIST_RECORDS_SQL, backend);
        crate::db::query_as(&sql)
            .bind(collection)
            .bind(limit)
            .bind(offset)
            .fetch_all(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("DB query failed: {e}")))?
    };

    let has_next_page = rows.len() as i64 == limit;

    let records: Vec<Value> = rows
        .into_iter()
        .filter_map(|(uri, _did, record_str)| {
            let mut record: Value = serde_json::from_str(&record_str).ok()?;
            record
                .as_object_mut()
                .map(|obj| obj.insert("uri".to_string(), json!(uri)));
            Some(record)
        })
        .collect();

    let mut result = json!({ "records": records });
    if has_next_page {
        let next_cursor = (offset + limit).to_string();
        result
            .as_object_mut()
            .unwrap()
            .insert("cursor".to_string(), json!(next_cursor));
    }

    Ok(Json(result).into_response())
}

pub(super) async fn handle_get_record(state: &AppState, uri: &str) -> Result<Response, AppError> {
    let backend = state.db_backend;
    let sql = adapt_sql(
        "SELECT record FROM happyview_records WHERE uri = ?",
        backend,
    );
    let row: Option<(String,)> = crate::db::query_as(&sql)
        .bind(uri)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("DB query failed: {e}")))?;

    let (record_str,) = row.ok_or_else(|| AppError::NotFound("record not found".into()))?;
    let mut record: Value = serde_json::from_str(&record_str)
        .map_err(|e| AppError::Internal(format!("invalid record JSON: {e}")))?;

    record
        .as_object_mut()
        .map(|obj| obj.insert("uri".to_string(), json!(uri)));

    Ok(Json(json!({ "record": record })).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_xrpc_listing_walks_the_collection_indexed_at_index() {
        let sql =
            crate::test_support::inline_binds(LIST_RECORDS_SQL, &["'app.test.post'", "50", "0"]);
        for (pool, backend) in crate::test_support::test_pools().await {
            let plan = crate::test_support::query_plan(&pool, backend, &sql).await;
            crate::test_support::assert_index_without_sort(
                &plan,
                "idx_records_collection_indexed_at",
                backend,
            );
        }
    }
}
