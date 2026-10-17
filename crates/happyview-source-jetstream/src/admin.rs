//! `/admin/backfill*` and `/admin/jetstream`: the Jetstream source's admin
//! routes, mounted by the host under `/admin` behind its auth layer.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use futures_util::stream::{self, StreamExt};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use happyview_provider::db::adapt_sql;
use happyview_provider::{AdminCaller, AdminRefusal, AdminUser, ProviderEvent, Severity};

use crate::archive::SourceChoice;
use crate::backfill::errors::{BackfillErrorKind, ERROR_DETAIL_CAP, ErrorCounts};
use crate::backfill::types::{
    BackfillErrorCount, BackfillErrorEntry, BackfillErrorsResponse, BackfillJob,
    CreateBackfillBody, ReplayProgress,
};
use crate::backfill::*;

// ---------------------------------------------------------------------------
// Admin handlers
// ---------------------------------------------------------------------------

/// POST /admin/backfill — create a backfill job and spawn background work.
pub async fn create_backfill(
    State(state): State<JobState>,
    AdminUser(admin): AdminUser,
    Json(body): Json<CreateBackfillBody>,
) -> Result<(StatusCode, Json<Value>), AdminError> {
    admin.require("backfill:create")?;

    let source = match body.source.as_deref() {
        None => state.archive.default_source,
        Some(raw) => crate::archive::parse_source(raw).ok_or_else(|| {
            AdminError::BadRequest("source must be one of auto, replay, crawl".into())
        })?,
    };

    let mut inputs = body.dids.unwrap_or_default();
    inputs.extend(body.did);
    // An account backfill crawls: block-granular planning downloads far more
    // than a crawl of a few repos, and the archive may not hold a repo's
    // oldest records.
    // An explicit `replay` is refused; a default of `replay` crawls them.
    if body.source.is_some() && source == SourceChoice::Replay && !inputs.is_empty() {
        return Err(AdminError::BadRequest(
            "replay is only available for collection backfills; account backfills always crawl"
                .into(),
        ));
    }
    let host = Arc::clone(&state.host);
    let dids = resolve_backfill_accounts(inputs, |input| {
        let host = Arc::clone(&host);
        async move {
            host.resolve_handle(&input)
                .await
                .map_err(|e| AdminError::BadRequest(e.0.message))
        }
    })
    .await?;

    let job_id = start_backfill_with(&state, body.collection, dids, &admin.did, source).await?;

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "id": job_id,
            "status": "running",
        })),
    ))
}

pub const MAX_BACKFILL_ACCOUNTS: usize = 500;

/// Resolve every requested account to a DID, all or nothing.
///
/// A partial job would silently skip accounts the operator asked for, so one
/// bad entry fails the request and the error lists every bad entry at once.
/// The cap is checked before resolving so an oversized request costs no
/// lookups.
///
/// An empty `inputs` means no accounts were asked for, which is a network
/// backfill. Entries that are all blank are refused instead: treating them as
/// "no accounts" would turn a malformed targeted request into a backfill of
/// the whole network.
pub async fn resolve_backfill_accounts<F, Fut>(
    inputs: Vec<String>,
    resolve: F,
) -> Result<Vec<String>, AdminError>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<String, AdminError>>,
{
    let supplied_any = !inputs.is_empty();
    let mut seen_inputs = HashSet::new();
    let unique: Vec<String> = inputs
        .into_iter()
        .map(|input| input.trim().to_string())
        .filter(|input| !input.is_empty() && seen_inputs.insert(input.clone()))
        .collect();

    if supplied_any && unique.is_empty() {
        return Err(AdminError::BadRequest(
            "no valid accounts given: every entry was blank".into(),
        ));
    }

    if unique.len() > MAX_BACKFILL_ACCOUNTS {
        return Err(AdminError::BadRequest(format!(
            "a backfill can target at most {MAX_BACKFILL_ACCOUNTS} accounts, got {}",
            unique.len()
        )));
    }

    let results: Vec<Result<String, AdminError>> = stream::iter(unique)
        .map(&resolve)
        .buffered(8)
        .collect()
        .await;

    let mut seen_dids = HashSet::new();
    let mut dids = Vec::new();
    let mut failures = Vec::new();
    for result in results {
        match result {
            Ok(did) => {
                if seen_dids.insert(did.clone()) {
                    dids.push(did);
                }
            }
            Err(AdminError::BadRequest(msg)) => failures.push(msg),
            Err(other) => failures.push(other.to_string()),
        }
    }

    if !failures.is_empty() {
        return Err(AdminError::BadRequest(format!(
            "could not resolve {} account(s): {}",
            failures.len(),
            failures.join("; ")
        )));
    }

    Ok(dids)
}

/// POST /admin/backfill/{id}/cancel — cancel a running backfill job.
pub async fn cancel_backfill(
    State(state): State<JobState>,
    AdminUser(admin): AdminUser,
    Path(job_id): Path<String>,
) -> Result<Json<Value>, AdminError> {
    admin.require("backfill:create")?;

    let sql = adapt_sql(
        "SELECT status FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    );
    let row: Option<(String,)> = happyview_provider::db::query_as(&sql)
        .bind(&job_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AdminError::Internal(format!("failed to query backfill job: {e}")))?;

    match row {
        None => Err(AdminError::NotFound("backfill job not found".into())),
        Some((ref status,)) if status == "cancelling" || status == "cancelled" => {
            Ok(Json(serde_json::json!({ "id": job_id, "status": status })))
        }
        Some((ref status,)) if status == "paused" => {
            finalise_cancel(&state, &job_id).await;
            state
                .host
                .log_event(ProviderEvent {
                    event_type: "backfill.cancelled".to_string(),
                    severity: Severity::Info,
                    actor_did: Some(admin.did.clone()),
                    subject: None,
                    detail: serde_json::json!({ "job_id": job_id }),
                })
                .await;
            Ok(Json(
                serde_json::json!({ "id": job_id, "status": "cancelled" }),
            ))
        }
        Some((status,)) if status != "running" => Err(AdminError::BadRequest(format!(
            "job is not running (status: {status})"
        ))),
        Some(_) => {
            request_cancel(&state, &job_id).await;
            state
                .host
                .log_event(ProviderEvent {
                    event_type: "backfill.cancelling".to_string(),
                    severity: Severity::Info,
                    actor_did: Some(admin.did.clone()),
                    subject: None,
                    detail: serde_json::json!({ "job_id": job_id }),
                })
                .await;
            Ok(Json(
                serde_json::json!({ "id": job_id, "status": "cancelling" }),
            ))
        }
    }
}

/// POST /admin/backfill/{id}/pause — pause a running backfill job.
pub async fn pause_backfill(
    State(state): State<JobState>,
    AdminUser(admin): AdminUser,
    Path(job_id): Path<String>,
) -> Result<Json<Value>, AdminError> {
    admin.require("backfill:create")?;

    let sql = adapt_sql(
        "SELECT status FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    );
    let row: Option<(String,)> = happyview_provider::db::query_as(&sql)
        .bind(&job_id)
        .fetch_optional(&state.backfill_db)
        .await
        .map_err(|e| AdminError::Internal(format!("failed to query backfill job: {e}")))?;

    match row {
        None => Err(AdminError::NotFound("backfill job not found".into())),
        Some((ref status,)) if status == "pausing" || status == "paused" => {
            Ok(Json(serde_json::json!({ "id": job_id, "status": status })))
        }
        Some((status,)) if status != "running" => Err(AdminError::BadRequest(format!(
            "job is not running (status: {status})"
        ))),
        Some(_) => {
            request_pause(&state, &job_id).await;
            state
                .host
                .log_event(ProviderEvent {
                    event_type: "backfill.pausing".to_string(),
                    severity: Severity::Info,
                    actor_did: Some(admin.did.clone()),
                    subject: None,
                    detail: serde_json::json!({ "job_id": job_id }),
                })
                .await;
            Ok(Json(
                serde_json::json!({ "id": job_id, "status": "pausing" }),
            ))
        }
    }
}

/// Why an account job's replay can't go on: its queue rows, which list its
/// accounts, went with a detail flush or the retention sweep. Without them a
/// replay would plan the whole network.
pub const DIDS_GONE: &str = "the job's DID list is no longer available; start a new backfill";

/// POST /admin/backfill/{id}/resume — resume a paused backfill job, or a
/// failed replay job: its sweeps, cursors and retry rows say exactly where it
/// stopped, so a transient failure doesn't throw away a long replay.
pub async fn resume_backfill(
    State(state): State<JobState>,
    AdminUser(admin): AdminUser,
    Path(job_id): Path<String>,
) -> Result<Json<Value>, AdminError> {
    admin.require("backfill:create")?;

    let sql = adapt_sql(
        "SELECT status, source, scope FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    );
    let row: Option<(String, String, String)> = happyview_provider::db::query_as(&sql)
        .bind(&job_id)
        .fetch_optional(&state.backfill_db)
        .await
        .map_err(|e| AdminError::Internal(format!("failed to query backfill job: {e}")))?;

    match row {
        None => Err(AdminError::NotFound("backfill job not found".into())),
        Some((status, source, _))
            if status != "paused" && !(status == "failed" && source == "replay") =>
        {
            Err(AdminError::BadRequest(format!(
                "job is not paused (status: {status})"
            )))
        }
        Some((status, _, scope))
            if status == "failed"
                && scope == "dids"
                && crate::backfill::replay_sweeps::job_dids(&state, &job_id)
                    .await
                    .map_err(|e| {
                        AdminError::Internal(format!("failed to read the job's accounts: {e}"))
                    })?
                    .is_empty() =>
        {
            Err(AdminError::BadRequest(DIDS_GONE.into()))
        }
        Some(_) => {
            // A failed job has a `completed_at`; a paused one has none.
            // Conditional, so of two resumes at once only one runs the job.
            let sql = adapt_sql(
                "UPDATE happyview_backfill_jobs SET status = 'running', error = NULL, completed_at = NULL \
                 WHERE id = ? AND (status = 'paused' OR (status = 'failed' AND source = 'replay'))",
                state.db_backend,
            );
            let resumed = happyview_provider::db::retry_on_busy(|| {
                happyview_provider::db::query(&sql)
                    .bind(&job_id)
                    .execute(&state.backfill_db)
            })
            .await
            .map_err(|e| AdminError::Internal(format!("failed to resume backfill job: {e}")))?
            .rows_affected();
            if resumed == 0 {
                return Err(AdminError::BadRequest(
                    "job is not paused (it was resumed or changed meanwhile)".into(),
                ));
            }

            let spawn_state = state.clone();
            let spawn_job_id = job_id.clone();
            tokio::spawn(async move {
                run_backfill_job(spawn_state, spawn_job_id).await;
            });

            state
                .host
                .log_event(ProviderEvent {
                    event_type: "backfill.resumed".to_string(),
                    severity: Severity::Info,
                    actor_did: Some(admin.did.clone()),
                    subject: None,
                    detail: serde_json::json!({ "job_id": job_id }),
                })
                .await;
            Ok(Json(
                serde_json::json!({ "id": job_id, "status": "running" }),
            ))
        }
    }
}

/// GET /admin/backfill/status — list all backfill jobs.
pub async fn backfill_status(
    State(state): State<JobState>,
    AdminUser(auth): AdminUser,
) -> Result<Json<Vec<BackfillJob>>, AdminError> {
    auth.require("backfill:read")?;
    let backend = state.db_backend;

    let sql = adapt_sql(
        "SELECT id, collection, did, scope, status, stage, total_repos, resolved_repos, processed_repos, total_records, error, started_at, completed_at, created_at, queue_version, discovery_complete FROM happyview_backfill_jobs ORDER BY created_at DESC",
        backend,
    );
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        String,
        Option<String>,
        Option<String>,
        String,
        String,
        String,
        Option<i32>,
        Option<i32>,
        Option<i32>,
        Option<i32>,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
        i32,
        i32,
    )> = happyview_provider::db::query_as(&sql)
        .fetch_all(&state.backfill_db)
        .await
        .map_err(|e| AdminError::Internal(format!("failed to list backfill jobs: {e}")))?;

    // The row above is at sqlx's tuple limit; the replay columns come apart.
    #[allow(clippy::type_complexity)]
    let replay_rows: Vec<(String, String, Option<i64>, Option<i64>, Option<i64>, i64, i32)> =
        happyview_provider::db::query_as(&adapt_sql(
            "SELECT id, source, replay_after_seq, replay_through_seq, replay_tip_seq, replay_bytes, replay_blocks_failed FROM happyview_backfill_jobs",
            backend,
        ))
        .fetch_all(&state.backfill_db)
        .await
        .map_err(|e| AdminError::Internal(format!("failed to list backfill jobs: {e}")))?;
    #[allow(clippy::type_complexity)]
    let mut replay: HashMap<String, (String, Option<i64>, Option<i64>, Option<i64>, i64, i32)> =
        replay_rows
            .into_iter()
            .map(|(id, source, after, through, tip, bytes, failed)| {
                (id, (source, after, through, tip, bytes, failed))
            })
            .collect();

    let jobs: Vec<BackfillJob> = rows
        .into_iter()
        .map(
            |(
                id,
                collection,
                did,
                scope,
                status,
                stage,
                total_repos,
                resolved_repos,
                processed_repos,
                total_records,
                error,
                started_at,
                completed_at,
                created_at,
                queue_version,
                discovery_complete,
            )| {
                let (source, after, through, tip, bytes, failed) = replay
                    .remove(&id)
                    .unwrap_or_else(|| ("crawl".to_string(), None, None, None, 0, 0));
                let replay = (source != "crawl").then_some(ReplayProgress {
                    after_seq: after.unwrap_or(0),
                    through_seq: through,
                    tip_seq: tip,
                    bytes,
                    blocks_failed: failed,
                });
                BackfillJob {
                    id,
                    collection,
                    did,
                    scope,
                    status,
                    stage,
                    total_repos,
                    resolved_repos,
                    processed_repos,
                    total_records,
                    error,
                    started_at,
                    completed_at,
                    created_at,
                    discovery_complete: discovery_complete != 0,
                    recent_completions_limit: (QueueVersion::from_column(queue_version)
                        == QueueVersion::Bounded)
                        .then_some(RECENT_COMPLETIONS_PER_JOB),
                    source,
                    replay,
                }
            },
        )
        .collect();

    Ok(Json(jobs))
}

// ---------------------------------------------------------------------------
// SSE events endpoint
// ---------------------------------------------------------------------------

pub async fn backfill_events(
    State(state): State<JobState>,
    Path(job_id): Path<String>,
    AdminUser(auth): AdminUser,
) -> Result<
    axum::response::sse::Sse<
        impl futures_util::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>,
    >,
    AdminError,
> {
    auth.require("backfill:read")?;

    let mut rx = state.backfill_events_tx.subscribe();

    let stream = async_stream::stream! {
        #[allow(clippy::collapsible_if)]
        if let Some(snapshot) = build_job_snapshot(&state, &job_id).await {
            if let Ok(json) = serde_json::to_string(&snapshot) {
                yield Ok(axum::response::sse::Event::default().event("event").data(json));
            }
        } else {
            tracing::warn!(job_id, "could not build initial job snapshot for SSE client");
        }

        loop {
            match rx.recv().await {
                Ok(event) => {
                    let event_job_id = match &event {
                        crate::backfill::types::BackfillEvent::RepoDiscovered { job_id, .. }
                        | crate::backfill::types::BackfillEvent::RepoResolved { job_id, .. }
                        | crate::backfill::types::BackfillEvent::RepoFetched { job_id, .. }
                        | crate::backfill::types::BackfillEvent::JobCounters { job_id, .. }
                        | crate::backfill::types::BackfillEvent::JobStageChanged { job_id, .. }
                        | crate::backfill::types::BackfillEvent::JobCompleted { job_id, .. }
                        | crate::backfill::types::BackfillEvent::ReplayProgress { job_id, .. }
                        | crate::backfill::types::BackfillEvent::JobSnapshot { job_id, .. } => job_id,
                    };
                    if *event_job_id != job_id {
                        continue;
                    }
                    if let Ok(json) = serde_json::to_string(&event) {
                        yield Ok(axum::response::sse::Event::default().event("event").data(json));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(job_id, skipped = n, "SSE client lagged behind, resyncing");
                    // Dropped deltas are unrecoverable; a snapshot is the only way back to
                    // a correct view.
                    #[allow(clippy::collapsible_if)]
                    if let Some(snapshot) = build_job_snapshot(&state, &job_id).await {
                        if let Ok(json) = serde_json::to_string(&snapshot) {
                            yield Ok(axum::response::sse::Event::default().event("event").data(json));
                        }
                    }
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Ok(axum::response::sse::Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default()))
}

// ---------------------------------------------------------------------------
// REST detail endpoints
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ReposQuery {
    pub phase: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<i32>,
}

/// A job's queue version, or `None` for an unknown job.
pub async fn job_queue_version(
    state: &JobState,
    job_id: &str,
) -> Result<Option<QueueVersion>, AdminError> {
    let sql = adapt_sql(
        "SELECT queue_version FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    );
    happyview_provider::db::query_as::<(i32,)>(&sql)
        .bind(job_id)
        .fetch_optional(&state.backfill_db)
        .await
        .map(|row| row.map(|(version,)| QueueVersion::from_column(version)))
        .map_err(|e| AdminError::Internal(format!("failed to query backfill job: {e}")))
}

/// `GET /admin/backfill/{id}/repos`.
///
/// A bounded job keeps no row per completed repo, so `phase=fetched` lists
/// its recent-completions log (`RECENT_COMPLETIONS_PER_JOB`, newest first).
/// `phase=resolved` and the default list its queued units. A legacy job
/// lists its `happyview_backfill_repos` rows as it always did. Cursors are
/// opaque and only valid for the listing that issued them.
pub async fn backfill_repos(
    State(state): State<JobState>,
    Path(job_id): Path<String>,
    AdminUser(auth): AdminUser,
    axum::extract::Query(query): axum::extract::Query<ReposQuery>,
) -> Result<Json<crate::backfill::types::BackfillReposResponse>, AdminError> {
    auth.require("backfill:read")?;

    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let cursor = query.cursor.as_deref();
    let page = match job_queue_version(&state, &job_id).await? {
        Some(QueueVersion::Bounded) => match query.phase.as_deref() {
            Some("fetched") => recent_completions_page(&state, &job_id, cursor, limit).await?,
            phase => {
                queued_units_page(&state, &job_id, phase == Some("resolved"), cursor, limit).await?
            }
        },
        // An unknown job lists nothing, as it always has.
        Some(QueueVersion::Legacy) | None => {
            legacy_repos_page(&state, &job_id, query.phase.as_deref(), cursor, limit).await?
        }
    };
    Ok(Json(page))
}

/// A legacy job's repo rows, keyset-paged by DID.
pub async fn legacy_repos_page(
    state: &JobState,
    job_id: &str,
    phase: Option<&str>,
    cursor: Option<&str>,
    limit: i32,
) -> Result<crate::backfill::types::BackfillReposResponse, AdminError> {
    let phase_filter = match phase {
        Some("resolved") => " AND pds_endpoint IS NOT NULL",
        Some("fetched") => " AND status = 'completed'",
        _ => "",
    };
    let cursor_filter = if cursor.is_some() { " AND did > ?" } else { "" };
    let sql = adapt_sql(
        &format!(
            "SELECT did, pds_endpoint, status, records_fetched FROM happyview_backfill_repos WHERE job_id = ?{phase_filter}{cursor_filter} ORDER BY did ASC LIMIT ?",
        ),
        state.db_backend,
    );
    let mut q = happyview_provider::db::query_as::<(String, Option<String>, String, i32)>(&sql)
        .bind(job_id);
    if let Some(cursor) = cursor {
        q = q.bind(cursor);
    }
    let rows = q
        .bind(limit + 1)
        .fetch_all(&state.backfill_db)
        .await
        .map_err(|e| AdminError::Internal(format!("failed to query backfill repos: {e}")))?;

    let has_more = rows.len() > limit as usize;
    let repos: Vec<crate::backfill::types::BackfillRepoEntry> = rows
        .into_iter()
        .take(limit as usize)
        .map(|(did, pds_endpoint, status, records_fetched)| {
            crate::backfill::types::BackfillRepoEntry {
                did,
                pds_endpoint,
                status,
                records_fetched,
            }
        })
        .collect();
    let cursor = if has_more {
        repos.last().map(|r| r.did.clone())
    } else {
        None
    };
    Ok(crate::backfill::types::BackfillReposResponse { repos, cursor })
}

/// A bounded job's recent completions, newest first, paged by log id.
pub async fn recent_completions_page(
    state: &JobState,
    job_id: &str,
    cursor: Option<&str>,
    limit: i32,
) -> Result<crate::backfill::types::BackfillReposResponse, AdminError> {
    let before = cursor
        .map(|c| {
            c.parse::<i64>()
                .map_err(|_| AdminError::BadRequest(format!("invalid cursor: {c}")))
        })
        .transpose()?;
    let sql = adapt_sql(
        &format!(
            "SELECT id, did, pds_endpoint, records_fetched FROM happyview_backfill_completions \
             WHERE job_id = ?{} ORDER BY id DESC LIMIT ?",
            if before.is_some() { " AND id < ?" } else { "" }
        ),
        state.db_backend,
    );
    let mut q = happyview_provider::db::query_as::<(i64, String, String, i32)>(&sql).bind(job_id);
    if let Some(id) = before {
        q = q.bind(id);
    }
    let rows = q
        .bind(limit + 1)
        .fetch_all(&state.backfill_db)
        .await
        .map_err(|e| AdminError::Internal(format!("failed to query backfill completions: {e}")))?;

    let has_more = rows.len() > limit as usize;
    let rows: Vec<_> = rows.into_iter().take(limit as usize).collect();
    let cursor = if has_more {
        rows.last().map(|(id, ..)| id.to_string())
    } else {
        None
    };
    let repos = rows
        .into_iter()
        .map(
            |(_, did, pds_endpoint, records_fetched)| crate::backfill::types::BackfillRepoEntry {
                did,
                pds_endpoint: Some(pds_endpoint),
                status: "completed".to_string(),
                records_fetched,
            },
        )
        .collect();
    Ok(crate::backfill::types::BackfillReposResponse { repos, cursor })
}

/// A bounded job's queued units in `(collection, did)` order, paged by a
/// `collection|did` cursor. Neither an NSID nor a DID contains `|`.
pub async fn queued_units_page(
    state: &JobState,
    job_id: &str,
    resolved_only: bool,
    cursor: Option<&str>,
    limit: i32,
) -> Result<crate::backfill::types::BackfillReposResponse, AdminError> {
    let after = cursor
        .map(|c| {
            c.split_once('|')
                .ok_or_else(|| AdminError::BadRequest(format!("invalid cursor: {c}")))
        })
        .transpose()?;
    let mut sql = String::from(
        "SELECT collection, did, pds_endpoint FROM happyview_backfill_queue WHERE job_id = ?",
    );
    if resolved_only {
        sql.push_str(" AND pds_endpoint IS NOT NULL");
    }
    if after.is_some() {
        sql.push_str(" AND (collection > ? OR (collection = ? AND did > ?))");
    }
    sql.push_str(" ORDER BY collection, did LIMIT ?");
    let sql = adapt_sql(&sql, state.db_backend);

    let mut q =
        happyview_provider::db::query_as::<(String, String, Option<String>)>(&sql).bind(job_id);
    if let Some((collection, did)) = after {
        q = q.bind(collection).bind(collection).bind(did);
    }
    let rows = q
        .bind(limit + 1)
        .fetch_all(&state.backfill_db)
        .await
        .map_err(|e| AdminError::Internal(format!("failed to query backfill queue: {e}")))?;

    let has_more = rows.len() > limit as usize;
    let rows: Vec<_> = rows.into_iter().take(limit as usize).collect();
    let cursor = if has_more {
        rows.last()
            .map(|(collection, did, _)| format!("{collection}|{did}"))
    } else {
        None
    };
    let repos = rows
        .into_iter()
        .map(
            |(_, did, pds_endpoint)| crate::backfill::types::BackfillRepoEntry {
                did,
                pds_endpoint,
                status: "pending".to_string(),
                records_fetched: 0,
            },
        )
        .collect();
    Ok(crate::backfill::types::BackfillReposResponse { repos, cursor })
}

/// `GET /admin/backfill/{id}/pds-summary`. A bounded job reads its per-PDS
/// counters; a legacy job aggregates its repo rows as before.
pub async fn backfill_pds_summary(
    State(state): State<JobState>,
    Path(job_id): Path<String>,
    AdminUser(auth): AdminUser,
) -> Result<Json<crate::backfill::types::PdsSummaryResponse>, AdminError> {
    auth.require("backfill:read")?;

    let bounded = job_queue_version(&state, &job_id).await? == Some(QueueVersion::Bounded);
    let sql = if bounded {
        "SELECT pds_endpoint, repos, completed_repos, records FROM happyview_backfill_pds_stats \
         WHERE job_id = ? ORDER BY repos DESC, pds_endpoint ASC"
    } else {
        "SELECT pds_endpoint, COUNT(*) as total_repos, SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END) as completed_repos, SUM(records_fetched) as total_records FROM happyview_backfill_repos WHERE job_id = ? AND pds_endpoint IS NOT NULL GROUP BY pds_endpoint ORDER BY COUNT(*) DESC"
    };
    let sql = adapt_sql(sql, state.db_backend);
    let rows: Vec<(String, i64, i64, i64)> = happyview_provider::db::query_as(&sql)
        .bind(&job_id)
        .fetch_all(&state.backfill_db)
        .await
        .map_err(|e| AdminError::Internal(format!("failed to query PDS summary: {e}")))?;

    let pds_endpoints = rows
        .into_iter()
        .map(
            |(pds_endpoint, total_repos, completed_repos, total_records)| {
                crate::backfill::types::PdsSummaryEntry {
                    pds_endpoint,
                    total_repos,
                    completed_repos,
                    total_records,
                }
            },
        )
        .collect();

    Ok(Json(crate::backfill::types::PdsSummaryResponse {
        pds_endpoints,
    }))
}

#[derive(Deserialize)]
pub struct BackfillErrorsQuery {
    pub kind: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<i32>,
}

/// GET /admin/backfill/{id}/errors — paginated failure detail plus exact
/// per-kind totals.
///
/// Named `..._list` rather than `backfill_errors`, which is already the SSE
/// stream handler's name.
pub async fn backfill_errors_list(
    State(state): State<JobState>,
    Path(job_id): Path<String>,
    AdminUser(auth): AdminUser,
    axum::extract::Query(query): axum::extract::Query<BackfillErrorsQuery>,
) -> Result<Json<BackfillErrorsResponse>, AdminError> {
    auth.require("backfill:read")?;

    // Clamped on both ends: SQLite reads a negative LIMIT as "no limit", which
    // would return the whole job's error set (up to ERROR_DETAIL_CAP rows) in
    // one response.
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let kind_filter = if query.kind.is_some() {
        " AND kind = ?"
    } else {
        ""
    };
    // Keyset pagination on `did` alone assumes at most one row per
    // (job_id, did) — the primary key is actually (job_id, did, phase), so a
    // DID with rows in two phases would let `did > ?` skip one at a page
    // boundary. That can't happen today: a DID that fails at `resolve` never
    // reaches `fetch`, and repeat `fetch` failures upsert into the same row.
    // That's a property of the current failure state machine, not of the
    // schema — a future change there could silently start skipping rows.
    let cursor_filter = if query.cursor.is_some() {
        " AND did > ?"
    } else {
        ""
    };

    let sql_str = format!(
        "SELECT did, collection, phase, kind, message, attempts, last_at \
         FROM happyview_backfill_errors WHERE job_id = ?{kind_filter}{cursor_filter} \
         ORDER BY did ASC LIMIT ?",
    );
    let sql = adapt_sql(&sql_str, state.db_backend);

    #[allow(clippy::type_complexity)]
    let mut q = happyview_provider::db::query_as::<(
        String,
        Option<String>,
        String,
        String,
        String,
        i32,
        String,
    )>(&sql)
    .bind(&job_id);
    if let Some(ref kind) = query.kind {
        q = q.bind(kind);
    }
    if let Some(ref cursor) = query.cursor {
        q = q.bind(cursor);
    }
    q = q.bind(limit + 1);

    #[allow(clippy::type_complexity)]
    let rows: Vec<(String, Option<String>, String, String, String, i32, String)> = q
        .fetch_all(&state.backfill_db)
        .await
        .map_err(|e| AdminError::Internal(format!("failed to query backfill errors: {e}")))?;

    let has_more = rows.len() > limit as usize;
    let errors: Vec<BackfillErrorEntry> = rows
        .into_iter()
        .take(limit as usize)
        .map(
            |(did, collection, phase, kind, message, attempts, last_at)| BackfillErrorEntry {
                did,
                collection,
                phase,
                kind,
                message,
                attempts,
                last_at,
            },
        )
        .collect();

    let cursor = if has_more {
        errors.last().map(|e| e.did.clone())
    } else {
        None
    };

    let error_counts = load_live_error_counts(&state, &job_id).await;
    let counts: Vec<BackfillErrorCount> = BackfillErrorKind::all()
        .into_iter()
        .filter_map(|kind| {
            let count = error_counts.get(kind);
            if count == 0 {
                return None;
            }
            Some(BackfillErrorCount {
                kind: kind.as_str().to_string(),
                count,
                retryable: kind.is_retryable(),
            })
        })
        .collect();
    let capped = error_counts.total() >= ERROR_DETAIL_CAP;

    Ok(Json(BackfillErrorsResponse {
        errors,
        cursor,
        counts,
        capped,
        cap: ERROR_DETAIL_CAP,
    }))
}

/// Per-kind counts for a job, reconciling the two partial views of them.
///
/// `error_counts` on the job row is only written by `ErrorRecorder::flush`, and
/// every flush site is terminal — so for the whole of a multi-hour backfill it
/// reads as empty and the dashboard shows no Errors row at all, and a crash
/// loses everything accumulated since the last flush while the detail rows it
/// summarises survive. Counting the detail table fixes both, and is exact below
/// `ERROR_DETAIL_CAP`. Above the cap rows stop being written and the JSON is the
/// only source left, so take whichever is larger per kind rather than either
/// one alone.
pub async fn load_live_error_counts(state: &JobState, job_id: &str) -> ErrorCounts {
    let mut counts = load_error_counts(state, job_id).await;

    let sql = adapt_sql(
        "SELECT kind, COUNT(*) FROM happyview_backfill_errors WHERE job_id = ? GROUP BY kind",
        state.db_backend,
    );
    let rows: Vec<(String, i64)> = happyview_provider::db::query_as(&sql)
        .bind(job_id)
        .fetch_all(&state.backfill_db)
        .await
        .unwrap_or_default();

    for (kind, count) in rows {
        // An unrecognised kind is skipped, not fatal — same rule as
        // `ErrorCounts::from_json`.
        if let Some(kind) = BackfillErrorKind::parse(&kind) {
            counts.raise_to(kind, count);
        }
    }

    counts
}

pub async fn load_error_counts(state: &JobState, job_id: &str) -> ErrorCounts {
    let sql = adapt_sql(
        "SELECT error_counts FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    );
    happyview_provider::db::query_as::<(Option<String>,)>(&sql)
        .bind(job_id)
        .fetch_optional(&state.backfill_db)
        .await
        .ok()
        .flatten()
        .and_then(|(json,)| json)
        .and_then(|json| serde_json::from_str(&json).ok())
        .map(|v| ErrorCounts::from_json(&v))
        .unwrap_or_default()
}

#[derive(Deserialize)]
pub struct RetryFailedBody {
    pub kinds: Option<Vec<String>>,
}

/// POST /admin/backfill/{id}/retry-failed — spawn a new job scoped to just
/// the failed DIDs from `job_id`.
///
/// A new job rather than mutating the finished one, seeded with
/// `stage = 'resolving_pds'` so `run_backfill_job` skips discovery (its DIDs
/// are already known) and enters the resolve/fetch pipeline directly.
pub async fn retry_failed_backfill(
    State(state): State<JobState>,
    AdminUser(admin): AdminUser,
    Path(job_id): Path<String>,
    Json(body): Json<RetryFailedBody>,
) -> Result<(StatusCode, Json<Value>), AdminError> {
    admin.require("backfill:create")?;
    let backend = state.db_backend;

    let sql = adapt_sql(
        "SELECT collection, source, scope FROM happyview_backfill_jobs WHERE id = ?",
        backend,
    );
    let row: Option<(Option<String>, String, String)> = happyview_provider::db::query_as(&sql)
        .bind(&job_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AdminError::Internal(format!("failed to query backfill job: {e}")))?;

    let Some((collection, source, scope)) = row else {
        return Err(AdminError::NotFound("backfill job not found".into()));
    };

    if source == "replay" {
        if let Some(requested) = &body.kinds
            && !requested.is_empty()
            && !requested.iter().any(|k| k == "replay_block")
        {
            return Err(AdminError::BadRequest(
                "no retryable failures for this job".into(),
            ));
        }
        let rows: Vec<(String, String)> = happyview_provider::db::query_as(&adapt_sql(
            "SELECT did, phase FROM happyview_backfill_errors WHERE job_id = ? AND kind = 'replay_block'",
            backend,
        ))
        .bind(&job_id)
        .fetch_all(&state.db)
        .await
        .map_err(|e| AdminError::Internal(format!("failed to read block failures: {e}")))?;
        let blocks: Vec<(String, i32)> = rows
            .into_iter()
            .filter_map(|(segment, phase)| match phase.as_str() {
                "replay_segment" => Some((segment, -1)),
                other => other
                    .strip_prefix("replay_block:")
                    .and_then(|i| i.parse().ok())
                    .map(|i| (segment, i)),
            })
            .collect();
        if blocks.is_empty() {
            return Err(AdminError::BadRequest(
                "no retryable failures for this job".into(),
            ));
        }
        let dids = if scope == "dids" {
            let dids = crate::backfill::replay_sweeps::job_dids(&state, &job_id)
                .await
                .map_err(|e| {
                    AdminError::Internal(format!("failed to read the job's accounts: {e}"))
                })?;
            // A finished job's queue rows go with a detail flush or the
            // retention sweep. Never widen an account job to the network.
            if dids.is_empty() {
                return Err(AdminError::BadRequest(DIDS_GONE.into()));
            }
            dids
        } else {
            Vec::new()
        };
        let new_job_id = create_backfill_job_with(
            &state,
            collection.as_deref(),
            &dids,
            crate::archive::SourceChoice::Replay,
        )
        .await?;
        crate::backfill::replay_sweeps::insert_retries(&state, &new_job_id, &blocks)
            .await
            .map_err(|e| {
                AdminError::Internal(format!("failed to record the blocks to retry: {e}"))
            })?;

        state
            .host
            .log_event(ProviderEvent {
                event_type: "backfill.retry_started".to_string(),
                severity: Severity::Info,
                actor_did: Some(admin.did.clone()),
                subject: collection.clone(),
                detail: serde_json::json!({
                    "job_id": new_job_id,
                    "source_job_id": job_id,
                    "source": "replay",
                    "blocks": blocks.len(),
                }),
            })
            .await;

        // Spawned only now, so the driver finds the retries.
        let spawn_state = state.clone();
        let spawn_job_id = new_job_id.clone();
        tokio::spawn(async move {
            run_backfill_job(spawn_state, spawn_job_id).await;
        });

        return Ok((
            StatusCode::CREATED,
            Json(serde_json::json!({ "id": new_job_id })),
        ));
    }

    let kinds: Vec<&'static str> = match &body.kinds {
        Some(requested) if !requested.is_empty() => requested
            .iter()
            .filter_map(|s| BackfillErrorKind::parse(s))
            .map(BackfillErrorKind::as_str)
            .collect(),
        _ => BackfillErrorKind::all()
            .into_iter()
            .filter(|k| k.is_retryable())
            .map(BackfillErrorKind::as_str)
            .collect(),
    };

    if kinds.is_empty() {
        return Err(AdminError::BadRequest(
            "no retryable failures for this job".into(),
        ));
    }

    let placeholders = vec!["?"; kinds.len()].join(", ");
    let sql_str = format!(
        "SELECT DISTINCT did FROM happyview_backfill_errors WHERE job_id = ? AND kind IN ({placeholders})",
    );
    let sql = adapt_sql(&sql_str, backend);
    let mut q = happyview_provider::db::query_as::<(String,)>(&sql).bind(&job_id);
    for kind in &kinds {
        q = q.bind(*kind);
    }
    let dids: Vec<(String,)> = q
        .fetch_all(&state.db)
        .await
        .map_err(|e| AdminError::Internal(format!("failed to query failed DIDs: {e}")))?;

    if dids.is_empty() {
        return Err(AdminError::BadRequest(
            "no retryable failures for this job".into(),
        ));
    }

    let dids: Vec<String> = dids.into_iter().map(|(did,)| did).collect();
    let new_job_id = Uuid::new_v4().to_string();
    insert_targeted_job(
        &state,
        &new_job_id,
        collection.as_deref(),
        &dids,
        SourceChoice::Crawl,
    )
    .await?;

    state
        .host
        .log_event(ProviderEvent {
            event_type: "backfill.retry_started".to_string(),
            severity: Severity::Info,
            actor_did: Some(admin.did.clone()),
            subject: collection.clone(),
            detail: serde_json::json!({
                "job_id": new_job_id,
                "source_job_id": job_id,
                "retried_repos": dids.len(),
            }),
        })
        .await;

    let spawn_state = state.clone();
    let spawn_job_id = new_job_id.clone();
    tokio::spawn(async move {
        run_backfill_job(spawn_state, spawn_job_id).await;
    });

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": new_job_id })),
    ))
}

/// `DELETE /admin/backfill/{id}/details`.
///
/// A bounded job is flushed only once it has finished: its queue is its
/// work, not detail, and a flush that took it from a running or paused job
/// would lose repos. A legacy job is flushed in any state, as it always was.
pub async fn flush_backfill_details(
    State(state): State<JobState>,
    Path(job_id): Path<String>,
    AdminUser(auth): AdminUser,
) -> Result<StatusCode, AdminError> {
    auth.require("backfill:create")?;

    let sql = adapt_sql(
        "SELECT status, queue_version FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    );
    let job: Option<(String, i32)> = happyview_provider::db::query_as(&sql)
        .bind(&job_id)
        .fetch_optional(&state.backfill_db)
        .await
        .map_err(|e| AdminError::Internal(format!("failed to query backfill job: {e}")))?;
    if let Some((status, version)) = job
        && QueueVersion::from_column(version) == QueueVersion::Bounded
        && !FINISHED_STATUSES.contains(&status.as_str())
    {
        return Err(AdminError::Conflict(format!(
            "job is {status}; only completed, cancelled or failed jobs can be flushed"
        )));
    }

    flush_job_details(&state, &job_id, BACKFILL_DELETE_BATCH).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /admin/backfill/details`: every detail and leftover queue row of
/// every finished job.
pub async fn flush_all_backfill_details(
    State(state): State<JobState>,
    AdminUser(auth): AdminUser,
) -> Result<StatusCode, AdminError> {
    auth.require("backfill:create")?;

    flush_finished_details(&state, BACKFILL_DELETE_BATCH).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

/// `/backfill*` and `/jetstream/status`, over `jobs`.
pub fn routes(jobs: JobState) -> axum::Router {
    router().with_state(jobs)
}

/// [`routes`] before it is given its jobs.
pub fn router() -> axum::Router<JobState> {
    axum::Router::new()
        .route("/backfill", post(create_backfill))
        .route("/backfill/status", get(backfill_status))
        .route("/backfill/details", delete(flush_all_backfill_details))
        .route("/backfill/{id}/cancel", post(cancel_backfill))
        .route("/backfill/{id}/pause", post(pause_backfill))
        .route("/backfill/{id}/resume", post(resume_backfill))
        .route("/backfill/{id}/events", get(backfill_events))
        .route("/backfill/{id}/repos", get(backfill_repos))
        .route("/backfill/{id}/pds-summary", get(backfill_pds_summary))
        .route("/backfill/{id}/errors", get(backfill_errors_list))
        .route("/backfill/{id}/retry-failed", post(retry_failed_backfill))
        .route("/backfill/{id}/details", delete(flush_backfill_details))
        .route("/jetstream/status", get(jetstream_status))
}

/// `GET /admin/jetstream/status`: what the live consumer is doing. The
/// platform principal may read it.
pub async fn jetstream_status(
    caller: AdminCaller,
) -> Result<Json<crate::consumer::status::JetstreamStatus>, AdminError> {
    caller.require("stats:read")?;
    Ok(Json(crate::consumer::status::snapshot()))
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// A failed admin request or job write, answered with the status and JSON
/// HappyView's own routes give.
#[derive(Debug)]
pub enum AdminError {
    BadRequest(String),
    Conflict(String),
    NotFound(String),
    Internal(String),
    Refused(AdminRefusal),
}

impl From<AdminRefusal> for AdminError {
    fn from(refusal: AdminRefusal) -> Self {
        AdminError::Refused(refusal)
    }
}

impl std::fmt::Display for AdminError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdminError::BadRequest(msg) => write!(f, "bad request: {msg}"),
            AdminError::Conflict(msg) => write!(f, "conflict: {msg}"),
            AdminError::NotFound(msg) => write!(f, "not found: {msg}"),
            AdminError::Internal(msg) => write!(f, "internal error: {msg}"),
            AdminError::Refused(AdminRefusal::MissingPermission(permission)) => {
                write!(f, "Missing permission: {permission}")
            }
            AdminError::Refused(AdminRefusal::Platform) => {
                write!(f, "forbidden: the platform key cannot use this endpoint")
            }
        }
    }
}

impl std::error::Error for AdminError {}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            AdminError::Refused(refusal) => return refusal.into_response(),
            AdminError::Internal(msg) => {
                // Never leak internal details (SQL/driver errors) to clients.
                // Log the real message with a correlation id and return only
                // that id, so an operator can find it in the logs.
                let correlation_id = format!("{:016x}", rand::random::<u64>());
                tracing::error!(correlation_id, "internal error: {msg}");
                let body = serde_json::json!({
                    "error": "Internal server error",
                    "correlationId": correlation_id,
                });
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response();
            }
            AdminError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            AdminError::Conflict(msg) => (StatusCode::CONFLICT, msg),
            AdminError::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
        };
        (status, Json(serde_json::json!({ "error": message }))).into_response()
    }
}
