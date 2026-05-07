use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::Value;

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};
use crate::lexicon::{LexiconType, ParsedLexicon, ProcedureAction};

use super::auth::UserAuth;
use super::permissions::Permission;
use super::types::{LexiconSummary, UploadLexiconBody};

/// Broadcast the current record collection list so the Jetstream
/// subscription syncs its filter.
async fn notify_collections(state: &AppState) {
    let collections = state.lexicons.get_record_collections().await;
    let _ = state.collections_tx.send(collections);
}

/// POST /admin/lexicons — upload (upsert) a lexicon.
pub(super) async fn upload_lexicon(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<UploadLexiconBody>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    auth.require(Permission::LexiconsCreate).await?;
    let backend = state.db_backend;
    // Validate basic structure
    let lexicon_version = body
        .lexicon_json
        .get("lexicon")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| {
            AppError::BadRequest("lexicon JSON must have a numeric 'lexicon' field".into())
        })?;

    if lexicon_version != 1 {
        return Err(AppError::BadRequest(format!(
            "unsupported lexicon version: {lexicon_version}"
        )));
    }

    let id = body
        .lexicon_json
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("lexicon JSON must have a string 'id' field".into()))?
        .to_string();

    // Validate action
    let action =
        ProcedureAction::from_optional_str(body.action.as_deref()).map_err(AppError::BadRequest)?;

    // Validate it parses correctly
    ParsedLexicon::parse(
        body.lexicon_json.clone(),
        1,
        body.target_collection.clone(),
        action.clone(),
        body.token_cost.map(|c| c as u32),
    )
    .map_err(|e| AppError::BadRequest(format!("failed to parse lexicon: {e}")))?;

    let action_str = action.to_optional_str();
    let lexicon_json_str = serde_json::to_string(&body.lexicon_json).unwrap_or_default();
    let now = now_rfc3339();

    // Upsert into database
    let sql = adapt_sql(
        r#"
        INSERT INTO lexicons (id, lexicon_json, backfill, target_collection, action, token_cost, source, created_at)
        VALUES (?, ?, ?, ?, ?, ?, 'manual', ?)
        ON CONFLICT (id) DO UPDATE SET
            lexicon_json = EXCLUDED.lexicon_json,
            backfill = EXCLUDED.backfill,
            target_collection = EXCLUDED.target_collection,
            action = EXCLUDED.action,
            token_cost = EXCLUDED.token_cost,
            source = 'manual',
            revision = lexicons.revision + 1,
            updated_at = ?
        RETURNING revision
        "#,
        backend,
    );
    let row: (i32,) = sqlx::query_as(&sql)
        .bind(&id)
        .bind(&lexicon_json_str)
        .bind(if body.backfill { 1_i32 } else { 0_i32 })
        .bind(&body.target_collection)
        .bind(action_str)
        .bind(body.token_cost)
        .bind(&now)
        .bind(&now)
        .fetch_one(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to upsert lexicon: {e}")))?;

    let revision = row.0;

    // Update in-memory registry with correct revision
    let parsed = ParsedLexicon::parse(
        body.lexicon_json,
        revision,
        body.target_collection,
        action,
        body.token_cost.map(|c| c as u32),
    )
    .map_err(|e| AppError::Internal(format!("failed to re-parse lexicon: {e}")))?;
    let is_record = parsed.lexicon_type == LexiconType::Record;
    state.lexicons.upsert(parsed).await;

    if is_record {
        notify_collections(&state).await;
    }

    let status = if revision == 1 {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };

    let event_type = if status == StatusCode::CREATED {
        "lexicon.created"
    } else {
        "lexicon.updated"
    };
    log_event(
        &state.db,
        EventLog {
            event_type: event_type.to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(id.clone()),
            detail: serde_json::json!({
                "revision": revision,
                "source": "manual",
            }),
        },
        backend,
    )
    .await;

    Ok((
        status,
        Json(serde_json::json!({
            "id": id,
            "revision": revision,
        })),
    ))
}

/// GET /admin/lexicons — list all lexicons.
pub(super) async fn list_lexicons(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<Vec<LexiconSummary>>, AppError> {
    auth.require(Permission::LexiconsRead).await?;
    let backend = state.db_backend;
    let sql = adapt_sql(
        "SELECT id, revision, lexicon_json, backfill, action, target_collection, source, authority_did, last_fetched_at, created_at, updated_at, token_cost FROM lexicons ORDER BY id",
        backend,
    );
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        String,
        i32,
        String,
        i32,
        Option<String>,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        String,
        String,
        Option<i32>,
    )> = sqlx::query_as(&sql)
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list lexicons: {e}")))?;

    let summaries: Vec<LexiconSummary> = rows
        .into_iter()
        .map(
            |(
                id,
                revision,
                json_str,
                backfill,
                action,
                target_collection,
                source,
                authority_did,
                last_fetched_at,
                created_at,
                updated_at,
                token_cost,
            )| {
                let json: Value = serde_json::from_str(&json_str).unwrap_or_default();
                let parsed =
                    ParsedLexicon::parse(json, revision, None, ProcedureAction::Upsert, None);
                let lexicon_type = parsed
                    .as_ref()
                    .map(|p| format!("{:?}", p.lexicon_type).to_lowercase())
                    .unwrap_or_else(|_| "unknown".into());
                let record_schema = parsed
                    .ok()
                    .filter(|p| p.lexicon_type == LexiconType::Record)
                    .and_then(|p| p.record_schema);

                LexiconSummary {
                    id,
                    revision,
                    lexicon_type,
                    backfill: backfill != 0,
                    action,
                    target_collection,
                    source,
                    authority_did,
                    last_fetched_at,
                    created_at,
                    updated_at,
                    record_schema,
                    token_cost,
                }
            },
        )
        .collect();

    Ok(Json(summaries))
}

/// GET /admin/lexicons/:id — get a single lexicon.
pub(super) async fn get_lexicon(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    auth.require(Permission::LexiconsRead).await?;
    let backend = state.db_backend;
    let sql = adapt_sql(
        "SELECT id, revision, lexicon_json, backfill, action, target_collection, source, authority_did, last_fetched_at, created_at, updated_at, token_cost FROM lexicons WHERE id = ?",
        backend,
    );
    #[allow(clippy::type_complexity)]
    let row: Option<(
        String,
        i32,
        String,
        i32,
        Option<String>,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        String,
        String,
        Option<i32>,
    )> = sqlx::query_as(&sql)
        .bind(&id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to get lexicon: {e}")))?;

    let (
        id,
        revision,
        lexicon_json_str,
        backfill,
        action,
        target_collection,
        source,
        authority_did,
        last_fetched_at,
        created_at,
        updated_at,
        token_cost,
    ) = row.ok_or_else(|| AppError::NotFound(format!("lexicon '{id}' not found")))?;

    let lexicon_json: Value = serde_json::from_str(&lexicon_json_str).unwrap_or_default();

    let lexicon_type = ParsedLexicon::parse(
        lexicon_json.clone(),
        revision,
        None,
        ProcedureAction::Upsert,
        None,
    )
    .map(|p| format!("{:?}", p.lexicon_type).to_lowercase())
    .unwrap_or_else(|_| "unknown".into());

    Ok(Json(serde_json::json!({
        "id": id,
        "revision": revision,
        "lexicon_json": lexicon_json,
        "lexicon_type": lexicon_type,
        "backfill": backfill != 0,
        "action": action,
        "target_collection": target_collection,
        "source": source,
        "authority_did": authority_did,
        "last_fetched_at": last_fetched_at,
        "created_at": created_at,
        "updated_at": updated_at,
        "token_cost": token_cost,
    })))
}

/// DELETE /admin/lexicons/:id — remove a lexicon.
pub(super) async fn delete_lexicon(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::LexiconsDelete).await?;
    let backend = state.db_backend;
    let sql = adapt_sql("DELETE FROM lexicons WHERE id = ?", backend);
    let result = sqlx::query(&sql)
        .bind(&id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete lexicon: {e}")))?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("lexicon '{id}' not found")));
    }

    state.lexicons.remove(&id).await;
    notify_collections(&state).await;

    log_event(
        &state.db,
        EventLog {
            event_type: "lexicon.deleted".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(id.clone()),
            detail: serde_json::json!({}),
        },
        backend,
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}
