use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::Value;

use crate::AppState;
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};
use crate::lexicon::{LexiconType, ProcedureAction};

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
    let id = body
        .lexicon_json
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("lexicon JSON must have a string 'id' field".into()))?
        .to_string();

    // Before any other validation, so a project-owned id is always a 409.
    state.lexicons.check_writable(&id)?;

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

    happyview_nsid::validate_nsid(&id).map_err(|e| AppError::BadRequest(e.to_string()))?;

    // Validate action
    let action =
        ProcedureAction::from_optional_str(body.action.as_deref()).map_err(AppError::BadRequest)?;

    let revision = state
        .lexicons
        .put_manual(
            &state.db,
            backend,
            crate::catalog::LexiconWrite {
                lexicon_json: body.lexicon_json,
                backfill: body.backfill,
                target_collection: body.target_collection,
                action,
                token_cost: body.token_cost,
                authority_did: None,
            },
        )
        .await?;
    let is_record = state
        .lexicons
        .get(&id)
        .await
        .is_some_and(|l| l.lexicon_type == LexiconType::Record);

    if is_record {
        notify_collections(&state).await;
    }

    let backfill_job_id: Option<String> =
        if is_record && body.backfill && revision == 1 && auth.has(Permission::BackfillCreate) {
            match crate::admin::backfill::start_backfill(
                &state,
                Some(id.clone()),
                Vec::new(),
                &auth.did,
            )
            .await
            {
                Ok(job_id) => Some(job_id),
                Err(e) => {
                    tracing::warn!(
                        lexicon = id.as_str(),
                        "failed to start backfill for uploaded lexicon: {e}"
                    );
                    None
                }
            }
        } else {
            None
        };

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
            "backfill_job_id": backfill_job_id,
        })),
    ))
}

/// GET /admin/lexicons — list all lexicons.
pub(super) async fn list_lexicons(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<Vec<LexiconSummary>>, AppError> {
    auth.require(Permission::LexiconsRead).await?;
    let listings = state.lexicons.list(&state.db, state.db_backend).await?;
    Ok(Json(
        listings.into_iter().map(LexiconSummary::from).collect(),
    ))
}

/// GET /admin/lexicons/:id — get a single lexicon.
pub(super) async fn get_lexicon(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    auth.require(Permission::LexiconsRead).await?;
    let listing = state
        .lexicons
        .get_listing(&state.db, state.db_backend, &id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("lexicon '{id}' not found")))?;
    let lexicon_json = listing.lexicon_json.clone();
    let summary = LexiconSummary::from(listing);
    let mut out = serde_json::to_value(&summary)
        .map_err(|e| AppError::Internal(format!("failed to encode lexicon: {e}")))?;
    out["lexicon_json"] = lexicon_json;
    if let Some(obj) = out.as_object_mut() {
        obj.remove("record_schema");
    }
    Ok(Json(out))
}

/// DELETE /admin/lexicons/:id — remove a lexicon.
pub(super) async fn delete_lexicon(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::LexiconsDelete).await?;
    let backend = state.db_backend;
    // The same message as the network-lexicon route: the file is the source of
    // truth, so a delete here would be undone at the next boot.
    if state.config.declares_network_lexicon(&id) {
        return Err(AppError::Conflict(format!(
            "'{id}' is declared in happyview.toml; remove it from the file instead"
        )));
    }
    if !state.lexicons.remove(&state.db, backend, &id).await? {
        return Err(AppError::NotFound(format!("lexicon '{id}' not found")));
    }
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
