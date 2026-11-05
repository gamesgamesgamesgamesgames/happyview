use axum::Json;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::collections::HashMap;

use crate::AppState;
use crate::auth::Claims;
use crate::error::AppError;

pub(crate) async fn handle_query(
    state: &AppState,
    method: &str,
    params: &HashMap<String, Value>,
    lexicon: &crate::lexicon::ParsedLexicon,
    claims: Option<&Claims>,
) -> Result<Response, AppError> {
    // A query is its script, bound at `xrpc.query:<id>`. Without one, only
    // fetching a single record by `uri` needs nothing the lexicon cannot say.
    let trigger = crate::lua::ParsedTrigger::new(crate::lua::TriggerKind::XrpcQuery, &lexicon.id);
    let project = state.project.load_full();
    if let Some(resolved) = state.scripts.resolve_in(&project, &trigger).await {
        return crate::lua::execute_query_script(
            state, method, params, lexicon, &resolved, claims, None,
        )
        .await;
    }

    if let Some(uri) = params.get("uri").and_then(|v| v.as_str()) {
        return handle_get_record(state, uri).await;
    }

    Err(no_script("query", method))
}

/// The answer for a query or procedure with no script to run. A script names
/// the collection it reads or writes, so there is no default to fall back on.
pub(super) fn no_script(kind: &str, method: &str) -> AppError {
    AppError::XrpcError {
        status: axum::http::StatusCode::NOT_IMPLEMENTED,
        code: "MethodNotImplemented",
        message: format!("{method} has no script. Add one for the trigger xrpc.{kind}:{method}"),
    }
}

pub(super) async fn handle_get_record(state: &AppState, uri: &str) -> Result<Response, AppError> {
    let collection = crate::record_store::codec::parse_record_uri(uri)
        .map(|(_, collection, _)| collection)
        .unwrap_or_default();
    let stored = state
        .records
        .get(uri)
        .await
        .map_err(|e| read_error(state, collection, e))?
        .ok_or_else(|| AppError::NotFound("record not found".into()))?;
    let mut record: Value = serde_json::from_str(&stored.record)
        .map_err(|e| AppError::Internal(format!("invalid record JSON: {e}")))?;

    record
        .as_object_mut()
        .map(|obj| obj.insert("uri".to_string(), json!(uri)));

    Ok(Json(json!({ "record": record })).into_response())
}

/// A failed read of `collection`: 503 when the external storage that claims
/// it is down. A local collection's failure stays a 500, as it always was.
fn read_error(state: &AppState, collection: &str, e: sqlx::Error) -> AppError {
    let storage_down = state
        .records
        .external_route(collection)
        .is_some_and(|ext| ext.unavailable(&e));
    crate::error::record_read_error(e, storage_down)
}
