use std::collections::HashSet;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde_json::Value;
use sqlx::AnyPool;
use tracing::{info, warn};

use crate::AppState;
use crate::catalog::{CatalogError, LexiconCatalog, LexiconWrite};
use crate::db::DatabaseBackend;
use crate::error::AppError;
use crate::lexicon::{LexiconType, ProcedureAction};
use crate::resolve::{fetch_lexicon_from_pds, resolve_nsid_authority};

use super::auth::UserAuth;
use super::permissions::Permission;
use super::types::{AddNetworkLexiconBody, NetworkLexiconSummary};

/// Broadcast the current record collection list so the Jetstream
/// subscription syncs its filter. Stored even when nothing listens yet: the
/// backfill job system reads it to know what it may backfill.
async fn notify_collections(state: &AppState) {
    let collections = state.lexicons.get_record_collections().await;
    let _ = state.collections_tx.send_replace(collections);
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

pub(crate) struct TrackedNetworkLexicon {
    pub authority_did: String,
    pub revision: i32,
    pub is_record: bool,
    /// What was stored, after an omitted setting fell back to the existing
    /// row's or to indexed.
    pub indexed: bool,
}

/// Resolve an NSID's authority, fetch its lexicon and store it as a network
/// row through the catalog. Shared by the dashboard's add and the boot-time
/// application of `[network_lexicons]`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn track_network_lexicon(
    http: &reqwest::Client,
    plc_url: &str,
    db: &AnyPool,
    backend: DatabaseBackend,
    lexicons: &LexiconCatalog,
    nsid: &str,
    target_collection: Option<String>,
    backfill: bool,
    indexed: Option<bool>,
) -> Result<TrackedNetworkLexicon, AppError> {
    // Ahead of resolution: a file the project owns is never the network's.
    lexicons.check_writable(nsid)?;

    let indexed = match indexed {
        Some(v) => v,
        None => lexicons
            .get_listing(db, backend, nsid)
            .await?
            .is_none_or(|l| l.indexed),
    };

    let (authority_did, pds_endpoint) = resolve_nsid_authority(http, plc_url, nsid).await?;
    let lexicon_json = fetch_lexicon_from_pds(http, &pds_endpoint, &authority_did, nsid).await?;

    let revision = lexicons
        .put_network(
            db,
            backend,
            LexiconWrite {
                lexicon_json,
                backfill,
                target_collection,
                action: ProcedureAction::Upsert,
                token_cost: None,
                authority_did: Some(authority_did.clone()),
                indexed,
            },
        )
        .await?;
    let is_record = lexicons
        .get(nsid)
        .await
        .is_some_and(|l| l.lexicon_type == LexiconType::Record);

    Ok(TrackedNetworkLexicon {
        authority_did,
        revision,
        is_record,
        indexed,
    })
}

/// POST /admin/network-lexicons — add a network lexicon to watch.
pub(super) async fn add(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<AddNetworkLexiconBody>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    auth.require(Permission::LexiconsCreate).await?;
    let nsid = &body.nsid;

    let tracked = track_network_lexicon(
        &state.http,
        &state.config.plc_url,
        &state.db,
        state.db_backend,
        &state.lexicons,
        nsid,
        body.target_collection.clone(),
        body.backfill,
        body.indexed,
    )
    .await?;

    if tracked.is_record {
        notify_collections(&state).await;
    }

    let backfill_job_id: Option<String> = if tracked.is_record
        && body.backfill
        && tracked.indexed
        && tracked.revision == 1
        && auth.has(Permission::BackfillCreate)
    {
        match state
            .start_automatic_backfill(nsid.clone(), &auth.did)
            .await
        {
            Ok(job_id) => job_id,
            Err(e) => {
                warn!(
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
            "authority_did": tracked.authority_did,
            "revision": tracked.revision,
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

    let declared = crate::project::deploy::declared_network_lexicons(&state);
    let summaries: Vec<NetworkLexiconSummary> = state
        .lexicons
        .list_network(&state.db, state.db_backend)
        .await?
        .into_iter()
        .map(|l| NetworkLexiconSummary {
            declared: declared.contains(&l.id),
            nsid: l.id,
            authority_did: l.authority_did.unwrap_or_default(),
            target_collection: l.target_collection,
            last_fetched_at: l.last_fetched_at,
            created_at: l.created_at.unwrap_or_default(),
            indexed: l.indexed,
        })
        .collect();

    Ok(Json(summaries))
}

/// DELETE /admin/network-lexicons/{nsid} — stop watching a network lexicon.
/// `?delete_records=true`, as on the dashboard lexicon route.
#[derive(Debug, Default, serde::Deserialize)]
pub(super) struct RemoveParams {
    #[serde(default)]
    delete_records: bool,
}

/// DELETE /admin/network-lexicons/:nsid — stop tracking a network lexicon,
/// and with `?delete_records=true` also enqueue deletion of its indexed
/// records.
///
/// Everything about the records half matches `lexicons::delete_lexicon`,
/// deliberately: the same permission pair, the same record-lexicon-only rule,
/// the same refusal for a collection a shared store serves, and the same
/// ordering, so neither door is the lenient one.
pub(super) async fn remove(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(nsid): Path<String>,
    Query(params): Query<RemoveParams>,
) -> Result<axum::response::Response, AppError> {
    use axum::response::IntoResponse;

    auth.require(Permission::LexiconsDelete).await?;

    // The dashboard's error toast rewrites any message containing "network"
    // into a connection failure, so this one names the file instead.
    if crate::project::deploy::declares_network_lexicon(&state, &nsid) {
        return Err(AppError::Conflict(format!(
            "'{nsid}' is declared in happyview.toml; remove it from the file instead"
        )));
    }

    state.lexicons.check_writable(&nsid)?;

    // Decided while the lexicon still exists, as on the other route: a
    // refusal must not leave it removed and its records orphaned.
    let mut counted: Option<i64> = None;
    if params.delete_records {
        auth.require(Permission::RecordsDeleteCollection).await?;
        auth.require(Permission::RecordsDelete).await?;
        let lexicon = state
            .lexicons
            .get(&nsid)
            .await
            .ok_or_else(|| AppError::NotFound(format!("network lexicon '{nsid}' not found")))?;
        if lexicon.lexicon_type != LexiconType::Record {
            return Err(AppError::BadRequest(super::lexicons::not_a_record_lexicon(
                &nsid, &lexicon,
            )));
        }
        if let Some(reason) = super::lexicons::records_not_locally_deletable(&state, &nsid) {
            return Err(AppError::BadRequest(reason));
        }
        counted = state.records.count_collection(&nsid).await.ok();
    }

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

    if !params.delete_records {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    let job_id = crate::jobs::db::create_job(
        &state,
        "happyview.delete-collection",
        &serde_json::json!({ "collection": nsid }),
        &auth.did,
        false,
        None,
        None,
    )
    .await?;
    tracing::info!(
        collection = %nsid,
        job = %job_id,
        records = ?counted,
        "deleting a network lexicon's indexed records"
    );
    Ok((
        StatusCode::ACCEPTED,
        axum::Json(serde_json::json!({ "job_id": job_id, "collection": nsid })),
    )
        .into_response())
}

pub(crate) struct DeclaredPlan {
    pub to_track: Vec<String>,
    pub owned_by_dashboard: Vec<String>,
}

/// Which declared NSIDs still need tracking. One a dashboard lexicon already
/// uses is reported rather than tracked: tracking would rewrite that row as
/// a network lexicon, and the project never changes a dashboard row.
pub(crate) fn plan_declared(declared: &[String], existing: &[(String, String)]) -> DeclaredPlan {
    let mut plan = DeclaredPlan {
        to_track: Vec::new(),
        owned_by_dashboard: Vec::new(),
    };
    let mut seen = HashSet::new();
    for nsid in declared {
        if !seen.insert(nsid.as_str()) {
            continue;
        }
        match existing.iter().find(|(id, _)| id == nsid) {
            None => plan.to_track.push(nsid.clone()),
            Some((_, source)) if source == "network" => {}
            Some(_) => plan.owned_by_dashboard.push(nsid.clone()),
        }
    }
    plan
}

/// Every lexicon id the catalog answers for, with the layer it comes from.
pub(crate) async fn existing_lexicon_sources(
    lexicons: &LexiconCatalog,
    db: &AnyPool,
    backend: DatabaseBackend,
) -> Result<Vec<(String, String)>, CatalogError> {
    Ok(lexicons
        .list(db, backend)
        .await?
        .into_iter()
        .map(|l| (l.id, l.layer.as_str().to_string()))
        .collect())
}

/// Track every declared network lexicon not already tracked. An NSID that
/// fails to resolve is skipped with a warning and tried again next boot,
/// so an offline laptop still starts. No backfill starts: nobody asked for
/// a relay crawl at boot.
pub(crate) async fn track_declared<T, Fut>(
    lexicons: &LexiconCatalog,
    db: &AnyPool,
    backend: DatabaseBackend,
    declared: &[String],
    track: T,
) -> Result<DeclaredPlan, CatalogError>
where
    T: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<TrackedNetworkLexicon, AppError>>,
{
    let existing = existing_lexicon_sources(lexicons, db, backend).await?;
    let plan = plan_declared(declared, &existing);
    for nsid in &plan.owned_by_dashboard {
        let layer = existing
            .iter()
            .find(|(id, _)| id == nsid)
            .map(|(_, layer)| layer.as_str());
        let owner = if layer == Some("project") {
            "a project file defines"
        } else {
            "a dashboard lexicon has"
        };
        warn!(
            nsid,
            "happyview.toml declares this network lexicon, but {owner} the same id; that one is kept"
        );
    }
    for nsid in &plan.to_track {
        match track(nsid.clone()).await {
            Ok(_) => info!(nsid, "tracking network lexicon declared in happyview.toml"),
            Err(e) => warn!(
                nsid,
                "could not track network lexicon declared in happyview.toml: {e}"
            ),
        }
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::migrated_memory_pool;

    fn ids(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn only_untracked_declarations_are_tracked() {
        let existing = vec![
            ("app.bsky.actor.profile".to_string(), "network".to_string()),
            ("at.example.mine".to_string(), "manual".to_string()),
            ("at.example.file".to_string(), "project".to_string()),
        ];
        let plan = plan_declared(
            &ids(&[
                "app.bsky.actor.profile",
                "app.bsky.feed.post",
                "at.example.mine",
                "at.example.file",
                "app.bsky.feed.post",
            ]),
            &existing,
        );
        assert_eq!(
            plan.to_track,
            ids(&["app.bsky.feed.post"]),
            "tracked once, already-tracked skipped"
        );
        assert_eq!(
            plan.owned_by_dashboard,
            ids(&["at.example.mine", "at.example.file"])
        );
    }

    #[tokio::test]
    async fn a_dashboard_lexicon_with_a_declared_id_is_never_converted() {
        let db = migrated_memory_pool().await;
        let lexicons = LexiconCatalog::detached();
        lexicons
            .put_manual(
                &db,
                DatabaseBackend::Sqlite,
                LexiconWrite {
                    lexicon_json: serde_json::json!({"lexicon":1,"id":"at.example.mine","defs":{"main":{"type":"record","key":"tid","record":{"type":"object","properties":{}}}}}),
                    backfill: false,
                    target_collection: None,
                    action: ProcedureAction::Upsert,
                    token_cost: None,
                    authority_did: None,
                    indexed: true,
                },
            )
            .await
            .unwrap();

        let existing = existing_lexicon_sources(&lexicons, &db, DatabaseBackend::Sqlite)
            .await
            .unwrap();
        let plan = plan_declared(&ids(&["at.example.mine"]), &existing);
        assert!(plan.to_track.is_empty());
        assert_eq!(plan.owned_by_dashboard, ids(&["at.example.mine"]));
    }

    fn stub_tracked() -> TrackedNetworkLexicon {
        TrackedNetworkLexicon {
            authority_did: "did:plc:x".into(),
            revision: 1,
            is_record: true,
            indexed: true,
        }
    }

    #[tokio::test]
    async fn one_failed_track_does_not_stop_the_rest_and_owned_ids_are_skipped() {
        let db = migrated_memory_pool().await;
        let lexicons = LexiconCatalog::detached();
        lexicons
            .put_manual(
                &db,
                DatabaseBackend::Sqlite,
                LexiconWrite {
                    lexicon_json: serde_json::json!({"lexicon":1,"id":"at.example.mine","defs":{"main":{"type":"record","key":"tid","record":{"type":"object","properties":{}}}}}),
                    backfill: false,
                    target_collection: None,
                    action: ProcedureAction::Upsert,
                    token_cost: None,
                    authority_did: None,
                    indexed: true,
                },
            )
            .await
            .unwrap();

        let seen = std::sync::Mutex::new(Vec::new());
        let plan = track_declared(
            &lexicons,
            &db,
            DatabaseBackend::Sqlite,
            &ids(&["at.example.bad", "at.example.mine", "at.example.good"]),
            |nsid| {
                seen.lock().unwrap().push(nsid.clone());
                async move {
                    if nsid == "at.example.bad" {
                        Err(AppError::BadRequest("unresolvable".into()))
                    } else {
                        Ok(stub_tracked())
                    }
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(
            *seen.lock().unwrap(),
            ids(&["at.example.bad", "at.example.good"]),
            "the failure came first and the next id was still tried; the manual id never reached the tracker"
        );
        assert_eq!(plan.owned_by_dashboard, ids(&["at.example.mine"]));
    }
}
