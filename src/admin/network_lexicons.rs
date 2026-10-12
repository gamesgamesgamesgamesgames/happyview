use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::Value;

use crate::AppState;
use crate::error::AppError;
use crate::lexicon::{LexiconType, ProcedureAction};
use crate::resolve::{fetch_lexicon_from_pds, resolve_nsid_authority};

use super::auth::UserAuth;
use super::permissions::Permission;
use super::types::{AddNetworkLexiconBody, NetworkLexiconSummary};

/// Broadcast the current record collection list so the Jetstream
/// subscription syncs its filter.
async fn notify_collections(state: &AppState) {
    let collections = state.lexicons.get_record_collections().await;
    let _ = state.collections_tx.send(collections);
}

/// GET /admin/network-lexicons/resolve/{nsid} — preview-resolve a network lexicon without persisting.
pub(super) async fn resolve(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(nsid): Path<String>,
) -> Result<Json<Value>, AppError> {
    auth.require(Permission::LexiconsRead).await?;

    let (authority_did, pds_endpoint) =
        resolve_nsid_authority(&state.http, &state.config.plc_url, &nsid).await?;

    let lexicon_json =
        fetch_lexicon_from_pds(&state.http, &pds_endpoint, &authority_did, &nsid).await?;

    let main_type = lexicon_json
        .get("defs")
        .and_then(|d| d.get("main"))
        .and_then(|m| m.get("type"))
        .and_then(|t| t.as_str());

    Ok(Json(serde_json::json!({
        "nsid": nsid,
        "authority_did": authority_did,
        "type": main_type,
        "lexicon_json": lexicon_json,
    })))
}

/// POST /admin/network-lexicons — add a network lexicon to watch.
pub(super) async fn add(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<AddNetworkLexiconBody>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    auth.require(Permission::LexiconsCreate).await?;
    let nsid = &body.nsid;

    // Ahead of resolution: a file the project owns is never the network's.
    state.lexicons.check_writable(nsid)?;

    // Resolve NSID authority via DNS TXT lookup.
    let (authority_did, pds_endpoint) =
        resolve_nsid_authority(&state.http, &state.config.plc_url, nsid).await?;

    // Fetch the lexicon from the authority's PDS.
    let lexicon_json =
        fetch_lexicon_from_pds(&state.http, &pds_endpoint, &authority_did, nsid).await?;

    let revision = state
        .lexicons
        .put_network(
            &state.db,
            state.db_backend,
            crate::catalog::LexiconWrite {
                lexicon_json,
                backfill: body.backfill,
                target_collection: body.target_collection,
                action: ProcedureAction::Upsert,
                token_cost: None,
                authority_did: Some(authority_did.clone()),
            },
        )
        .await?;
    let is_record = state
        .lexicons
        .get(nsid)
        .await
        .is_some_and(|l| l.lexicon_type == LexiconType::Record);

    if is_record {
        notify_collections(&state).await;
    }

    let backfill_job_id: Option<String> =
        if is_record && body.backfill && revision == 1 && auth.has(Permission::BackfillCreate) {
            match crate::admin::backfill::start_backfill(
                &state,
                Some(nsid.clone()),
                Vec::new(),
                &auth.did,
            )
            .await
            {
                Ok(job_id) => Some(job_id),
                Err(e) => {
                    tracing::warn!(
                        lexicon = nsid.as_str(),
                        "failed to start backfill for network lexicon: {e}"
                    );
                    None
                }
            }
        } else {
            None
        };

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "nsid": nsid,
            "authority_did": authority_did,
            "revision": revision,
            "backfill_job_id": backfill_job_id,
        })),
    ))
}

/// GET /admin/network-lexicons — list tracked network lexicons.
pub(super) async fn list(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<Vec<NetworkLexiconSummary>>, AppError> {
    auth.require(Permission::LexiconsRead).await?;

    let summaries: Vec<NetworkLexiconSummary> = state
        .lexicons
        .list_network(&state.db, state.db_backend)
        .await?
        .into_iter()
        .map(|l| NetworkLexiconSummary {
            nsid: l.id,
            authority_did: l.authority_did.unwrap_or_default(),
            target_collection: l.target_collection,
            last_fetched_at: l.last_fetched_at,
            created_at: l.created_at.unwrap_or_default(),
        })
        .collect();

    Ok(Json(summaries))
}

/// DELETE /admin/network-lexicons/{nsid} — stop watching a network lexicon.
pub(super) async fn remove(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(nsid): Path<String>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::LexiconsDelete).await?;

    state.lexicons.check_writable(&nsid)?;
    if !state
        .lexicons
        .remove_network(&state.db, state.db_backend, &nsid)
        .await?
    {
        return Err(AppError::NotFound(format!(
            "network lexicon '{nsid}' not found"
        )));
    }
    notify_collections(&state).await;
    Ok(StatusCode::NO_CONTENT)
}
