use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use futures_util::FutureExt;
use futures_util::stream::{self, FuturesUnordered, StreamExt};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use rand::RngExt;

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};
use crate::http_retry::parse_retry_after;
use crate::profile;

use super::auth::UserAuth;
use super::backfill_errors::{BackfillErrorKind, ERROR_DETAIL_CAP, ErrorCounts};
use super::backfill_retry::{
    DeferredItem, DeferredQueue, DrainStep, HostCooldowns, next_drain_step,
};
use super::permissions::Permission;
use super::types::{
    BackfillErrorCount, BackfillErrorEntry, BackfillErrorsResponse, BackfillJob, CreateBackfillBody,
};

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ListReposResponse {
    repos: Vec<RepoEntry>,
    cursor: Option<String>,
}

#[derive(Deserialize)]
struct RepoEntry {
    did: String,
}

#[derive(Deserialize)]
struct ListRecordsResponse {
    records: Vec<RecordEntry>,
    cursor: Option<String>,
}

#[derive(Deserialize)]
struct RecordEntry {
    uri: String,
    cid: String,
    value: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn set_stage(state: &AppState, job_id: &str, stage: &str) {
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET stage = ? WHERE id = ?",
        state.db_backend,
    );
    let _ = crate::db::query(&sql)
        .bind(stage)
        .bind(job_id)
        .execute(&state.backfill_db)
        .await;
    publish_event(
        state,
        super::types::BackfillEvent::JobStageChanged {
            job_id: job_id.to_string(),
            stage: stage.to_string(),
        },
    );
}

async fn update_job_counter(state: &AppState, job_id: &str, column: &str, value: i32) {
    let query = match column {
        "total_repos" => "UPDATE happyview_backfill_jobs SET total_repos = ? WHERE id = ?",
        "resolved_repos" => "UPDATE happyview_backfill_jobs SET resolved_repos = ? WHERE id = ?",
        "processed_repos" => "UPDATE happyview_backfill_jobs SET processed_repos = ? WHERE id = ?",
        "total_records" => "UPDATE happyview_backfill_jobs SET total_records = ? WHERE id = ?",
        other => {
            tracing::error!(
                column = other,
                "update_job_counter called with unknown column"
            );
            return;
        }
    };
    let sql = adapt_sql(query, state.db_backend);
    let _ = crate::db::query(&sql)
        .bind(value)
        .bind(job_id)
        .execute(&state.backfill_db)
        .await;
}

async fn count_repos(state: &AppState, job_id: &str) -> i32 {
    let sql = adapt_sql(
        "SELECT COUNT(*) FROM happyview_backfill_repos WHERE job_id = ?",
        state.db_backend,
    );
    crate::db::query_as::<(i32,)>(&sql)
        .bind(job_id)
        .fetch_one(&state.backfill_db)
        .await
        .map(|(c,)| c)
        .unwrap_or(0)
}

fn publish_event(state: &AppState, event: super::types::BackfillEvent) {
    let _ = state.backfill_events_tx.send(event);
}

/// Current job state, straight from the database.
///
/// The SSE stream is otherwise delta-only over a lossy broadcast channel, so a
/// client that connects mid-phase or misses an event has no way to recover.
/// A snapshot is how it resyncs.
async fn build_job_snapshot(state: &AppState, job_id: &str) -> Option<super::types::BackfillEvent> {
    let sql = adapt_sql(
        "SELECT status, stage, total_repos, resolved_repos, processed_repos, total_records, error_counts \
         FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    );
    #[allow(clippy::type_complexity)]
    let row: Option<(
        String,
        String,
        Option<i32>,
        Option<i32>,
        Option<i32>,
        Option<i32>,
        Option<String>,
    )> = crate::db::query_as(&sql)
        .bind(job_id)
        .fetch_optional(&state.backfill_db)
        .await
        .ok()
        .flatten();

    let (status, stage, total_repos, resolved_repos, processed_repos, total_records, error_counts) =
        row?;

    Some(super::types::BackfillEvent::JobSnapshot {
        job_id: job_id.to_string(),
        status,
        stage,
        total_repos,
        resolved_repos,
        processed_repos,
        total_records,
        error_counts: error_counts
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({})),
    })
}

fn random_batch_threshold(base: i32) -> i32 {
    let low = base - base / 10;
    rand::rng().random_range(low..=base)
}

struct BackfillConcurrency {
    resolution: usize,
    pds: usize,
    dids_per_pds: usize,
}

async fn load_concurrency(state: &AppState) -> BackfillConcurrency {
    let resolution = super::settings::get_setting(
        &state.db,
        "backfill_concurrent_resolution",
        state.db_backend,
    )
    .await
    .and_then(|v| v.parse().ok())
    .unwrap_or(100usize)
    .max(1);
    let pds = super::settings::get_setting(&state.db, "backfill_concurrent_pds", state.db_backend)
        .await
        .and_then(|v| v.parse().ok())
        .unwrap_or(10usize)
        .max(1);
    let dids_per_pds = super::settings::get_setting(
        &state.db,
        "backfill_concurrent_dids_per_pds",
        state.db_backend,
    )
    .await
    .and_then(|v| v.parse().ok())
    .unwrap_or(3usize)
    .max(1);
    BackfillConcurrency {
        resolution,
        pds,
        dids_per_pds,
    }
}

async fn load_max_attempts(state: &AppState) -> u32 {
    super::settings::get_setting(&state.db, "backfill_max_attempts", state.db_backend)
        .await
        .and_then(|v| v.parse().ok())
        .unwrap_or(3u32)
        .clamp(1, 10)
}

async fn fail_job(state: &AppState, job_id: &str, error: &str) {
    let now = now_rfc3339();
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET status = 'failed', completed_at = ?, error = ? WHERE id = ?",
        state.db_backend,
    );
    let _ = crate::db::query(&sql)
        .bind(&now)
        .bind(error)
        .bind(job_id)
        .execute(&state.backfill_db)
        .await;
    publish_event(
        state,
        super::types::BackfillEvent::JobCompleted {
            job_id: job_id.to_string(),
            status: "failed".to_string(),
            error: Some(error.to_string()),
        },
    );
}

async fn should_stop(state: &AppState, job_id: &str) -> Option<&'static str> {
    let sql = adapt_sql(
        "SELECT status FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    );
    let status = crate::db::query_as::<(String,)>(&sql)
        .bind(job_id)
        .fetch_optional(&state.backfill_db)
        .await
        .ok()
        .flatten()
        .map(|(s,)| s);
    match status.as_deref() {
        Some("cancelling") => Some("cancelling"),
        Some("pausing") => Some("pausing"),
        _ => None,
    }
}

async fn should_stop_worker(state: &AppState, job_id: &str) -> bool {
    should_stop(state, job_id).await.is_some()
}

async fn request_cancel(state: &AppState, job_id: &str) {
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET status = 'cancelling' WHERE id = ? AND status IN ('running', 'paused')",
        state.db_backend,
    );
    let _ = crate::db::query(&sql)
        .bind(job_id)
        .execute(&state.backfill_db)
        .await;
}

async fn finalise_cancel(state: &AppState, job_id: &str) {
    let now = now_rfc3339();
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET status = 'cancelled', completed_at = ?, error = 'cancelled by user' WHERE id = ?",
        state.db_backend,
    );
    let _ = crate::db::query(&sql)
        .bind(&now)
        .bind(job_id)
        .execute(&state.backfill_db)
        .await;
    publish_event(
        state,
        super::types::BackfillEvent::JobCompleted {
            job_id: job_id.to_string(),
            status: "cancelled".to_string(),
            error: None,
        },
    );
}

async fn request_pause(state: &AppState, job_id: &str) {
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET status = 'pausing' WHERE id = ? AND status = 'running'",
        state.db_backend,
    );
    let _ = crate::db::query(&sql)
        .bind(job_id)
        .execute(&state.backfill_db)
        .await;
}

async fn finalise_pause(state: &AppState, job_id: &str) {
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET status = 'paused' WHERE id = ?",
        state.db_backend,
    );
    let _ = crate::db::query(&sql)
        .bind(job_id)
        .execute(&state.backfill_db)
        .await;
    publish_event(
        state,
        super::types::BackfillEvent::JobCompleted {
            job_id: job_id.to_string(),
            status: "paused".to_string(),
            error: None,
        },
    );
}

async fn complete_job(
    state: &AppState,
    job_id: &str,
    processed_repos: i32,
    total_records: i32,
    error: Option<&str>,
) {
    let now = now_rfc3339();
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET status = 'completed', stage = 'completed', completed_at = ?, processed_repos = ?, total_records = ?, error = ? WHERE id = ?",
        state.db_backend,
    );
    let _ = crate::db::query(&sql)
        .bind(&now)
        .bind(processed_repos)
        .bind(total_records)
        .bind(error)
        .bind(job_id)
        .execute(&state.backfill_db)
        .await;
    publish_event(
        state,
        super::types::BackfillEvent::JobCompleted {
            job_id: job_id.to_string(),
            status: "completed".to_string(),
            error: error.map(|e| e.to_string()),
        },
    );
}

// ---------------------------------------------------------------------------
// Phase 1: Discover repos via relay
// ---------------------------------------------------------------------------

async fn run_discovery_phase(
    state: &AppState,
    job_id: &str,
    collections: &[String],
    specific_did: Option<&str>,
) {
    set_stage(state, job_id, "discovering_repos").await;

    if let Some(did) = specific_did {
        let sql = adapt_sql(
            "INSERT INTO happyview_backfill_repos (job_id, did) VALUES (?, ?) ON CONFLICT DO NOTHING",
            state.db_backend,
        );
        let _ = crate::db::query(&sql)
            .bind(job_id)
            .bind(did)
            .execute(&state.backfill_db)
            .await;
        publish_event(
            state,
            super::types::BackfillEvent::RepoDiscovered {
                job_id: job_id.to_string(),
                did: did.to_string(),
            },
        );
    } else {
        stream::iter(collections.iter())
            .for_each_concurrent(5, |collection| async move {
                if should_stop_worker(state, job_id).await {
                    return;
                }
                if let Err(e) = discover_repos_from_relay(state, job_id, collection).await {
                    tracing::warn!(collection, error = %e, "failed to discover repos, skipping");
                }
            })
            .await;
    }

    let total = count_repos(state, job_id).await;
    update_job_counter(state, job_id, "total_repos", total).await;
    publish_event(
        state,
        super::types::BackfillEvent::JobCounters {
            job_id: job_id.to_string(),
            total_repos: Some(total),
            resolved_repos: None,
            processed_repos: None,
            total_records: None,
        },
    );
}

async fn discover_repos_from_relay(
    state: &AppState,
    job_id: &str,
    collection: &str,
) -> Result<(), String> {
    let base = state.config.relay_url.trim_end_matches('/');
    let mut cursor: Option<String> = None;
    let mut running_total: i32 = count_repos(state, job_id).await;

    loop {
        let mut url = format!(
            "{base}/xrpc/com.atproto.sync.listReposByCollection?collection={collection}&limit=1000"
        );
        if let Some(ref c) = cursor {
            url.push_str(&format!("&cursor={c}"));
        }

        let resp = loop {
            let r = state
                .http
                .get(&url)
                .send()
                .await
                .map_err(|e| format!("relay request failed: {e}"))?;

            if r.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let wait = parse_retry_after(r.headers());
                tracing::warn!(collection, wait, "rate limited by relay, sleeping");
                tokio::time::sleep(tokio::time::Duration::from_secs(wait)).await;
                continue;
            }

            break r;
        };

        if !resp.status().is_success() {
            return Err(format!("relay returned {}", resp.status()));
        }

        let body: ListReposResponse = resp
            .json()
            .await
            .map_err(|e| format!("invalid relay response: {e}"))?;

        let page_count = body.repos.len();

        if !body.repos.is_empty() {
            // SQLite has a 999 bound-parameter limit; each row uses 2 params
            let chunk_size = if state.db_backend == crate::db::DatabaseBackend::Sqlite {
                499
            } else {
                1000
            };

            for chunk in body.repos.chunks(chunk_size) {
                let base_sql = "INSERT INTO happyview_backfill_repos (job_id, did) VALUES ";
                let placeholders: Vec<String> = chunk
                    .iter()
                    .enumerate()
                    .map(|(i, _)| {
                        if state.db_backend == crate::db::DatabaseBackend::Postgres {
                            format!("(${}, ${})", i * 2 + 1, i * 2 + 2)
                        } else {
                            "(?, ?)".to_string()
                        }
                    })
                    .collect();
                let sql = format!(
                    "{base_sql}{} ON CONFLICT DO NOTHING",
                    placeholders.join(", ")
                );

                let mut query = crate::db::query(&sql);
                for repo in chunk {
                    query = query.bind(job_id).bind(&repo.did);
                }
                if let Ok(result) = query.execute(&state.backfill_db).await {
                    running_total += result.rows_affected() as i32;
                }
            }
        }

        update_job_counter(state, job_id, "total_repos", running_total).await;
        publish_event(
            state,
            super::types::BackfillEvent::JobCounters {
                job_id: job_id.to_string(),
                total_repos: Some(running_total),
                resolved_repos: None,
                processed_repos: None,
                total_records: None,
            },
        );

        if should_stop_worker(state, job_id).await {
            return Ok(());
        }

        match body.cursor {
            Some(c) if page_count > 0 => cursor = Some(c),
            _ => break,
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Pipelined Phase 2+3: Resolve PDS endpoints and fetch records concurrently
// ---------------------------------------------------------------------------

/// Finish resolving one DID: persist its PDS endpoint, publish the resolved
/// event, bump the resolved-repo counter (flushing to the DB on schedule),
/// and hand the `(did, pds)` pair to the fetcher.
///
/// Shared by the resolver's primary stream and its deferred-retry drain so
/// both paths do exactly the same thing on success, rather than the drain
/// pass repeating this by hand.
///
/// Returns `false` once `tx_resolver` has closed — the fetcher already
/// exited, so there is nothing left to resolve for.
#[allow(clippy::too_many_arguments)]
async fn on_resolved(
    resolver_state: &AppState,
    resolver_job_id: &str,
    did: String,
    pds: String,
    resolver_resolved: &AtomicI32,
    next_flush: &mut i32,
    tx_resolver: &mpsc::Sender<(String, String)>,
) -> bool {
    let sql = adapt_sql(
        "UPDATE happyview_backfill_repos SET pds_endpoint = ? WHERE job_id = ? AND did = ?",
        resolver_state.db_backend,
    );
    let _ = crate::db::query(&sql)
        .bind(&pds)
        .bind(resolver_job_id)
        .bind(&did)
        .execute(&resolver_state.backfill_db)
        .await;

    publish_event(
        resolver_state,
        super::types::BackfillEvent::RepoResolved {
            job_id: resolver_job_id.to_string(),
            did: did.clone(),
            pds_endpoint: pds.clone(),
        },
    );

    let count = resolver_resolved.fetch_add(1, Ordering::Relaxed) + 1;
    if count >= *next_flush {
        update_job_counter(resolver_state, resolver_job_id, "resolved_repos", count).await;
        *next_flush = count + random_batch_threshold(100);
    }
    publish_event(
        resolver_state,
        super::types::BackfillEvent::JobCounters {
            job_id: resolver_job_id.to_string(),
            total_repos: None,
            resolved_repos: Some(count),
            processed_repos: None,
            total_records: None,
        },
    );

    tx_resolver.send((did, pds)).await.is_ok()
}

async fn run_pipelined_resolve_and_fetch(
    state: &AppState,
    job_id: &str,
    collections: &[String],
    concurrency: &BackfillConcurrency,
) -> (i32, i32) {
    set_stage(state, job_id, "resolving_and_fetching").await;

    // Count already-resolved and already-completed repos for accurate progress
    let already_resolved: i32 = {
        let sql = adapt_sql(
            "SELECT COUNT(*) FROM happyview_backfill_repos WHERE job_id = ? AND pds_endpoint IS NOT NULL",
            state.db_backend,
        );
        crate::db::query_as::<(i32,)>(&sql)
            .bind(job_id)
            .fetch_one(&state.backfill_db)
            .await
            .map(|(c,)| c)
            .unwrap_or(0)
    };

    let already_completed: i32 = {
        let sql = adapt_sql(
            "SELECT COUNT(*) FROM happyview_backfill_repos WHERE job_id = ? AND status = 'completed'",
            state.db_backend,
        );
        crate::db::query_as::<(i32,)>(&sql)
            .bind(job_id)
            .fetch_one(&state.backfill_db)
            .await
            .map(|(c,)| c)
            .unwrap_or(0)
    };

    update_job_counter(state, job_id, "resolved_repos", already_resolved).await;
    update_job_counter(state, job_id, "processed_repos", already_completed).await;

    let existing_records: i32 = {
        let sql = adapt_sql(
            "SELECT total_records FROM happyview_backfill_jobs WHERE id = ?",
            state.db_backend,
        );
        crate::db::query_as::<(Option<i32>,)>(&sql)
            .bind(job_id)
            .fetch_one(&state.backfill_db)
            .await
            .map(|(c,)| c.unwrap_or(0))
            .unwrap_or(0)
    };

    // Shared atomics for lock-free counter updates
    let resolved_repos = Arc::new(AtomicI32::new(already_resolved));
    let processed_repos = Arc::new(AtomicI32::new(already_completed));
    let total_records = Arc::new(AtomicI32::new(existing_records));
    let cancelled = Arc::new(AtomicBool::new(false));

    let (tx, mut rx) = mpsc::channel::<(String, String)>(256);
    let tx_resolver = tx.clone();
    let tx_backlog = tx.clone();

    // One error sink for the whole job, shared by every phase and (once Task 7
    // lands) every per-PDS worker — see `ErrorRecorder`'s doc comment for why
    // it must not be constructed per-worker.
    let recorder = Arc::new(super::backfill_errors::ErrorRecorder::new(state, job_id).await);

    // --- Resolver task ---
    let resolution_concurrency = concurrency.resolution;
    let resolver_state = state.clone();
    let resolver_job_id = job_id.to_string();
    let resolver_resolved = Arc::clone(&resolved_repos);
    let resolver_cancelled = Arc::clone(&cancelled);
    let resolver_recorder = Arc::clone(&recorder);

    let resolver_handle = tokio::spawn(async move {
        let sql = adapt_sql(
            "SELECT did FROM happyview_backfill_repos WHERE job_id = ? AND pds_endpoint IS NULL",
            resolver_state.db_backend,
        );
        let unresolved: Vec<(String,)> = crate::db::query_as(&sql)
            .bind(&resolver_job_id)
            .fetch_all(&resolver_state.backfill_db)
            .await
            .unwrap_or_default();

        let mut attempted: i32 = 0;
        let mut next_flush = random_batch_threshold(100);
        let mut next_cancel_check = random_batch_threshold(10);
        let max_attempts = load_max_attempts(&resolver_state).await;
        // Local to this task, not job-wide state: every worker Task 7 spawns
        // gets its own cooldowns and queue, keyed to the PDS host(s) it alone
        // talks to.
        let mut cooldowns = HostCooldowns::new();
        let mut deferred: DeferredQueue<String> = DeferredQueue::new();

        let stream_state = resolver_state.clone();
        let stream_cancelled = Arc::clone(&resolver_cancelled);
        let mut results = stream::iter(unresolved)
            .map(move |(did,)| {
                let state = stream_state.clone();
                let cancelled = Arc::clone(&stream_cancelled);
                async move {
                    if cancelled.load(Ordering::Relaxed) {
                        return None;
                    }
                    let result = profile::resolve_pds_endpoint_once(
                        &state.http,
                        &state.config.plc_url,
                        &did,
                    )
                    .await;
                    Some((did, result))
                }
            })
            .buffer_unordered(resolution_concurrency);

        while let Some(item) = results.next().await {
            let Some((did, result)) = item else {
                break;
            };

            match result {
                Ok(pds) => {
                    let host = profile::did_doc_host(&resolver_state.config.plc_url, &did);
                    cooldowns.record_success(&host);
                    if !on_resolved(
                        &resolver_state,
                        &resolver_job_id,
                        did,
                        pds,
                        &resolver_resolved,
                        &mut next_flush,
                        &tx_resolver,
                    )
                    .await
                    {
                        tracing::warn!(
                            job_id = %resolver_job_id,
                            deferred_queued = deferred.len(),
                            "fetcher channel closed while resolving; abandoning the \
                             remaining resolved DIDs — they will not be fetched, \
                             counted, or recorded as errors"
                        );
                        break;
                    }
                }
                Err(failure) => {
                    let host = profile::did_doc_host(&resolver_state.config.plc_url, &did);
                    let now = std::time::Instant::now();
                    let attempts = 1;

                    if failure.kind.is_retryable() && attempts < max_attempts {
                        cooldowns.record_failure(&host, failure.retry_after, now);
                        deferred.push(DeferredItem {
                            payload: did.clone(),
                            host,
                            attempts,
                            eligible_at: now,
                        });
                    } else {
                        resolver_recorder
                            .record(
                                &resolver_state,
                                &resolver_job_id,
                                &did,
                                None,
                                "resolve",
                                &failure,
                                attempts,
                            )
                            .await;
                        tracing::warn!(
                            did,
                            kind = failure.kind.as_str(),
                            attempts,
                            "giving up resolving PDS endpoint: {}",
                            failure.message
                        );
                    }
                }
            }

            attempted += 1;
            if attempted >= next_cancel_check {
                if should_stop_worker(&resolver_state, &resolver_job_id).await {
                    resolver_cancelled.store(true, Ordering::Relaxed);
                    break;
                }
                next_cancel_check = attempted + random_batch_threshold(10);
            }
        }

        // Deferred pass. The primary stream is exhausted, so anything still
        // here is waiting on a clock rather than on work.
        loop {
            if resolver_cancelled.load(Ordering::Relaxed) {
                break;
            }
            match next_drain_step(&mut deferred, &cooldowns, Duration::from_secs(2)).await {
                DrainStep::Done => break,
                DrainStep::Slept => {
                    if should_stop_worker(&resolver_state, &resolver_job_id).await {
                        resolver_cancelled.store(true, Ordering::Relaxed);
                        break;
                    }
                }
                DrainStep::Retry(item) => {
                    let attempts = item.attempts + 1;
                    match profile::resolve_pds_endpoint_once(
                        &resolver_state.http,
                        &resolver_state.config.plc_url,
                        &item.payload,
                    )
                    .await
                    {
                        Ok(pds) => {
                            cooldowns.record_success(&item.host);
                            // Same path as the primary Ok arm.
                            if !on_resolved(
                                &resolver_state,
                                &resolver_job_id,
                                item.payload,
                                pds,
                                &resolver_resolved,
                                &mut next_flush,
                                &tx_resolver,
                            )
                            .await
                            {
                                tracing::warn!(
                                    job_id = %resolver_job_id,
                                    deferred_queued = deferred.len(),
                                    "fetcher channel closed while draining deferred \
                                     resolutions; abandoning the remaining queued DIDs \
                                     — they will not be fetched, counted, or recorded \
                                     as errors"
                                );
                                break;
                            }
                        }
                        Err(failure) => {
                            let now = std::time::Instant::now();
                            if failure.kind.is_retryable() && attempts < max_attempts {
                                let host = item.host.clone();
                                cooldowns.record_failure(&host, failure.retry_after, now);
                                deferred.push(DeferredItem {
                                    attempts,
                                    eligible_at: now,
                                    ..item
                                });
                                // A host that has stopped answering must not be
                                // re-asked once per cooldown for every DID
                                // behind it — that turns a bounded drain into a
                                // multi-day one. Declare it down and record the
                                // whole queue at once, so every DID still lands
                                // in `backfill_errors` with the right kind.
                                if cooldowns.is_saturated(&host) {
                                    let abandoned = deferred.drain_host(&host);
                                    tracing::warn!(
                                        host,
                                        abandoned = abandoned.len(),
                                        kind = failure.kind.as_str(),
                                        "host failed {} times consecutively; giving up on \
                                         its remaining deferred resolutions: {}",
                                        cooldowns.consecutive_failures(&host),
                                        failure.message
                                    );
                                    for item in abandoned {
                                        resolver_recorder
                                            .record(
                                                &resolver_state,
                                                &resolver_job_id,
                                                &item.payload,
                                                None,
                                                "resolve",
                                                &failure,
                                                item.attempts,
                                            )
                                            .await;
                                    }
                                }
                            } else {
                                resolver_recorder
                                    .record(
                                        &resolver_state,
                                        &resolver_job_id,
                                        &item.payload,
                                        None,
                                        "resolve",
                                        &failure,
                                        attempts,
                                    )
                                    .await;
                            }
                        }
                    }
                }
            }
        }

        // Persist final resolved count
        let final_resolved = resolver_resolved.load(Ordering::Relaxed);
        update_job_counter(
            &resolver_state,
            &resolver_job_id,
            "resolved_repos",
            final_resolved,
        )
        .await;
        resolver_recorder
            .flush(&resolver_state, &resolver_job_id)
            .await;
        // tx_resolver is dropped here, at the end of this task's async block —
        // only now does the fetcher learn that no more DIDs are coming, which
        // is why the deferred pass above must finish before this point.
    });

    // --- Also send already-resolved-but-unfetched DIDs to the fetcher ---
    let pending_sql = adapt_sql(
        "SELECT did, pds_endpoint FROM happyview_backfill_repos WHERE job_id = ? AND status = 'pending' AND pds_endpoint IS NOT NULL",
        state.db_backend,
    );
    let pending_rows: Vec<(String, String)> = crate::db::query_as(&pending_sql)
        .bind(job_id)
        .fetch_all(&state.backfill_db)
        .await
        .unwrap_or_default();

    let backlog_cancelled = Arc::clone(&cancelled);
    let backlog_handle = tokio::spawn(async move {
        for (did, pds) in pending_rows {
            if backlog_cancelled.load(Ordering::Relaxed) {
                break;
            }
            if tx_backlog.send((did, pds)).await.is_err() {
                break;
            }
        }
    });

    // Drop our copy of tx so the channel closes when both senders finish
    drop(tx);

    // --- Fetcher: receive (did, pds) pairs and dispatch to PDS workers ---
    // Each PDS gets its own worker with a DID channel, and every worker starts
    // immediately — see `FetchContext::requests` for why gating startup on a
    // semaphore deadlocks the job. Concurrency is capped on in-flight requests
    // instead. We never hold the workers lock across an `.await` — use
    // `try_send` to avoid blocking when a worker's channel is full (overflow
    // goes to a retry queue drained on each iteration).
    let state = Arc::new(state.clone());
    let collections = Arc::new(collections.to_vec());
    let job_id_arc = Arc::new(job_id.to_string());

    // Derived from the two existing settings rather than introduced as a third,
    // so every deployment keeps the effective concurrency it has today: the old
    // scheme allowed `pds` workers each with `dids_per_pds` fetches in flight.
    // The difference is that those requests are no longer confined to `pds`
    // endpoints — they spread across every PDS in the job, which is what stops
    // a handful of hosts absorbing the whole rate-limit budget while the rest
    // sit idle.
    let request_limit = concurrency
        .pds
        .saturating_mul(concurrency.dids_per_pds)
        .max(1);
    let pds_semaphore = Arc::new(tokio::sync::Semaphore::new(request_limit));
    let mut pds_workers: HashMap<String, mpsc::Sender<String>> = HashMap::new();
    let mut worker_handles = FuturesUnordered::new();
    let mut overflow: Vec<(String, String)> = Vec::new();

    loop {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }

        let poll_state = Arc::clone(&state);
        let poll_job_id = Arc::clone(&job_id_arc);
        let poll_cancelled = Arc::clone(&cancelled);
        let pair = tokio::select! {
            result = rx.recv() => result,
            _ = async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    if poll_cancelled.load(Ordering::Relaxed) || should_stop_worker(&poll_state, &poll_job_id).await {
                        poll_cancelled.store(true, Ordering::Relaxed);
                        return;
                    }
                }
            } => None,
        };
        let Some((did, pds_endpoint)) = pair else {
            break;
        };

        // Also drain any overflow from previous iterations
        overflow.push((did, pds_endpoint));

        let mut still_pending = Vec::new();
        for (did, pds_endpoint) in overflow.drain(..) {
            if cancelled.load(Ordering::Relaxed) {
                break;
            }

            // Try to send to an existing PDS worker
            if let Some(pds_tx) = pds_workers.get(&pds_endpoint) {
                match pds_tx.try_send(did.clone()) {
                    Ok(()) => continue,
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        still_pending.push((did, pds_endpoint));
                        continue;
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        // Worker finished, will be removed below
                    }
                }
            }

            // Remove stale workers whose channels have closed
            pds_workers.retain(|_, tx| !tx.is_closed());

            // Spawn a new PDS worker. It starts consuming immediately; only
            // its requests are capped.
            let requests = Arc::clone(&pds_semaphore);
            let (pds_tx, pds_rx) = mpsc::channel::<String>(64);
            let _ = pds_tx.try_send(did);
            pds_workers.insert(pds_endpoint.clone(), pds_tx);

            let ctx = FetchContext {
                state: Arc::clone(&state),
                job_id: Arc::clone(&job_id_arc),
                collections: Arc::clone(&collections),
                processed_repos: Arc::clone(&processed_repos),
                total_records: Arc::clone(&total_records),
                cancelled: Arc::clone(&cancelled),
                dids_per_pds: concurrency.dids_per_pds,
                recorder: Arc::clone(&recorder),
                requests,
            };

            worker_handles.push(tokio::spawn(async move {
                run_pds_worker(ctx, pds_endpoint, pds_rx).await;
            }));
        }
        overflow = still_pending;

        // Drain any completed worker handles to avoid unbounded accumulation
        while let Some(result) = worker_handles.next().now_or_never() {
            if let Some(Err(e)) = result {
                tracing::warn!(error = %e, "PDS worker task panicked");
            }
        }
    }

    // Drain remaining overflow after channel closes
    for (did, pds_endpoint) in overflow.drain(..) {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }

        // Remove stale workers
        pds_workers.retain(|_, tx| !tx.is_closed());

        if let Some(pds_tx) = pds_workers.get(&pds_endpoint) {
            // Bounded channel, so this can block — but only until the worker
            // consumes, and every worker in the map is running (startup is no
            // longer gated on a permit). When it *was* gated, a worker parked
            // waiting for a permit could never drain this queue, this send
            // blocked forever, `pds_workers` was never dropped, and no running
            // worker could exit to release a permit: the job deadlocked.
            let _ = pds_tx.send(did).await;
            continue;
        }

        let requests = Arc::clone(&pds_semaphore);
        let (pds_tx, pds_rx) = mpsc::channel::<String>(64);
        let _ = pds_tx.try_send(did);
        pds_workers.insert(pds_endpoint.clone(), pds_tx);

        let ctx = FetchContext {
            state: Arc::clone(&state),
            job_id: Arc::clone(&job_id_arc),
            collections: Arc::clone(&collections),
            processed_repos: Arc::clone(&processed_repos),
            total_records: Arc::clone(&total_records),
            cancelled: Arc::clone(&cancelled),
            dids_per_pds: concurrency.dids_per_pds,
            recorder: Arc::clone(&recorder),
            requests,
        };

        worker_handles.push(tokio::spawn(async move {
            run_pds_worker(ctx, pds_endpoint.clone(), pds_rx).await;
        }));
    }

    // Drop all PDS senders so workers know no more DIDs are coming
    drop(pds_workers);

    // Wait for all PDS workers to finish
    while let Some(result) = worker_handles.next().await {
        if let Err(e) = result {
            tracing::warn!(error = %e, "PDS worker task panicked");
        }
    }

    // Wait for resolver and backlog tasks
    let _ = resolver_handle.await;
    let _ = backlog_handle.await;

    // Flush again now that every PDS worker (and the resolver) has finished.
    // The resolver already flushed once inside its own task when resolution
    // finished, but that predates most of the fetch phase's give-ups —
    // fetching is the long pole, so flushing only there left `error_counts`
    // frozen at a resolve-only snapshot. This must come after both joins
    // above: flushing earlier could race the resolver's own flush and get
    // overwritten by its older counts.
    recorder.flush(&state, job_id).await;

    let final_repos = processed_repos.load(Ordering::Relaxed);
    let final_records = total_records.load(Ordering::Relaxed);

    // Persist final counts
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET processed_repos = ?, total_records = ? WHERE id = ?",
        state.db_backend,
    );
    let _ = crate::db::query(&sql)
        .bind(final_repos)
        .bind(final_records)
        .bind(job_id)
        .execute(&state.backfill_db)
        .await;

    (final_repos, final_records)
}

struct FetchContext {
    state: Arc<AppState>,
    job_id: Arc<String>,
    collections: Arc<Vec<String>>,
    processed_repos: Arc<AtomicI32>,
    total_records: Arc<AtomicI32>,
    cancelled: Arc<AtomicBool>,
    dids_per_pds: usize,
    // Shared with every other PDS worker for this job — see the doc comment
    // on `recorder`'s construction in `run_pipelined_resolve_and_fetch` for
    // why this must be a clone, never a fresh `ErrorRecorder::new`.
    recorder: Arc<super::backfill_errors::ErrorRecorder>,
    /// Caps in-flight PDS requests across the whole job.
    ///
    /// This gates *requests*, never worker startup. Gating startup deadlocks:
    /// a worker cannot exit until its channel closes, and its channel closes
    /// only when the dispatcher finishes, so no permit is ever released during
    /// dispatch — a worker that never got one never drains its bounded queue,
    /// and the dispatcher then blocks forever trying to fill it. Every worker
    /// must be able to consume the moment its sender exists.
    requests: Arc<tokio::sync::Semaphore>,
}

/// DIDs already recorded as a fetch-phase give-up, so a DID that fails on
/// several collections produces one error row and one count, not N of each.
///
/// The detail table's primary key is `(job_id, did, phase)`, so the Nth write
/// for one DID upserts over the first while `ErrorCounts` gains N — which is
/// how a 20-lexicon job could report "20,000 failed" for 1,000 dead repos and
/// announce a cap it had not reached. Collapsing here rather than widening the
/// key keeps `backfill_errors_list`'s `did > ?` keyset cursor sound.
///
/// One set per worker is one set per job for any given DID, since a DID is
/// routed to exactly one PDS worker.
type RecordedDids = std::collections::HashSet<String>;

/// Order a DID's failed collections so a retryable failure is handled first.
///
/// Only the first give-up for a DID is recorded, and a retryable kind is the
/// more useful one to keep: `retry-failed` selects on retryability, so this is
/// what keeps the DID reachable by a retry job.
fn retryable_give_ups_first(failures: &mut [(String, FetchOutcome)]) {
    failures.sort_by_key(|(_, outcome)| match outcome {
        FetchOutcome::Failed { failure, .. } => !failure.kind.is_retryable(),
        FetchOutcome::Complete { .. } => true,
    });
}

/// One fetch failure for one (did, collection): either defer it for retry or
/// hand it to the recorder as a give-up.
///
/// Shared by every place a `FetchOutcome::Failed` is handled — the primary
/// per-DID results arm, the post-cancellation drain, and the deferred-retry
/// loop — so the retry/give-up policy can't drift between them.
#[allow(clippy::too_many_arguments)]
async fn defer_or_give_up_fetch(
    state: &AppState,
    job_id: &str,
    pds_endpoint: &str,
    pds_host: &str,
    recorder: &super::backfill_errors::ErrorRecorder,
    cooldowns: &mut HostCooldowns,
    deferred: &mut DeferredQueue<(String, String, Option<String>)>,
    recorded: &mut RecordedDids,
    max_attempts: u32,
    did: String,
    collection: String,
    cursor: Option<String>,
    failure: crate::admin::backfill_errors::BackfillFailure,
    attempts: u32,
) {
    let now = std::time::Instant::now();
    if failure.kind.is_retryable() && attempts < max_attempts {
        cooldowns.record_failure(pds_host, failure.retry_after, now);
        deferred.push(DeferredItem {
            payload: (did, collection, cursor),
            host: pds_host.to_string(),
            attempts,
            eligible_at: now,
        });
    } else {
        record_fetch_give_up(
            state,
            job_id,
            recorder,
            recorded,
            &did,
            &collection,
            &failure,
            attempts,
        )
        .await;
        tracing::warn!(
            did,
            collection,
            pds = %pds_endpoint,
            kind = failure.kind.as_str(),
            attempts,
            "giving up fetching records from PDS: {}",
            failure.message
        );
    }
}

/// Record one fetch-phase give-up, at most once per DID per job.
///
/// The `tracing` line stays per-collection at every call site — an operator
/// reading logs wants to know which collection failed. Only the *row* and the
/// *count*, which are per-DID by the detail table's key, are collapsed.
#[allow(clippy::too_many_arguments)]
async fn record_fetch_give_up(
    state: &AppState,
    job_id: &str,
    recorder: &super::backfill_errors::ErrorRecorder,
    recorded: &mut RecordedDids,
    did: &str,
    collection: &str,
    failure: &crate::admin::backfill_errors::BackfillFailure,
    attempts: u32,
) {
    if !recorded.insert(did.to_string()) {
        return;
    }
    recorder
        .record(
            state,
            job_id,
            did,
            Some(collection),
            "fetch",
            failure,
            attempts,
        )
        .await;
}

/// Give up on a PDS that has stopped answering, rather than re-offering it one
/// deferred item per cooldown.
///
/// A no-op unless the host is saturated, so the healthy path — a transient
/// rate limit that clears on the first successful retry — is untouched. Every
/// abandoned item still reaches the recorder carrying the failure that killed
/// the host, so the error taxonomy the dashboard reads is unchanged; only the
/// time taken to reach it stops scaling with the queue length.
#[allow(clippy::too_many_arguments)]
async fn abandon_saturated_pds(
    state: &AppState,
    job_id: &str,
    pds_endpoint: &str,
    pds_host: &str,
    recorder: &super::backfill_errors::ErrorRecorder,
    cooldowns: &HostCooldowns,
    deferred: &mut DeferredQueue<(String, String, Option<String>)>,
    recorded: &mut RecordedDids,
    failure: &crate::admin::backfill_errors::BackfillFailure,
) {
    // Only a host-level failure may be attributed to the rest of the queue.
    // A `repo_not_found` is a property of the one repo that provoked it, so
    // stamping it onto every other DID behind this host would misreport them
    // in exactly the way this feature exists to prevent — and a definitive
    // answer from a live server is evidence the host is answering, not that
    // it is down.
    if !failure.kind.is_retryable() || !cooldowns.is_saturated(pds_host) {
        return;
    }
    let abandoned = deferred.drain_host(pds_host);
    if abandoned.is_empty() {
        return;
    }
    tracing::warn!(
        pds = %pds_endpoint,
        host = pds_host,
        abandoned = abandoned.len(),
        kind = failure.kind.as_str(),
        "PDS failed {} times consecutively; giving up on its remaining deferred \
         fetches: {}",
        cooldowns.consecutive_failures(pds_host),
        failure.message
    );
    for item in abandoned {
        let (did, collection, _cursor) = item.payload;
        record_fetch_give_up(
            state,
            job_id,
            recorder,
            recorded,
            &did,
            &collection,
            failure,
            item.attempts,
        )
        .await;
    }
}

/// The result of fetching every collection for one DID: total records
/// fetched, whether any collection succeeded (clears the host's cooldown),
/// and the per-collection failures still needing a defer-or-give-up decision.
type DidFetchResult = (String, i32, bool, Vec<(String, FetchOutcome)>);

async fn run_pds_worker(ctx: FetchContext, pds_endpoint: String, mut rx: mpsc::Receiver<String>) {
    let FetchContext {
        state,
        job_id,
        collections,
        processed_repos,
        total_records,
        cancelled,
        dids_per_pds,
        recorder,
        requests,
    } = ctx;
    let mut fetches = FuturesUnordered::new();
    let mut rx_open = true;
    let mut next_flush = random_batch_threshold(10);

    let max_attempts = load_max_attempts(&state).await;
    let pds_host = profile::host_of(&pds_endpoint);
    // Local to this worker, not job-wide state: every PDS worker owns its own
    // cooldown and queue, keyed to the one host it alone talks to.
    let mut cooldowns = HostCooldowns::new();
    let mut deferred: DeferredQueue<(String, String, Option<String>)> = DeferredQueue::new();
    let mut recorded: RecordedDids = RecordedDids::new();

    loop {
        tokio::select! {
            biased;

            Some(result) = fetches.next(), if !fetches.is_empty() => {
                let (did, records, any_success, mut failures): DidFetchResult = result;
                total_records.fetch_add(records, Ordering::Relaxed);
                if any_success {
                    cooldowns.record_success(&pds_host);
                }
                retryable_give_ups_first(&mut failures);
                for (collection, outcome) in failures {
                    let FetchOutcome::Failed { cursor, failure, .. } = outcome else {
                        continue;
                    };
                    defer_or_give_up_fetch(
                        &state,
                        job_id.as_str(),
                        &pds_endpoint,
                        &pds_host,
                        &recorder,
                        &mut cooldowns,
                        &mut deferred,
                        &mut recorded,
                        max_attempts,
                        did.clone(),
                        collection,
                        cursor,
                        failure,
                        1,
                    )
                    .await;
                }

                // Mark DID as completed
                let sql = adapt_sql(
                    "UPDATE happyview_backfill_repos SET status = 'completed', records_fetched = ? WHERE job_id = ? AND did = ?",
                    state.db_backend,
                );
                let _ = crate::db::query(&sql)
                    .bind(records)
                    .bind(job_id.as_str())
                    .bind(&did)
                    .execute(&state.backfill_db)
                    .await;

                publish_event(&state, super::types::BackfillEvent::RepoFetched {
                    job_id: job_id.to_string(),
                    did: did.clone(),
                    pds_endpoint: pds_endpoint.clone(),
                    records_fetched: records,
                });

                let repos = processed_repos.fetch_add(1, Ordering::Relaxed) + 1;
                let records = total_records.load(Ordering::Relaxed);
                if repos >= next_flush {
                    let sql = adapt_sql(
                        "UPDATE happyview_backfill_jobs SET processed_repos = ?, total_records = ? WHERE id = ?",
                        state.db_backend,
                    );
                    let _ = crate::db::query(&sql)
                        .bind(repos)
                        .bind(records)
                        .bind(job_id.as_str())
                        .execute(&state.backfill_db)
                        .await;
                    next_flush = repos + random_batch_threshold(10);
                }
                if cancelled.load(Ordering::Relaxed) || should_stop_worker(&state, job_id.as_str()).await {
                    cancelled.store(true, Ordering::Relaxed);
                    break;
                }
                publish_event(&state, super::types::BackfillEvent::JobCounters {
                    job_id: job_id.to_string(),
                    total_repos: None,
                    resolved_repos: None,
                    processed_repos: Some(repos),
                    total_records: Some(records),
                });
            }

            did = rx.recv(), if rx_open && fetches.len() < dids_per_pds => {
                match did {
                    Some(did) if !cancelled.load(Ordering::Relaxed) => {
                        let state = Arc::clone(&state);
                        let collections = collections.clone();
                        let pds_endpoint = pds_endpoint.clone();
                        let cancelled = Arc::clone(&cancelled);
                        let requests = Arc::clone(&requests);

                        fetches.push(async move {
                            let mut count: i32 = 0;
                            let mut any_success = false;
                            let mut failures: Vec<(String, FetchOutcome)> = Vec::new();
                            for collection in collections.iter() {
                                if cancelled.load(Ordering::Relaxed) {
                                    break;
                                }
                                // Per collection, not per DID: a job spanning
                                // twenty lexicons must not pin one permit for
                                // all twenty sequential request streams.
                                let _permit = requests
                                    .acquire()
                                    .await
                                    .expect("request semaphore is never closed");
                                match fetch_records_page_loop(
                                    &state,
                                    &pds_endpoint,
                                    &did,
                                    collection,
                                    None,
                                    &cancelled,
                                )
                                .await
                                {
                                    FetchOutcome::Complete { count: c } => {
                                        count += c as i32;
                                        any_success = true;
                                    }
                                    outcome @ FetchOutcome::Failed { count: c, .. } => {
                                        count += c as i32;
                                        failures.push((collection.clone(), outcome));
                                    }
                                }
                            }
                            (did, count, any_success, failures)
                        });
                    }
                    _ => {
                        rx_open = false;
                    }
                }
            }

            else => break,
        }
    }

    // Drain any remaining fetches
    while let Some(result) = fetches.next().await {
        let (did, records, any_success, mut failures): DidFetchResult = result;
        total_records.fetch_add(records, Ordering::Relaxed);
        if any_success {
            cooldowns.record_success(&pds_host);
        }
        retryable_give_ups_first(&mut failures);
        for (collection, outcome) in failures {
            let FetchOutcome::Failed {
                cursor, failure, ..
            } = outcome
            else {
                continue;
            };
            defer_or_give_up_fetch(
                &state,
                job_id.as_str(),
                &pds_endpoint,
                &pds_host,
                &recorder,
                &mut cooldowns,
                &mut deferred,
                &mut recorded,
                max_attempts,
                did.clone(),
                collection,
                cursor,
                failure,
                1,
            )
            .await;
        }

        let sql = adapt_sql(
            "UPDATE happyview_backfill_repos SET status = 'completed', records_fetched = ? WHERE job_id = ? AND did = ?",
            state.db_backend,
        );
        let _ = crate::db::query(&sql)
            .bind(records)
            .bind(job_id.as_str())
            .bind(&did)
            .execute(&state.backfill_db)
            .await;

        publish_event(
            &state,
            super::types::BackfillEvent::RepoFetched {
                job_id: job_id.to_string(),
                did: did.clone(),
                pds_endpoint: pds_endpoint.clone(),
                records_fetched: records,
            },
        );

        processed_repos.fetch_add(1, Ordering::Relaxed);
    }

    // Deferred pass. All primary fetches are exhausted, so anything still
    // here is waiting on a clock rather than on work.
    loop {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        match next_drain_step(&mut deferred, &cooldowns, Duration::from_secs(2)).await {
            DrainStep::Done => break,
            DrainStep::Slept => {
                if should_stop_worker(&state, job_id.as_str()).await {
                    cancelled.store(true, Ordering::Relaxed);
                    break;
                }
            }
            DrainStep::Retry(item) => {
                let (did, collection, cursor) = item.payload;
                let attempts = item.attempts + 1;
                // A retry is a request like any other and counts against the
                // same budget, or a job full of retrying workers would ignore
                // the cap entirely.
                let permit = requests
                    .acquire()
                    .await
                    .expect("request semaphore is never closed");
                let outcome = fetch_records_page_loop(
                    &state,
                    &pds_endpoint,
                    &did,
                    &collection,
                    cursor,
                    &cancelled,
                )
                .await;
                drop(permit);
                match outcome {
                    FetchOutcome::Complete { count } => {
                        cooldowns.record_success(&pds_host);
                        total_records.fetch_add(count as i32, Ordering::Relaxed);
                    }
                    FetchOutcome::Failed {
                        count,
                        cursor,
                        failure,
                    } => {
                        total_records.fetch_add(count as i32, Ordering::Relaxed);
                        let last_failure = failure.clone();
                        defer_or_give_up_fetch(
                            &state,
                            job_id.as_str(),
                            &pds_endpoint,
                            &pds_host,
                            &recorder,
                            &mut cooldowns,
                            &mut deferred,
                            &mut recorded,
                            max_attempts,
                            did,
                            collection,
                            cursor,
                            failure,
                            attempts,
                        )
                        .await;
                        abandon_saturated_pds(
                            &state,
                            job_id.as_str(),
                            &pds_endpoint,
                            &pds_host,
                            &recorder,
                            &cooldowns,
                            &mut deferred,
                            &mut recorded,
                            &last_failure,
                        )
                        .await;
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 3: Fetch records from PDS instances (legacy, for resumed jobs)
// ---------------------------------------------------------------------------

async fn run_fetching_phase(
    state: &AppState,
    job_id: &str,
    collections: &[String],
    concurrency: &BackfillConcurrency,
) -> (i32, i32) {
    set_stage(state, job_id, "fetching_records").await;

    // One error sink for the whole job, shared by every PDS below — see
    // `ErrorRecorder`'s doc comment for why a per-worker recorder would be
    // wrong. This function only runs once per job (the alternate, "already
    // resolved" path to `run_pipelined_resolve_and_fetch`), so constructing
    // it once here follows the same one-per-job rule.
    let recorder = Arc::new(super::backfill_errors::ErrorRecorder::new(state, job_id).await);
    let max_attempts = load_max_attempts(state).await;

    // Load pending repos grouped by PDS
    let sql = adapt_sql(
        "SELECT did, pds_endpoint FROM happyview_backfill_repos WHERE job_id = ? AND status = 'pending' AND pds_endpoint IS NOT NULL",
        state.db_backend,
    );
    let rows: Vec<(String, String)> = crate::db::query_as(&sql)
        .bind(job_id)
        .fetch_all(&state.backfill_db)
        .await
        .unwrap_or_default();

    let mut pds_to_dids: HashMap<String, Vec<String>> = HashMap::new();
    for (did, pds) in rows {
        pds_to_dids.entry(pds).or_default().push(did);
    }

    // Count already-completed repos for accurate progress
    let sql = adapt_sql(
        "SELECT COUNT(*) FROM happyview_backfill_repos WHERE job_id = ? AND status = 'completed'",
        state.db_backend,
    );
    let already_completed: i32 = crate::db::query_as::<(i32,)>(&sql)
        .bind(job_id)
        .fetch_one(&state.backfill_db)
        .await
        .map(|(c,)| c)
        .unwrap_or(0);

    // Reset processed_repos for the fetching phase
    update_job_counter(state, job_id, "processed_repos", already_completed).await;

    // Seed total_records from DB so a resumed job doesn't lose its prior count
    let existing_records: i32 = {
        let sql = adapt_sql(
            "SELECT total_records FROM happyview_backfill_jobs WHERE id = ?",
            state.db_backend,
        );
        crate::db::query_as::<(Option<i32>,)>(&sql)
            .bind(job_id)
            .fetch_one(&state.backfill_db)
            .await
            .map(|(c,)| c.unwrap_or(0))
            .unwrap_or(0)
    };

    let processed_repos = Arc::new(AtomicI32::new(already_completed));
    let total_records = Arc::new(AtomicI32::new(existing_records));
    let cancelled = Arc::new(AtomicBool::new(false));
    let next_flush = Arc::new(AtomicI32::new(
        already_completed + random_batch_threshold(10),
    ));
    let state = Arc::new(state.clone());
    let collections = Arc::new(collections.to_vec());
    let job_id_arc = Arc::new(job_id.to_string());

    let pds_entries: Vec<(String, Vec<String>)> = pds_to_dids.into_iter().collect();

    let dids_per_pds = concurrency.dids_per_pds;
    stream::iter(pds_entries)
        .for_each_concurrent(concurrency.pds, |(pds_endpoint, dids)| {
            let state = Arc::clone(&state);
            let collections = Arc::clone(&collections);
            let processed_repos = Arc::clone(&processed_repos);
            let total_records = Arc::clone(&total_records);
            let cancelled = Arc::clone(&cancelled);
            let next_flush = Arc::clone(&next_flush);
            let job_id = Arc::clone(&job_id_arc);
            let recorder = Arc::clone(&recorder);

            async move {
                // Local to this PDS, not job-wide state: every PDS here gets
                // its own cooldown and queue, keyed to the one host it alone
                // talks to.
                let pds_host = profile::host_of(&pds_endpoint);
                let mut cooldowns = HostCooldowns::new();
                let mut deferred: DeferredQueue<(String, String, Option<String>)> =
                    DeferredQueue::new();
                let mut recorded: RecordedDids = RecordedDids::new();

                stream::iter(dids)
                    .map(|did| {
                        let state = Arc::clone(&state);
                        let collections = Arc::clone(&collections);
                        let cancelled = Arc::clone(&cancelled);
                        let pds_endpoint = pds_endpoint.clone();

                        async move {
                            if cancelled.load(Ordering::Relaxed) {
                                return (did, 0i32, false, Vec::new());
                            }

                            let mut did_records: i32 = 0;
                            let mut any_success = false;
                            let mut failures: Vec<(String, FetchOutcome)> = Vec::new();
                            for collection in collections.iter() {
                                if cancelled.load(Ordering::Relaxed) {
                                    break;
                                }
                                match fetch_records_page_loop(
                                    &state,
                                    &pds_endpoint,
                                    &did,
                                    collection,
                                    None,
                                    &cancelled,
                                )
                                .await
                                {
                                    FetchOutcome::Complete { count } => {
                                        did_records += count as i32;
                                        any_success = true;
                                    }
                                    outcome @ FetchOutcome::Failed { count, .. } => {
                                        did_records += count as i32;
                                        failures.push((collection.clone(), outcome));
                                    }
                                }
                            }
                            (did, did_records, any_success, failures)
                        }
                    })
                    // Fetches for different DIDs on this PDS run concurrently;
                    // `for_each` below still consumes their results one at a
                    // time, which is what lets the cooldown/deferred-queue
                    // bookkeeping below use plain `&mut` instead of a lock.
                    .buffer_unordered(dids_per_pds)
                    .for_each(|(did, did_records, any_success, mut failures)| {
                        // `HostCooldowns`/`DeferredQueue` can't be borrowed
                        // into the returned future here — `for_each`'s FnMut
                        // signature doesn't let a captured `&mut` escape into
                        // it (a borrow-checker limitation, not a concurrency
                        // one; `for_each` still drives one future to
                        // completion before calling this closure again). So
                        // the defer/give-up decision is made synchronously
                        // here, before the async block, which only awaits
                        // the give-ups' recorder I/O.
                        total_records.fetch_add(did_records, Ordering::Relaxed);
                        if any_success {
                            cooldowns.record_success(&pds_host);
                        }

                        retryable_give_ups_first(&mut failures);
                        // At most one give-up per DID reaches the recorder —
                        // the detail table is keyed `(job_id, did, phase)`, so
                        // recording once per failed collection inflated
                        // `error_counts` against the rows it was supposed to
                        // summarise. `retryable_give_ups_first` above is what
                        // decides *which* one survives.
                        let mut give_up: Option<(
                            String,
                            String,
                            crate::admin::backfill_errors::BackfillFailure,
                        )> = None;
                        for (collection, outcome) in failures {
                            let FetchOutcome::Failed { cursor, failure, .. } = outcome else {
                                continue;
                            };
                            let attempts = 1;
                            if failure.kind.is_retryable() && attempts < max_attempts {
                                let now = std::time::Instant::now();
                                cooldowns.record_failure(&pds_host, failure.retry_after, now);
                                deferred.push(DeferredItem {
                                    payload: (did.clone(), collection, cursor),
                                    host: pds_host.clone(),
                                    attempts,
                                    eligible_at: now,
                                });
                            } else {
                                // Logged per collection even when only one is
                                // recorded: the log is where an operator finds
                                // out *which* collection failed.
                                tracing::warn!(
                                    did,
                                    collection,
                                    pds = %pds_endpoint,
                                    kind = failure.kind.as_str(),
                                    attempts,
                                    "giving up fetching records from PDS: {}",
                                    failure.message
                                );
                                if give_up.is_none() && recorded.insert(did.clone()) {
                                    give_up = Some((did.clone(), collection, failure));
                                }
                            }
                        }

                        let state = Arc::clone(&state);
                        let processed_repos = Arc::clone(&processed_repos);
                        let total_records = Arc::clone(&total_records);
                        let cancelled = Arc::clone(&cancelled);
                        let next_flush = Arc::clone(&next_flush);
                        let job_id = Arc::clone(&job_id);
                        let recorder = Arc::clone(&recorder);

                        async move {
                            if let Some((did, collection, failure)) = give_up {
                                recorder
                                    .record(
                                        &state,
                                        job_id.as_str(),
                                        &did,
                                        Some(collection.as_str()),
                                        "fetch",
                                        &failure,
                                        1,
                                    )
                                    .await;
                            }

                            // Mark DID as completed
                            let sql = adapt_sql(
                                "UPDATE happyview_backfill_repos SET status = 'completed', records_fetched = ? WHERE job_id = ? AND did = ?",
                                state.db_backend,
                            );
                            let _ = crate::db::query(&sql)
                                .bind(did_records)
                                .bind(job_id.as_str())
                                .bind(&did)
                                .execute(&state.backfill_db)
                                .await;

                            let repos = processed_repos.fetch_add(1, Ordering::Relaxed) + 1;
                            let records = total_records.load(Ordering::Relaxed);

                            let threshold = next_flush.load(Ordering::Relaxed);
                            if repos >= threshold
                                && next_flush.compare_exchange(threshold, repos + random_batch_threshold(10), Ordering::Relaxed, Ordering::Relaxed).is_ok()
                            {
                                let backend = state.db_backend;
                                let sql = adapt_sql(
                                    "UPDATE happyview_backfill_jobs SET processed_repos = ?, total_records = ? WHERE id = ?",
                                    backend,
                                );
                                let _ = crate::db::query(&sql)
                                    .bind(repos)
                                    .bind(records)
                                    .bind(job_id.as_str())
                                    .execute(&state.backfill_db)
                                    .await;

                                if should_stop_worker(&state, job_id.as_str()).await {
                                    cancelled.store(true, Ordering::Relaxed);
                                }
                            }

                            publish_event(&state, super::types::BackfillEvent::JobCounters {
                                job_id: job_id.to_string(),
                                total_repos: None,
                                resolved_repos: None,
                                processed_repos: Some(repos),
                                total_records: Some(records),
                            });
                        }
                    })
                    .await;

                // Deferred pass. All primary fetches for this PDS are
                // exhausted, so anything still here is waiting on a clock
                // rather than on work.
                loop {
                    if cancelled.load(Ordering::Relaxed) {
                        break;
                    }
                    match next_drain_step(&mut deferred, &cooldowns, Duration::from_secs(2)).await
                    {
                        DrainStep::Done => break,
                        DrainStep::Slept => {
                            if should_stop_worker(&state, job_id.as_str()).await {
                                cancelled.store(true, Ordering::Relaxed);
                                break;
                            }
                        }
                        DrainStep::Retry(item) => {
                            let (did, collection, cursor) = item.payload;
                            let attempts = item.attempts + 1;
                            match fetch_records_page_loop(
                                &state,
                                &pds_endpoint,
                                &did,
                                &collection,
                                cursor,
                                &cancelled,
                            )
                            .await
                            {
                                FetchOutcome::Complete { count } => {
                                    cooldowns.record_success(&pds_host);
                                    total_records.fetch_add(count as i32, Ordering::Relaxed);
                                }
                                FetchOutcome::Failed { count, cursor, failure } => {
                                    total_records.fetch_add(count as i32, Ordering::Relaxed);
                                    let last_failure = failure.clone();
                                    defer_or_give_up_fetch(
                                        &state,
                                        job_id.as_str(),
                                        &pds_endpoint,
                                        &pds_host,
                                        &recorder,
                                        &mut cooldowns,
                                        &mut deferred,
                                        &mut recorded,
                                        max_attempts,
                                        did,
                                        collection,
                                        cursor,
                                        failure,
                                        attempts,
                                    )
                                    .await;
                                    abandon_saturated_pds(
                                        &state,
                                        job_id.as_str(),
                                        &pds_endpoint,
                                        &pds_host,
                                        &recorder,
                                        &cooldowns,
                                        &mut deferred,
                                        &mut recorded,
                                        &last_failure,
                                    )
                                    .await;
                                }
                            }
                        }
                    }
                }
            }
        })
        .await;

    recorder.flush(&state, job_id).await;

    let final_repos = processed_repos.load(Ordering::Relaxed);
    let final_records = total_records.load(Ordering::Relaxed);

    // Persist final counts so they're accurate regardless of batch size
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET processed_repos = ?, total_records = ? WHERE id = ?",
        state.db_backend,
    );
    let _ = crate::db::query(&sql)
        .bind(final_repos)
        .bind(final_records)
        .bind(job_id)
        .execute(&state.backfill_db)
        .await;

    (final_repos, final_records)
}

struct PreparedRecord {
    uri: String,
    did: String,
    collection: String,
    rkey: String,
    record_json: String,
    cid: String,
}

async fn batch_upsert_records(state: &AppState, batch: &[PreparedRecord]) {
    if batch.is_empty() {
        return;
    }

    let backend = state.db_backend;
    let now = now_rfc3339();

    // Build multi-row INSERT. 8 params per row; ON CONFLICT uses EXCLUDED.
    let placeholders: Vec<String> = (0..batch.len())
        .map(|_| "(?, ?, ?, ?, ?, ?, ?, ?)".to_string())
        .collect();
    let raw_sql = format!(
        "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at) VALUES {} ON CONFLICT (uri) DO UPDATE SET record = EXCLUDED.record, cid = EXCLUDED.cid, indexed_at = EXCLUDED.indexed_at",
        placeholders.join(", ")
    );
    let sql = adapt_sql(&raw_sql, backend);

    let mut query = crate::db::query(&sql);
    for rec in batch {
        query = query
            .bind(&rec.uri)
            .bind(&rec.did)
            .bind(&rec.collection)
            .bind(&rec.rkey)
            .bind(&rec.record_json)
            .bind(&rec.cid)
            .bind(&now)
            .bind(&now);
    }

    if let Err(e) = query.execute(&state.backfill_db).await {
        tracing::warn!(batch_size = batch.len(), "batch record upsert failed: {e}");
    }

    // Batch sync_refs: delete old refs for all URIs, then insert new ones.
    let uris: Vec<&str> = batch.iter().map(|r| r.uri.as_str()).collect();
    let delete_placeholders: Vec<&str> = (0..uris.len()).map(|_| "?").collect();
    let delete_raw = format!(
        "DELETE FROM happyview_record_refs WHERE source_uri IN ({})",
        delete_placeholders.join(", ")
    );
    let delete_sql = adapt_sql(&delete_raw, backend);
    let mut del_query = crate::db::query(&delete_sql);
    for uri in &uris {
        del_query = del_query.bind(*uri);
    }
    let _ = del_query.execute(&state.backfill_db).await;

    // Collect all new refs and batch insert them
    let mut all_refs: Vec<(&str, String, &str)> = Vec::new();
    for rec in batch {
        let record_val: serde_json::Value =
            serde_json::from_str(&rec.record_json).unwrap_or_default();
        for target_uri in crate::record_refs::extract_at_uris(&record_val) {
            all_refs.push((&rec.uri, target_uri, &rec.collection));
        }
    }

    // Insert refs in chunks to stay within SQLite's param limit (3 params per ref)
    for chunk in all_refs.chunks(300) {
        let ref_placeholders: Vec<&str> = (0..chunk.len()).map(|_| "(?, ?, ?)").collect();
        let ref_raw = format!(
            "INSERT INTO happyview_record_refs (source_uri, target_uri, collection) VALUES {} ON CONFLICT DO NOTHING",
            ref_placeholders.join(", ")
        );
        let ref_sql = adapt_sql(&ref_raw, backend);
        let mut ref_query = crate::db::query(&ref_sql);
        for (source, target, collection) in chunk {
            ref_query = ref_query.bind(*source).bind(target).bind(*collection);
        }
        let _ = ref_query.execute(&state.backfill_db).await;
    }

    // Queue label backfill only if there are active labeler subscriptions.
    // Check once per batch instead of spawning a task per record.
    let has_subscriptions: bool = crate::db::query_as::<(i64,)>(
        "SELECT COUNT(*) FROM happyview_labeler_subscriptions WHERE status = 'active'",
    )
    .fetch_one(&state.db)
    .await
    .map(|(c,)| c > 0)
    .unwrap_or(false);

    if has_subscriptions {
        for rec in batch {
            crate::labeler::backfill_labels_for_uri(Arc::new(state.clone()), rec.uri.clone());
        }
    }
}

/// The outcome of a records-page-loop attempt.
///
/// `Failed` carries the cursor reached so far so a retry can resume
/// mid-pagination instead of restarting the DID's collection from page one.
pub(super) enum FetchOutcome {
    Complete {
        count: u32,
    },
    Failed {
        count: u32,
        // Read by the deferred-retry wiring in both fetch call sites, to
        // resume mid-pagination instead of restarting the DID's collection.
        cursor: Option<String>,
        failure: crate::admin::backfill_errors::BackfillFailure,
    },
}

/// Fetch all records for a given DID and collection from a PDS via
/// `com.atproto.repo.listRecords`, paginating from `start_cursor`.
///
/// This is a single attempt at draining the collection: it never sleeps on a
/// rate limit and never retries a transport or server error. It returns
/// `Failed` with the cursor reached so far so the caller can defer and resume.
async fn fetch_records_page_loop(
    state: &AppState,
    pds_endpoint: &str,
    did: &str,
    collection: &str,
    start_cursor: Option<String>,
    cancelled: &AtomicBool,
) -> FetchOutcome {
    let base = pds_endpoint.trim_end_matches('/');
    let mut cursor: Option<String> = start_cursor;
    let mut count: u32 = 0;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }

        let mut url = format!(
            "{base}/xrpc/com.atproto.repo.listRecords?repo={did}&collection={collection}&limit=100"
        );
        if let Some(ref c) = cursor {
            url.push_str(&format!("&cursor={c}"));
        }

        let resp = match state.http.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                return FetchOutcome::Failed {
                    count,
                    cursor,
                    failure: crate::admin::backfill_errors::BackfillFailure::from_reqwest(&e),
                };
            }
        };

        if !resp.status().is_success() {
            // The body is the only thing that distinguishes the common cases —
            // a PDS answers `400 InvalidRequest / Could not find repo` for an
            // account that has been deleted or migrated away, which is routine
            // during backfill and not worth investigating. Without it, every
            // non-2xx reads identically.
            let status = resp.status();
            let headers = resp.headers().clone();
            let body = resp.text().await.unwrap_or_default();
            return FetchOutcome::Failed {
                count,
                cursor,
                failure: crate::admin::backfill_errors::BackfillFailure::from_pds_response(
                    status, &body, &headers,
                ),
            };
        }

        let body: ListRecordsResponse = match resp.json().await {
            Ok(b) => b,
            Err(e) => {
                return FetchOutcome::Failed {
                    count,
                    cursor,
                    failure: crate::admin::backfill_errors::BackfillFailure {
                        kind: crate::admin::backfill_errors::BackfillErrorKind::Other,
                        message: format!("invalid PDS response: {e}"),
                        retry_after: None,
                    },
                };
            }
        };

        let page_count = body.records.len();

        let mut batch: Vec<PreparedRecord> = Vec::with_capacity(page_count);
        for entry in &body.records {
            let rkey = entry.uri.rsplit('/').next().unwrap_or_default().to_string();
            let uri = format!("at://{did}/{collection}/{rkey}");

            // Reject records whose claimed CID doesn't match their content
            // (security review L9). The backfill source PDS is attacker-
            // controllable via the DID document, so a hostile PDS could serve a
            // record under a mismatched CID. Skip on mismatch; `Skipped`
            // (unencodable value) proceeds unchanged.
            if crate::cid_verify::verify_record_cid(&entry.cid, &entry.value)
                == crate::cid_verify::CidCheck::Mismatch
            {
                tracing::warn!(
                    collection,
                    did,
                    rkey,
                    claimed_cid = %entry.cid,
                    "record content does not match claimed CID, skipping"
                );
                continue;
            }

            let rec_to_store = match crate::lua::run_record_event_script(
                state,
                crate::lua::RecordEventPayload {
                    nsid: collection,
                    action: "create",
                    uri: &uri,
                    did,
                    rkey: &rkey,
                    record: Some(&entry.value),
                },
            )
            .await
            {
                crate::lua::RecordHookOutcome::Skip => continue,
                crate::lua::RecordHookOutcome::Replace(v) => v,
                crate::lua::RecordHookOutcome::Proceed => entry.value.clone(),
            };

            batch.push(PreparedRecord {
                uri,
                did: did.to_string(),
                collection: collection.to_string(),
                rkey,
                record_json: serde_json::to_string(&rec_to_store).unwrap_or_default(),
                cid: entry.cid.clone(),
            });
        }

        count += batch.len() as u32;
        batch_upsert_records(state, &batch).await;

        match body.cursor {
            Some(c) if page_count > 0 => cursor = Some(c),
            _ => break,
        }
    }

    FetchOutcome::Complete { count }
}

// ---------------------------------------------------------------------------
// Background backfill worker
// ---------------------------------------------------------------------------

async fn run_backfill_job(state: AppState, job_id: String) {
    let backend = state.db_backend;

    // Load job metadata
    let sql = adapt_sql(
        "SELECT collection, did, stage FROM happyview_backfill_jobs WHERE id = ?",
        backend,
    );
    let job: Option<(Option<String>, Option<String>, String)> = crate::db::query_as(&sql)
        .bind(&job_id)
        .fetch_optional(&state.backfill_db)
        .await
        .ok()
        .flatten();

    let Some((collection, did, stage)) = job else {
        tracing::error!(job_id, "backfill job not found");
        return;
    };

    // Determine target collections
    let collections: Vec<String> = if let Some(ref col) = collection {
        let lexicon_exists: bool = state
            .lexicons
            .get(col)
            .await
            .is_some_and(|lex| lex.lexicon_type == crate::lexicon::LexiconType::Record);
        if !lexicon_exists {
            let error = format!("no record-type lexicon registered for collection '{col}'");
            fail_job(&state, &job_id, &error).await;
            return;
        }
        vec![col.clone()]
    } else {
        let sql = adapt_sql(
            "SELECT id FROM happyview_lexicons WHERE json_extract(lexicon_json, '$.defs.main.type') = 'record'",
            backend,
        );
        let rows: Vec<(String,)> = match crate::db::query_as(&sql)
            .fetch_all(&state.backfill_db)
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                let error = format!("failed to query backfill-eligible lexicons: {e}");
                fail_job(&state, &job_id, &error).await;
                return;
            }
        };
        rows.into_iter().map(|(id,)| id).collect()
    };

    if collections.is_empty() {
        complete_job(
            &state,
            &job_id,
            0,
            0,
            Some("no backfill-eligible collections"),
        )
        .await;
        return;
    }

    // Run phases, skipping those already completed
    if matches!(stage.as_str(), "pending" | "discovering_repos") {
        run_discovery_phase(&state, &job_id, &collections, did.as_deref()).await;

        match should_stop(&state, &job_id).await {
            Some("cancelling") => {
                tracing::info!(job_id, "backfill job cancelled");
                finalise_cancel(&state, &job_id).await;
                return;
            }
            Some("pausing") => {
                tracing::info!(job_id, "backfill job paused");
                finalise_pause(&state, &job_id).await;
                return;
            }
            _ => {}
        }

        let total = count_repos(&state, &job_id).await;
        if total == 0 {
            complete_job(&state, &job_id, 0, 0, None).await;
            log_event(
                &state.db,
                EventLog {
                    event_type: "backfill.completed".to_string(),
                    severity: Severity::Info,
                    actor_did: None,
                    subject: collection.clone(),
                    detail: serde_json::json!({
                        "job_id": job_id,
                        "total_repos": 0,
                        "total_records": 0,
                    }),
                },
                backend,
            )
            .await;
            return;
        }
    }

    let concurrency = load_concurrency(&state).await;
    let (final_processed, final_records) = if matches!(
        stage.as_str(),
        "pending" | "discovering_repos" | "resolving_pds" | "resolving_and_fetching"
    ) {
        run_pipelined_resolve_and_fetch(&state, &job_id, &collections, &concurrency).await
    } else {
        // stage == "fetching_records": resolution already done (legacy or resumed)
        run_fetching_phase(&state, &job_id, &collections, &concurrency).await
    };

    match should_stop(&state, &job_id).await {
        Some("cancelling") => {
            tracing::info!(job_id, "backfill job cancelled");
            finalise_cancel(&state, &job_id).await;
            return;
        }
        Some("pausing") => {
            tracing::info!(job_id, "backfill job paused");
            finalise_pause(&state, &job_id).await;
            return;
        }
        _ => {}
    }

    complete_job(&state, &job_id, final_processed, final_records, None).await;

    log_event(
        &state.db,
        EventLog {
            event_type: "backfill.completed".to_string(),
            severity: Severity::Info,
            actor_did: None,
            subject: collection,
            detail: serde_json::json!({
                "job_id": job_id,
                "total_repos": final_processed,
                "total_records": final_records,
            }),
        },
        backend,
    )
    .await;
}

// ---------------------------------------------------------------------------
// Admin handlers
// ---------------------------------------------------------------------------

/// POST /admin/backfill — create a backfill job and spawn background work.
pub(super) async fn create_backfill(
    State(state): State<AppState>,
    admin: UserAuth,
    Json(body): Json<CreateBackfillBody>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    admin.require(Permission::BackfillCreate).await?;
    let backend = state.db_backend;

    let now = now_rfc3339();
    let job_id = Uuid::new_v4().to_string();
    let sql = adapt_sql(
        "INSERT INTO happyview_backfill_jobs (id, collection, did, status, stage, started_at, created_at) VALUES (?, ?, ?, 'running', 'pending', ?, ?) RETURNING id",
        backend,
    );
    let row: (String,) = crate::db::query_as(&sql)
        .bind(&job_id)
        .bind(&body.collection)
        .bind(&body.did)
        .bind(&now)
        .bind(&now)
        .fetch_one(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create backfill job: {e}")))?;

    let job_id = row.0.clone();

    log_event(
        &state.db,
        EventLog {
            event_type: "backfill.started".to_string(),
            severity: Severity::Info,
            actor_did: Some(admin.did.clone()),
            subject: body.collection.clone(),
            detail: serde_json::json!({
                "job_id": job_id.clone(),
            }),
        },
        backend,
    )
    .await;

    let spawn_state = state.clone();
    let spawn_job_id = job_id.clone();
    tokio::spawn(async move {
        run_backfill_job(spawn_state, spawn_job_id).await;
    });

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "id": job_id,
            "status": "running",
        })),
    ))
}

/// POST /admin/backfill/{id}/cancel — cancel a running backfill job.
pub(super) async fn cancel_backfill(
    State(state): State<AppState>,
    admin: UserAuth,
    Path(job_id): Path<String>,
) -> Result<Json<Value>, AppError> {
    admin.require(Permission::BackfillCreate).await?;

    let sql = adapt_sql(
        "SELECT status FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    );
    let row: Option<(String,)> = crate::db::query_as(&sql)
        .bind(&job_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to query backfill job: {e}")))?;

    match row {
        None => Err(AppError::NotFound("backfill job not found".into())),
        Some((ref status,)) if status == "cancelling" || status == "cancelled" => {
            Ok(Json(serde_json::json!({ "id": job_id, "status": status })))
        }
        Some((ref status,)) if status == "paused" => {
            finalise_cancel(&state, &job_id).await;
            log_event(
                &state.db,
                EventLog {
                    event_type: "backfill.cancelled".to_string(),
                    severity: Severity::Info,
                    actor_did: Some(admin.did.clone()),
                    subject: None,
                    detail: serde_json::json!({ "job_id": job_id }),
                },
                state.db_backend,
            )
            .await;
            Ok(Json(
                serde_json::json!({ "id": job_id, "status": "cancelled" }),
            ))
        }
        Some((status,)) if status != "running" => Err(AppError::BadRequest(format!(
            "job is not running (status: {status})"
        ))),
        Some(_) => {
            request_cancel(&state, &job_id).await;
            log_event(
                &state.db,
                EventLog {
                    event_type: "backfill.cancelling".to_string(),
                    severity: Severity::Info,
                    actor_did: Some(admin.did.clone()),
                    subject: None,
                    detail: serde_json::json!({ "job_id": job_id }),
                },
                state.db_backend,
            )
            .await;
            Ok(Json(
                serde_json::json!({ "id": job_id, "status": "cancelling" }),
            ))
        }
    }
}

/// POST /admin/backfill/{id}/pause — pause a running backfill job.
pub(super) async fn pause_backfill(
    State(state): State<AppState>,
    admin: UserAuth,
    Path(job_id): Path<String>,
) -> Result<Json<Value>, AppError> {
    admin.require(Permission::BackfillCreate).await?;

    let sql = adapt_sql(
        "SELECT status FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    );
    let row: Option<(String,)> = crate::db::query_as(&sql)
        .bind(&job_id)
        .fetch_optional(&state.backfill_db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to query backfill job: {e}")))?;

    match row {
        None => Err(AppError::NotFound("backfill job not found".into())),
        Some((ref status,)) if status == "pausing" || status == "paused" => {
            Ok(Json(serde_json::json!({ "id": job_id, "status": status })))
        }
        Some((status,)) if status != "running" => Err(AppError::BadRequest(format!(
            "job is not running (status: {status})"
        ))),
        Some(_) => {
            request_pause(&state, &job_id).await;
            log_event(
                &state.db,
                EventLog {
                    event_type: "backfill.pausing".to_string(),
                    severity: Severity::Info,
                    actor_did: Some(admin.did.clone()),
                    subject: None,
                    detail: serde_json::json!({ "job_id": job_id }),
                },
                state.db_backend,
            )
            .await;
            Ok(Json(
                serde_json::json!({ "id": job_id, "status": "pausing" }),
            ))
        }
    }
}

/// POST /admin/backfill/{id}/resume — resume a paused backfill job.
pub(super) async fn resume_backfill(
    State(state): State<AppState>,
    admin: UserAuth,
    Path(job_id): Path<String>,
) -> Result<Json<Value>, AppError> {
    admin.require(Permission::BackfillCreate).await?;

    let sql = adapt_sql(
        "SELECT status FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    );
    let row: Option<(String,)> = crate::db::query_as(&sql)
        .bind(&job_id)
        .fetch_optional(&state.backfill_db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to query backfill job: {e}")))?;

    match row {
        None => Err(AppError::NotFound("backfill job not found".into())),
        Some((status,)) if status != "paused" => Err(AppError::BadRequest(format!(
            "job is not paused (status: {status})"
        ))),
        Some(_) => {
            let sql = adapt_sql(
                "UPDATE happyview_backfill_jobs SET status = 'running' WHERE id = ?",
                state.db_backend,
            );
            let _ = crate::db::query(&sql)
                .bind(&job_id)
                .execute(&state.backfill_db)
                .await;

            let spawn_state = state.clone();
            let spawn_job_id = job_id.clone();
            tokio::spawn(async move {
                run_backfill_job(spawn_state, spawn_job_id).await;
            });

            log_event(
                &state.db,
                EventLog {
                    event_type: "backfill.resumed".to_string(),
                    severity: Severity::Info,
                    actor_did: Some(admin.did.clone()),
                    subject: None,
                    detail: serde_json::json!({ "job_id": job_id }),
                },
                state.db_backend,
            )
            .await;
            Ok(Json(
                serde_json::json!({ "id": job_id, "status": "running" }),
            ))
        }
    }
}

/// GET /admin/backfill/status — list all backfill jobs.
pub(super) async fn backfill_status(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<Vec<BackfillJob>>, AppError> {
    auth.require(Permission::BackfillRead).await?;
    let backend = state.db_backend;

    let sql = adapt_sql(
        "SELECT id, collection, did, status, stage, total_repos, resolved_repos, processed_repos, total_records, error, started_at, completed_at, created_at FROM happyview_backfill_jobs ORDER BY created_at DESC",
        backend,
    );
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        String,
        Option<String>,
        Option<String>,
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
    )> = crate::db::query_as(&sql)
        .fetch_all(&state.backfill_db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list backfill jobs: {e}")))?;

    let jobs: Vec<BackfillJob> = rows
        .into_iter()
        .map(
            |(
                id,
                collection,
                did,
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
            )| {
                BackfillJob {
                    id,
                    collection,
                    did,
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
                }
            },
        )
        .collect();

    Ok(Json(jobs))
}

// ---------------------------------------------------------------------------
// SSE events endpoint
// ---------------------------------------------------------------------------

pub(super) async fn backfill_events(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    auth: UserAuth,
) -> Result<
    axum::response::sse::Sse<
        impl futures_util::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>,
    >,
    AppError,
> {
    auth.require(Permission::BackfillRead).await?;

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
                        super::types::BackfillEvent::RepoDiscovered { job_id, .. }
                        | super::types::BackfillEvent::RepoResolved { job_id, .. }
                        | super::types::BackfillEvent::RepoFetched { job_id, .. }
                        | super::types::BackfillEvent::JobCounters { job_id, .. }
                        | super::types::BackfillEvent::JobStageChanged { job_id, .. }
                        | super::types::BackfillEvent::JobCompleted { job_id, .. }
                        | super::types::BackfillEvent::JobSnapshot { job_id, .. } => job_id,
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
pub(super) struct ReposQuery {
    phase: Option<String>,
    cursor: Option<String>,
    limit: Option<i32>,
}

pub(super) async fn backfill_repos(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    auth: UserAuth,
    axum::extract::Query(query): axum::extract::Query<ReposQuery>,
) -> Result<Json<super::types::BackfillReposResponse>, AppError> {
    auth.require(Permission::BackfillRead).await?;

    let limit = query.limit.unwrap_or(50).min(100);
    let phase_filter = match query.phase.as_deref() {
        Some("resolved") => " AND pds_endpoint IS NOT NULL",
        Some("fetched") => " AND status = 'completed'",
        _ => "",
    };
    let cursor_filter = if query.cursor.is_some() {
        " AND did > ?"
    } else {
        ""
    };

    let sql_str = format!(
        "SELECT did, pds_endpoint, status, records_fetched FROM happyview_backfill_repos WHERE job_id = ?{phase_filter}{cursor_filter} ORDER BY did ASC LIMIT ?",
    );
    let sql = adapt_sql(&sql_str, state.db_backend);

    let mut q = crate::db::query_as::<(String, Option<String>, String, i32)>(&sql).bind(&job_id);
    if let Some(ref cursor) = query.cursor {
        q = q.bind(cursor);
    }
    q = q.bind(limit + 1);

    let rows: Vec<(String, Option<String>, String, i32)> = q
        .fetch_all(&state.backfill_db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to query backfill repos: {e}")))?;

    let has_more = rows.len() > limit as usize;
    let repos: Vec<super::types::BackfillRepoEntry> = rows
        .into_iter()
        .take(limit as usize)
        .map(
            |(did, pds_endpoint, status, records_fetched)| super::types::BackfillRepoEntry {
                did,
                pds_endpoint,
                status,
                records_fetched,
            },
        )
        .collect();

    let cursor = if has_more {
        repos.last().map(|r| r.did.clone())
    } else {
        None
    };

    Ok(Json(super::types::BackfillReposResponse { repos, cursor }))
}

pub(super) async fn backfill_pds_summary(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    auth: UserAuth,
) -> Result<Json<super::types::PdsSummaryResponse>, AppError> {
    auth.require(Permission::BackfillRead).await?;

    let sql = adapt_sql(
        "SELECT pds_endpoint, COUNT(*) as total_repos, SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END) as completed_repos, SUM(records_fetched) as total_records FROM happyview_backfill_repos WHERE job_id = ? AND pds_endpoint IS NOT NULL GROUP BY pds_endpoint ORDER BY COUNT(*) DESC",
        state.db_backend,
    );

    let rows: Vec<(String, i32, i32, i64)> = crate::db::query_as(&sql)
        .bind(&job_id)
        .fetch_all(&state.backfill_db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to query PDS summary: {e}")))?;

    let pds_endpoints: Vec<super::types::PdsSummaryEntry> = rows
        .into_iter()
        .map(
            |(pds_endpoint, total_repos, completed_repos, total_records)| {
                super::types::PdsSummaryEntry {
                    pds_endpoint,
                    total_repos,
                    completed_repos,
                    total_records: total_records as i32,
                }
            },
        )
        .collect();

    Ok(Json(super::types::PdsSummaryResponse { pds_endpoints }))
}

#[derive(Deserialize)]
pub(super) struct BackfillErrorsQuery {
    kind: Option<String>,
    cursor: Option<String>,
    limit: Option<i32>,
}

/// GET /admin/backfill/{id}/errors — paginated failure detail plus exact
/// per-kind totals.
///
/// Named `..._list` rather than `backfill_errors`, which is already the SSE
/// stream handler's name.
pub(super) async fn backfill_errors_list(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    auth: UserAuth,
    axum::extract::Query(query): axum::extract::Query<BackfillErrorsQuery>,
) -> Result<Json<BackfillErrorsResponse>, AppError> {
    auth.require(Permission::BackfillRead).await?;

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
    let mut q =
        crate::db::query_as::<(String, Option<String>, String, String, String, i32, String)>(&sql)
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
        .map_err(|e| AppError::Internal(format!("failed to query backfill errors: {e}")))?;

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
async fn load_live_error_counts(state: &AppState, job_id: &str) -> ErrorCounts {
    let mut counts = load_error_counts(state, job_id).await;

    let sql = adapt_sql(
        "SELECT kind, COUNT(*) FROM happyview_backfill_errors WHERE job_id = ? GROUP BY kind",
        state.db_backend,
    );
    let rows: Vec<(String, i64)> = crate::db::query_as(&sql)
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

async fn load_error_counts(state: &AppState, job_id: &str) -> ErrorCounts {
    let sql = adapt_sql(
        "SELECT error_counts FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    );
    crate::db::query_as::<(Option<String>,)>(&sql)
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
pub(super) struct RetryFailedBody {
    kinds: Option<Vec<String>>,
}

/// POST /admin/backfill/{id}/retry-failed — spawn a new job scoped to just
/// the failed DIDs from `job_id`.
///
/// A new job rather than mutating the finished one, seeded with
/// `stage = 'resolving_pds'` so `run_backfill_job` skips discovery (its DIDs
/// are already known) and enters the resolve/fetch pipeline directly.
pub(super) async fn retry_failed_backfill(
    State(state): State<AppState>,
    admin: UserAuth,
    Path(job_id): Path<String>,
    Json(body): Json<RetryFailedBody>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    admin.require(Permission::BackfillCreate).await?;
    let backend = state.db_backend;

    let sql = adapt_sql(
        "SELECT collection, did FROM happyview_backfill_jobs WHERE id = ?",
        backend,
    );
    let row: Option<(Option<String>, Option<String>)> = crate::db::query_as(&sql)
        .bind(&job_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to query backfill job: {e}")))?;

    let Some((collection, did)) = row else {
        return Err(AppError::NotFound("backfill job not found".into()));
    };

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
        return Err(AppError::BadRequest(
            "no retryable failures for this job".into(),
        ));
    }

    let placeholders = vec!["?"; kinds.len()].join(", ");
    let sql_str = format!(
        "SELECT DISTINCT did FROM happyview_backfill_errors WHERE job_id = ? AND kind IN ({placeholders})",
    );
    let sql = adapt_sql(&sql_str, backend);
    let mut q = crate::db::query_as::<(String,)>(&sql).bind(&job_id);
    for kind in &kinds {
        q = q.bind(*kind);
    }
    let dids: Vec<(String,)> = q
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to query failed DIDs: {e}")))?;

    if dids.is_empty() {
        return Err(AppError::BadRequest(
            "no retryable failures for this job".into(),
        ));
    }

    let now = now_rfc3339();
    let new_job_id = Uuid::new_v4().to_string();

    // The job row and its seeded repos must appear together or not at all: a
    // failure partway through the chunked insert must not leave behind a job
    // marked `running` with no worker and no (or partial) repos to work on,
    // indistinguishable from a live job until the next restart's
    // `resume_backfill_jobs` sweep notices it. Committing before spawning the
    // worker also matters — spawning inside the transaction would let the
    // worker observe rows that could still roll back.
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| AppError::Internal(format!("failed to begin transaction: {e}")))?;

    let sql = adapt_sql(
        "INSERT INTO happyview_backfill_jobs \
         (id, collection, did, status, stage, started_at, created_at) \
         VALUES (?, ?, ?, 'running', 'resolving_pds', ?, ?)",
        backend,
    );
    crate::db::query(&sql)
        .bind(&new_job_id)
        .bind(&collection)
        .bind(&did)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create retry job: {e}")))?;

    // SQLite has a 999 bound-parameter limit; each row uses 2 params.
    let chunk_size = if backend == crate::db::DatabaseBackend::Sqlite {
        499
    } else {
        1000
    };
    for chunk in dids.chunks(chunk_size) {
        let placeholders = vec!["(?, ?)"; chunk.len()].join(", ");
        let sql_str = format!(
            "INSERT INTO happyview_backfill_repos (job_id, did) VALUES {placeholders} ON CONFLICT DO NOTHING",
        );
        let sql = adapt_sql(&sql_str, backend);
        let mut insert = crate::db::query(&sql);
        for (did,) in chunk {
            insert = insert.bind(&new_job_id).bind(did);
        }
        insert
            .execute(&mut *tx)
            .await
            .map_err(|e| AppError::Internal(format!("failed to seed retry repos: {e}")))?;
    }

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("failed to commit retry job: {e}")))?;

    log_event(
        &state.db,
        EventLog {
            event_type: "backfill.retry_started".to_string(),
            severity: Severity::Info,
            actor_did: Some(admin.did.clone()),
            subject: collection.clone(),
            detail: serde_json::json!({
                "job_id": new_job_id,
                "source_job_id": job_id,
                "retried_repos": dids.len(),
            }),
        },
        backend,
    )
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

// ---------------------------------------------------------------------------
// Flush endpoints
// ---------------------------------------------------------------------------

pub(super) async fn flush_backfill_details(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    auth: UserAuth,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::BackfillCreate).await?;

    let sql = adapt_sql(
        "DELETE FROM happyview_backfill_repos WHERE job_id = ?",
        state.db_backend,
    );
    let _ = crate::db::query(&sql)
        .bind(&job_id)
        .execute(&state.backfill_db)
        .await;

    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn flush_all_backfill_details(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::BackfillCreate).await?;

    let sql = adapt_sql(
        "DELETE FROM happyview_backfill_repos WHERE job_id IN (SELECT id FROM happyview_backfill_jobs WHERE status IN ('completed', 'cancelled', 'failed'))",
        state.db_backend,
    );
    let _ = crate::db::query(&sql).execute(&state.backfill_db).await;

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Retention cleanup
// ---------------------------------------------------------------------------

pub async fn run_backfill_retention_cleanup(state: &AppState) {
    use super::settings::get_setting;

    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(86400));

    loop {
        interval.tick().await;

        let retention_days: i64 = get_setting(
            &state.backfill_db,
            "backfill_retention_days",
            state.db_backend,
        )
        .await
        .and_then(|v| v.parse().ok())
        .unwrap_or(28);

        if retention_days == 0 {
            continue;
        }

        let cutoff = chrono::Utc::now() - chrono::Duration::days(retention_days);
        let cutoff_str = cutoff.to_rfc3339();

        let sql = adapt_sql(
            "DELETE FROM happyview_backfill_repos WHERE job_id IN (SELECT id FROM happyview_backfill_jobs WHERE completed_at IS NOT NULL AND completed_at < ?)",
            state.db_backend,
        );
        match crate::db::query(&sql)
            .bind(&cutoff_str)
            .execute(&state.backfill_db)
            .await
        {
            Ok(result) => {
                let deleted = result.rows_affected();
                if deleted > 0 {
                    tracing::info!(
                        deleted,
                        retention_days,
                        "cleaned up old backfill detail rows"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "backfill retention cleanup failed");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Startup resumption
// ---------------------------------------------------------------------------

/// Resume any backfill jobs that were running when the server last stopped.
/// Jobs stuck in `cancelling` are finalised immediately.
pub async fn resume_backfill_jobs(state: &AppState) {
    let sql = adapt_sql(
        "SELECT id, status FROM happyview_backfill_jobs WHERE status IN ('running', 'cancelling', 'pausing')",
        state.db_backend,
    );
    let rows: Vec<(String, String)> = crate::db::query_as(&sql)
        .fetch_all(&state.backfill_db)
        .await
        .unwrap_or_default();

    for (job_id, status) in rows {
        match status.as_str() {
            "cancelling" => {
                tracing::info!(
                    job_id,
                    "finalising cancelled backfill job from previous run"
                );
                finalise_cancel(state, &job_id).await;
            }
            "pausing" => {
                tracing::info!(job_id, "finalising paused backfill job from previous run");
                finalise_pause(state, &job_id).await;
            }
            _ => {
                tracing::info!(job_id, "resuming interrupted backfill job");
                let spawn_state = state.clone();
                tokio::spawn(async move {
                    run_backfill_job(spawn_state, job_id).await;
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::{memory_pool, test_state_with_pool};

    async fn state_with_job(
        job_id: &str,
        status: &str,
        stage: &str,
        error_counts: Option<&str>,
    ) -> AppState {
        let pool = memory_pool().await;
        crate::db::query(
            "CREATE TABLE happyview_backfill_jobs (
                id TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                stage TEXT NOT NULL,
                total_repos INTEGER,
                resolved_repos INTEGER,
                processed_repos INTEGER,
                total_records INTEGER,
                error_counts TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("create happyview_backfill_jobs table: {e}"));

        crate::db::query(
            "INSERT INTO happyview_backfill_jobs \
             (id, status, stage, total_repos, resolved_repos, processed_repos, total_records, error_counts) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(job_id)
        .bind(status)
        .bind(stage)
        .bind(10i32)
        .bind(4i32)
        .bind(2i32)
        .bind(50i32)
        .bind(error_counts)
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("insert backfill job: {e}"));

        test_state_with_pool(pool)
    }

    #[tokio::test]
    async fn snapshot_reflects_current_row() {
        let state = state_with_job(
            "job-1",
            "running",
            "resolving_and_fetching",
            Some(r#"{"dns_failure":2}"#),
        )
        .await;

        let event = build_job_snapshot(&state, "job-1")
            .await
            .expect("snapshot for existing job");

        match event {
            super::super::types::BackfillEvent::JobSnapshot {
                job_id,
                status,
                stage,
                total_repos,
                resolved_repos,
                processed_repos,
                total_records,
                error_counts,
            } => {
                assert_eq!(job_id, "job-1");
                assert_eq!(status, "running");
                assert_eq!(stage, "resolving_and_fetching");
                assert_eq!(total_repos, Some(10));
                assert_eq!(resolved_repos, Some(4));
                assert_eq!(processed_repos, Some(2));
                assert_eq!(total_records, Some(50));
                assert_eq!(error_counts, serde_json::json!({"dns_failure": 2}));
            }
            other => panic!("expected JobSnapshot, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn snapshot_defaults_error_counts_when_null() {
        let state = state_with_job("job-2", "completed", "completed", None).await;

        let event = build_job_snapshot(&state, "job-2")
            .await
            .expect("snapshot for existing job");

        match event {
            super::super::types::BackfillEvent::JobSnapshot { error_counts, .. } => {
                assert_eq!(error_counts, serde_json::json!({}));
            }
            other => panic!("expected JobSnapshot, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn snapshot_is_none_for_unknown_job() {
        let state = state_with_job("job-3", "running", "discovering_repos", None).await;

        assert!(build_job_snapshot(&state, "does-not-exist").await.is_none());
    }

    // -----------------------------------------------------------------------
    // Errors API
    // -----------------------------------------------------------------------

    async fn state_with_errors_job(
        job_id: &str,
        collection: Option<&str>,
        error_counts_json: &str,
    ) -> AppState {
        let pool = memory_pool().await;
        crate::db::query(
            "CREATE TABLE happyview_backfill_jobs (
                id TEXT PRIMARY KEY,
                collection TEXT,
                did TEXT,
                status TEXT NOT NULL,
                stage TEXT NOT NULL,
                total_repos INTEGER,
                resolved_repos INTEGER,
                processed_repos INTEGER,
                total_records INTEGER,
                error TEXT,
                started_at TEXT,
                completed_at TEXT,
                created_at TEXT NOT NULL,
                error_counts TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("create happyview_backfill_jobs table: {e}"));

        crate::db::query(
            "CREATE TABLE happyview_backfill_errors (
                job_id TEXT NOT NULL,
                did TEXT NOT NULL,
                collection TEXT,
                phase TEXT NOT NULL,
                kind TEXT NOT NULL,
                message TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 1,
                last_at TEXT NOT NULL,
                PRIMARY KEY (job_id, did, phase)
            )",
        )
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("create happyview_backfill_errors table: {e}"));

        crate::db::query(
            "CREATE TABLE happyview_backfill_repos (
                job_id TEXT NOT NULL,
                did TEXT NOT NULL,
                pds_endpoint TEXT,
                status TEXT NOT NULL DEFAULT 'pending',
                records_fetched INTEGER DEFAULT 0,
                PRIMARY KEY (job_id, did)
            )",
        )
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("create happyview_backfill_repos table: {e}"));

        crate::db::query(
            "INSERT INTO happyview_backfill_jobs \
             (id, collection, did, status, stage, created_at, error_counts) \
             VALUES (?, ?, NULL, 'completed', 'completed', '2026-01-01T00:00:00+00:00', ?)",
        )
        .bind(job_id)
        .bind(collection)
        .bind(error_counts_json)
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("insert backfill job: {e}"));

        test_state_with_pool(pool)
    }

    fn super_auth(state: &AppState) -> UserAuth {
        UserAuth {
            did: "did:plc:admin".to_string(),
            user_id: "admin".to_string(),
            is_super: true,
            permissions: std::collections::HashSet::new(),
            db: state.db.clone(),
            db_backend: state.db_backend,
        }
    }

    async fn insert_error(
        state: &AppState,
        job_id: &str,
        did: &str,
        phase: &str,
        kind: BackfillErrorKind,
    ) {
        crate::db::query(
            "INSERT INTO happyview_backfill_errors \
             (job_id, did, collection, phase, kind, message, attempts, last_at) \
             VALUES (?, ?, NULL, ?, ?, 'boom', 1, '2026-01-01T00:00:00+00:00')",
        )
        .bind(job_id)
        .bind(did)
        .bind(phase)
        .bind(kind.as_str())
        .execute(&state.backfill_db)
        .await
        .unwrap_or_else(|e| panic!("insert backfill error: {e}"));
    }

    #[tokio::test]
    async fn errors_list_reports_exact_counts_and_capped_flag() {
        let state = state_with_errors_job(
            "job-err-1",
            Some("dummy.collection"),
            r#"{"dns_failure":2,"repo_not_found":1}"#,
        )
        .await;
        insert_error(
            &state,
            "job-err-1",
            "did:plc:a",
            "resolve",
            BackfillErrorKind::DnsFailure,
        )
        .await;
        insert_error(
            &state,
            "job-err-1",
            "did:plc:b",
            "resolve",
            BackfillErrorKind::DnsFailure,
        )
        .await;
        insert_error(
            &state,
            "job-err-1",
            "did:plc:c",
            "fetch",
            BackfillErrorKind::RepoNotFound,
        )
        .await;

        let response = backfill_errors_list(
            State(state.clone()),
            Path("job-err-1".to_string()),
            super_auth(&state),
            axum::extract::Query(BackfillErrorsQuery {
                kind: None,
                cursor: None,
                limit: None,
            }),
        )
        .await
        .expect("list errors")
        .0;

        assert_eq!(response.errors.len(), 3);
        assert!(!response.capped);

        let dns = response
            .counts
            .iter()
            .find(|c| c.kind == "dns_failure")
            .expect("dns_failure present");
        assert_eq!(dns.count, 2);
        assert!(dns.retryable);

        let repo = response
            .counts
            .iter()
            .find(|c| c.kind == "repo_not_found")
            .expect("repo_not_found present");
        assert_eq!(repo.count, 1);
        assert!(!repo.retryable);

        // Zero-count kinds are skipped rather than zero-filled.
        assert!(!response.counts.iter().any(|c| c.kind == "timeout"));
    }

    #[tokio::test]
    async fn errors_list_counts_are_live_before_the_first_flush() {
        // `error_counts` is only written by a terminal `ErrorRecorder::flush`,
        // so mid-run it is empty (or, after a crash, stale) while detail rows
        // pile up underneath. Counting the table is what puts an Errors row on
        // the dashboard during the hours a backfill actually takes.
        let state = state_with_errors_job("job-err-live", None, "{}").await;
        for (did, kind) in [
            ("did:plc:a", BackfillErrorKind::DnsFailure),
            ("did:plc:b", BackfillErrorKind::DnsFailure),
            ("did:plc:c", BackfillErrorKind::RepoNotFound),
        ] {
            insert_error(&state, "job-err-live", did, "resolve", kind).await;
        }

        let response = backfill_errors_list(
            State(state.clone()),
            Path("job-err-live".to_string()),
            super_auth(&state),
            axum::extract::Query(BackfillErrorsQuery {
                kind: None,
                cursor: None,
                limit: None,
            }),
        )
        .await
        .expect("list errors")
        .0;

        let dns = response
            .counts
            .iter()
            .find(|c| c.kind == "dns_failure")
            .expect("dns_failure present despite an empty error_counts");
        assert_eq!(dns.count, 2);
        assert_eq!(
            response
                .counts
                .iter()
                .find(|c| c.kind == "repo_not_found")
                .map(|c| c.count),
            Some(1)
        );
    }

    #[tokio::test]
    async fn errors_list_counts_keep_the_json_where_it_exceeds_the_table() {
        // Above `ERROR_DETAIL_CAP` rows stop being written, so the flushed JSON
        // is the only remaining record of how many failures there were. The
        // table must raise a count, never lower one.
        let state =
            state_with_errors_job("job-err-max", None, r#"{"dns_failure":9000,"timeout":1}"#).await;
        insert_error(
            &state,
            "job-err-max",
            "did:plc:a",
            "resolve",
            BackfillErrorKind::DnsFailure,
        )
        .await;
        for did in ["did:plc:b", "did:plc:c"] {
            insert_error(
                &state,
                "job-err-max",
                did,
                "resolve",
                BackfillErrorKind::Timeout,
            )
            .await;
        }

        let response = backfill_errors_list(
            State(state.clone()),
            Path("job-err-max".to_string()),
            super_auth(&state),
            axum::extract::Query(BackfillErrorsQuery {
                kind: None,
                cursor: None,
                limit: None,
            }),
        )
        .await
        .expect("list errors")
        .0;

        let count = |kind: &str| {
            response
                .counts
                .iter()
                .find(|c| c.kind == kind)
                .map(|c| c.count)
        };
        // JSON wins where it is higher...
        assert_eq!(count("dns_failure"), Some(9000));
        // ...and the table wins where it is.
        assert_eq!(count("timeout"), Some(2));
    }

    #[test]
    fn a_retryable_give_up_is_recorded_in_preference_to_a_permanent_one() {
        // Only one give-up per DID reaches the recorder; `retry-failed`
        // selects on retryability, so the retryable one is what keeps the DID
        // reachable by a retry job.
        use crate::admin::backfill_errors::BackfillFailure;

        let failed = |kind: BackfillErrorKind| FetchOutcome::Failed {
            count: 0,
            cursor: None,
            failure: BackfillFailure {
                kind,
                message: String::new(),
                retry_after: None,
            },
        };

        let mut failures = vec![
            (
                "app.bsky.feed.post".to_string(),
                failed(BackfillErrorKind::RepoNotFound),
            ),
            (
                "app.bsky.feed.like".to_string(),
                failed(BackfillErrorKind::Other),
            ),
            (
                "app.bsky.graph.follow".to_string(),
                failed(BackfillErrorKind::Timeout),
            ),
        ];
        retryable_give_ups_first(&mut failures);

        assert_eq!(failures[0].0, "app.bsky.graph.follow");
        // The rest keep their original relative order, so the log stays
        // predictable.
        assert_eq!(failures[1].0, "app.bsky.feed.post");
        assert_eq!(failures[2].0, "app.bsky.feed.like");
    }

    #[tokio::test]
    async fn errors_list_reports_capped_once_the_total_reaches_the_cap() {
        let state = state_with_errors_job(
            "job-err-cap",
            None,
            &format!(r#"{{"dns_failure":{ERROR_DETAIL_CAP}}}"#),
        )
        .await;

        let response = backfill_errors_list(
            State(state.clone()),
            Path("job-err-cap".to_string()),
            super_auth(&state),
            axum::extract::Query(BackfillErrorsQuery {
                kind: None,
                cursor: None,
                limit: None,
            }),
        )
        .await
        .expect("list errors")
        .0;

        assert!(response.capped);
        assert_eq!(response.cap, ERROR_DETAIL_CAP);
    }

    #[tokio::test]
    async fn errors_list_filters_by_kind_and_paginates_by_cursor() {
        let state = state_with_errors_job("job-err-2", None, "{}").await;
        for (did, kind) in [
            ("did:plc:a", BackfillErrorKind::DnsFailure),
            ("did:plc:b", BackfillErrorKind::DnsFailure),
            ("did:plc:c", BackfillErrorKind::RepoNotFound),
        ] {
            insert_error(&state, "job-err-2", did, "resolve", kind).await;
        }

        let response = backfill_errors_list(
            State(state.clone()),
            Path("job-err-2".to_string()),
            super_auth(&state),
            axum::extract::Query(BackfillErrorsQuery {
                kind: Some("dns_failure".to_string()),
                cursor: None,
                limit: Some(1),
            }),
        )
        .await
        .expect("list errors")
        .0;

        assert_eq!(response.errors.len(), 1);
        assert_eq!(response.errors[0].did, "did:plc:a");
        assert_eq!(response.errors[0].kind, "dns_failure");
        assert_eq!(response.cursor.as_deref(), Some("did:plc:a"));

        // Turn the page: the cursor from page 1 must reach the remaining
        // dns_failure row (did:plc:b) and not did:plc:c, which is filtered
        // out by kind, and the second page must terminate the pagination.
        let page2 = backfill_errors_list(
            State(state.clone()),
            Path("job-err-2".to_string()),
            super_auth(&state),
            axum::extract::Query(BackfillErrorsQuery {
                kind: Some("dns_failure".to_string()),
                cursor: response.cursor.clone(),
                limit: Some(1),
            }),
        )
        .await
        .expect("list errors page 2")
        .0;

        assert_eq!(page2.errors.len(), 1);
        assert_eq!(page2.errors[0].did, "did:plc:b");
        assert_eq!(page2.errors[0].kind, "dns_failure");
        assert_eq!(page2.cursor, None, "second page should end the pagination");
    }

    #[tokio::test]
    async fn retry_failed_seeds_new_job_from_retryable_kinds_only() {
        let state = state_with_errors_job(
            "job-retry-1",
            Some("dummy.collection"),
            r#"{"dns_failure":2,"repo_not_found":1}"#,
        )
        .await;
        insert_error(
            &state,
            "job-retry-1",
            "did:plc:a",
            "resolve",
            BackfillErrorKind::DnsFailure,
        )
        .await;
        insert_error(
            &state,
            "job-retry-1",
            "did:plc:b",
            "resolve",
            BackfillErrorKind::DnsFailure,
        )
        .await;
        insert_error(
            &state,
            "job-retry-1",
            "did:plc:c",
            "fetch",
            BackfillErrorKind::RepoNotFound,
        )
        .await;

        let (status, Json(body)) = retry_failed_backfill(
            State(state.clone()),
            super_auth(&state),
            Path("job-retry-1".to_string()),
            Json(RetryFailedBody { kinds: None }),
        )
        .await
        .expect("retry-failed");

        assert_eq!(status, StatusCode::CREATED);
        let new_job_id = body["id"].as_str().expect("id field").to_string();
        assert_ne!(new_job_id, "job-retry-1");

        let (stage,): (String,) =
            crate::db::query_as("SELECT stage FROM happyview_backfill_jobs WHERE id = ?")
                .bind(&new_job_id)
                .fetch_one(&state.backfill_db)
                .await
                .expect("new job row");
        assert_eq!(stage, "resolving_pds");

        let mut repo_dids: Vec<String> = crate::db::query_as::<(String,)>(
            "SELECT did FROM happyview_backfill_repos WHERE job_id = ? ORDER BY did",
        )
        .bind(&new_job_id)
        .fetch_all(&state.backfill_db)
        .await
        .expect("repo rows")
        .into_iter()
        .map(|(did,)| did)
        .collect();
        repo_dids.sort();
        assert_eq!(
            repo_dids,
            vec!["did:plc:a".to_string(), "did:plc:b".to_string()],
            "only the retryable dns_failure DIDs should be re-seeded"
        );
    }

    #[tokio::test]
    async fn retry_failed_honors_explicit_kinds_beyond_the_default_retryable_set() {
        let state = state_with_errors_job(
            "job-retry-2",
            Some("dummy.collection"),
            r#"{"repo_not_found":1}"#,
        )
        .await;
        insert_error(
            &state,
            "job-retry-2",
            "did:plc:a",
            "fetch",
            BackfillErrorKind::RepoNotFound,
        )
        .await;

        let (status, Json(body)) = retry_failed_backfill(
            State(state.clone()),
            super_auth(&state),
            Path("job-retry-2".to_string()),
            Json(RetryFailedBody {
                kinds: Some(vec!["repo_not_found".to_string()]),
            }),
        )
        .await
        .expect("retry-failed with an explicit non-retryable kind");

        assert_eq!(status, StatusCode::CREATED);
        let new_job_id = body["id"].as_str().expect("id field").to_string();
        let (did,): (String,) =
            crate::db::query_as("SELECT did FROM happyview_backfill_repos WHERE job_id = ?")
                .bind(&new_job_id)
                .fetch_one(&state.backfill_db)
                .await
                .expect("repo row");
        assert_eq!(did, "did:plc:a");
    }

    #[tokio::test]
    async fn retry_failed_rejects_when_nothing_retryable() {
        let state = state_with_errors_job(
            "job-retry-3",
            Some("dummy.collection"),
            r#"{"repo_not_found":1}"#,
        )
        .await;
        insert_error(
            &state,
            "job-retry-3",
            "did:plc:a",
            "fetch",
            BackfillErrorKind::RepoNotFound,
        )
        .await;

        let err = retry_failed_backfill(
            State(state.clone()),
            super_auth(&state),
            Path("job-retry-3".to_string()),
            Json(RetryFailedBody { kinds: None }),
        )
        .await
        .expect_err("should reject a job with no retryable failures");

        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[tokio::test]
    async fn retry_failed_returns_not_found_for_unknown_job() {
        let state = state_with_errors_job("job-retry-4", None, "{}").await;

        let err = retry_failed_backfill(
            State(state.clone()),
            super_auth(&state),
            Path("does-not-exist".to_string()),
            Json(RetryFailedBody { kinds: None }),
        )
        .await
        .expect_err("should 404 for an unknown job");

        assert!(matches!(err, AppError::NotFound(_)));
    }
}
