use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::Value;

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::lexicon::{LexiconType, ParsedLexicon, ProcedureAction};
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

/// POST /admin/network-lexicons — add a network lexicon to watch.
pub(super) async fn add(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<AddNetworkLexiconBody>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    auth.require(Permission::LexiconsCreate).await?;
    let nsid = &body.nsid;

    // Resolve NSID authority via DNS TXT lookup.
    let (authority_did, pds_endpoint) =
        resolve_nsid_authority(&state.http, &state.config.plc_url, nsid).await?;

    // Fetch the lexicon from the authority's PDS.
    let lexicon_json =
        fetch_lexicon_from_pds(&state.http, &pds_endpoint, &authority_did, nsid).await?;

    // Parse to validate.
    let parsed = ParsedLexicon::parse(
        lexicon_json.clone(),
        1,
        body.target_collection.clone(),
        ProcedureAction::Upsert,
        None,
    )
    .map_err(|e| AppError::BadRequest(format!("failed to parse lexicon: {e}")))?;

    let backend = state.db_backend;
    let now = now_rfc3339();
    let lexicon_json_str = serde_json::to_string(&lexicon_json).unwrap_or_default();

    // Upsert into lexicons table with network source.
    let sql = adapt_sql(
        r#"
        INSERT INTO lexicons (id, lexicon_json, backfill, target_collection, source, authority_did, last_fetched_at, created_at)
        VALUES (?, ?, 0, ?, 'network', ?, ?, ?)
        ON CONFLICT (id) DO UPDATE SET
            lexicon_json = EXCLUDED.lexicon_json,
            target_collection = EXCLUDED.target_collection,
            source = 'network',
            authority_did = EXCLUDED.authority_did,
            last_fetched_at = ?,
            revision = lexicons.revision + 1,
            updated_at = ?
        RETURNING revision
        "#,
        backend,
    );
    let row: (i32,) = sqlx::query_as(&sql)
        .bind(nsid)
        .bind(&lexicon_json_str)
        .bind(&body.target_collection)
        .bind(&authority_did)
        .bind(&now)
        .bind(&now)
        .bind(&now)
        .bind(&now)
        .fetch_one(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to upsert network lexicon: {e}")))?;

    let revision = row.0;

    // Update in-memory registry.
    let is_record = parsed.lexicon_type == LexiconType::Record;
    let parsed = ParsedLexicon::parse(
        lexicon_json,
        revision,
        body.target_collection,
        ProcedureAction::Upsert,
        None,
    )
    .map_err(|e| AppError::Internal(format!("failed to re-parse lexicon: {e}")))?;
    state.lexicons.upsert(parsed).await;

    if is_record {
        notify_collections(&state).await;
    }

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "nsid": nsid,
            "authority_did": authority_did,
            "revision": revision,
        })),
    ))
}

/// GET /admin/network-lexicons — list tracked network lexicons.
pub(super) async fn list(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<Vec<NetworkLexiconSummary>>, AppError> {
    auth.require(Permission::LexiconsRead).await?;

    let backend = state.db_backend;
    let sql = adapt_sql(
        "SELECT id, authority_did, target_collection, last_fetched_at, created_at FROM lexicons WHERE source = 'network' ORDER BY id",
        backend,
    );
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
    )> = sqlx::query_as(&sql)
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list network lexicons: {e}")))?;

    let summaries: Vec<NetworkLexiconSummary> = rows
        .into_iter()
        .map(
            |(nsid, authority_did, target_collection, last_fetched_at, created_at)| {
                NetworkLexiconSummary {
                    nsid,
                    authority_did: authority_did.unwrap_or_default(),
                    target_collection,
                    last_fetched_at,
                    created_at,
                }
            },
        )
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

    let backend = state.db_backend;
    let sql = adapt_sql(
        "DELETE FROM lexicons WHERE id = ? AND source = 'network'",
        backend,
    );
    let result = sqlx::query(&sql)
        .bind(&nsid)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete network lexicon: {e}")))?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!(
            "network lexicon '{nsid}' not found"
        )));
    }

    state.lexicons.remove(&nsid).await;
    notify_collections(&state).await;

    Ok(StatusCode::NO_CONTENT)
}
