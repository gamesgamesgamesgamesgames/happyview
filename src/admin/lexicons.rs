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
/// subscription syncs its filter. Stored even when nothing listens yet: the
/// backfill job system reads it to know what it may backfill.
async fn notify_collections(state: &AppState) {
    let collections = state.lexicons.get_record_collections().await;
    let _ = state.collections_tx.send_replace(collections);
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

    // Per field rather than one sentence: an editor can point at each place
    // the document is wrong, and warnings ride along with the errors so one
    // answer says everything. Same rule set the project build uses.
    let issues = crate::lexicon::validate::validate(&body.lexicon_json);
    if issues
        .iter()
        .any(|i| i.severity == crate::lexicon::validate::Severity::Error)
    {
        return Err(AppError::InvalidLexicon(issues));
    }
    // A warning does not refuse the upload, but it still has to reach the
    // caller: a `subscription` lexicon is valid, stores, and generates
    // nothing, and succeeding in silence is what makes that read as support.
    // Carried on the success response, since there is no error to attach it
    // to.
    let warnings = issues;

    // Validate action
    let action =
        ProcedureAction::from_optional_str(body.action.as_deref()).map_err(AppError::BadRequest)?;

    let indexed = match body.indexed {
        Some(v) => v,
        None => state
            .lexicons
            .get_listing(&state.db, backend, &id)
            .await?
            .is_none_or(|l| l.indexed),
    };

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
                indexed,
            },
        )
        .await?;
    crate::lexicon_publishing::spawn_reconcile(&state);

    let is_record = state
        .lexicons
        .get(&id)
        .await
        .is_some_and(|l| l.lexicon_type == LexiconType::Record);

    if is_record {
        notify_collections(&state).await;
    }

    let backfill_job_id: Option<String> = if is_record
        && body.backfill
        && indexed
        && revision == 1
        && auth.has(Permission::BackfillCreate)
    {
        match state.start_automatic_backfill(id.clone(), &auth.did).await {
            Ok(job_id) => job_id,
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
            "issues": warnings,
        })),
    ))
}

/// The refusal `delete_lexicon` answers is the one `declared` reports, so the
/// dashboard can leave out a Delete that could only fail.
fn summarize(declared: &[String], listing: crate::catalog::LexiconListing) -> LexiconSummary {
    let mut summary = LexiconSummary::from(listing);
    summary.declared = declared.contains(&summary.id);
    summary
}

/// GET /admin/lexicons — list all lexicons.
pub(super) async fn list_lexicons(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<Vec<LexiconSummary>>, AppError> {
    auth.require(Permission::LexiconsRead).await?;
    let listings = state.lexicons.list(&state.db, state.db_backend).await?;
    let declared = crate::project::deploy::declared_network_lexicons(&state);
    Ok(Json(
        listings
            .into_iter()
            .map(|l| summarize(&declared, l))
            .collect(),
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
    let summary = summarize(
        &crate::project::deploy::declared_network_lexicons(&state),
        listing,
    );
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
    if crate::project::deploy::declares_network_lexicon(&state, &id) {
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
