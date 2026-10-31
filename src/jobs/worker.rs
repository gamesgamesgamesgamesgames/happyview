use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use crate::AppState;
use crate::event_log::{EventLog, Severity, log_event};
use crate::lua::scripts;
use crate::plugin::ScriptExecuteOutput;
use crate::repo;
use crate::script::{Invocation, ScriptOrigin, dispatch};

use super::db;

const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// How often each worker settles jobs whose worker stopped (see
/// [`db::recover_expired_jobs`]).
const RECOVER_EVERY: Duration = Duration::from_secs(60);

/// This process's name in a job's `claimed_by`: unique to the process, so
/// two instances on one database never take each other's claims for their
/// own.
fn instance_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        let host = std::env::var("HOSTNAME").unwrap_or_else(|_| "happyview".to_string());
        format!("{host}:{}:{}", std::process::id(), uuid::Uuid::new_v4())
    })
}

/// Start the background job worker. Polls for pending jobs and
/// executes them one at a time per worker; see `spawn_workers`.
pub async fn run_worker(state: AppState) {
    run_worker_as(state, instance_id().to_string()).await
}

/// [`run_worker`], claiming jobs as `owner`. Public so a test can run two
/// instances' workers in one process.
pub async fn run_worker_as(state: AppState, owner: String) {
    run_worker_renewing(state, owner, db::RENEW_EVERY, never()).await
}

/// [`run_worker`] until `stop` holds `true`. A job already running then runs
/// to its end, and its thread is joined, before this returns. Aborting the
/// worker's task instead leaves that thread running, so a runtime dropped
/// right after waits on it, and the thread waits on the runtime.
pub async fn run_worker_until(state: AppState, stop: watch::Receiver<bool>) {
    run_worker_renewing(state, instance_id().to_string(), db::RENEW_EVERY, stop).await
}

/// [`run_worker_until`], claiming jobs as `owner` and renewing each job's
/// lease every `renew_every`, which a test shortens to see a run outlast it.
/// A dropped `stop` sender never stops the worker.
#[doc(hidden)]
pub async fn run_worker_renewing(
    state: AppState,
    owner: String,
    renew_every: Duration,
    mut stop: watch::Receiver<bool>,
) {
    tracing::info!(owner, "job worker started");

    loop {
        if *stop.borrow() {
            return;
        }
        match db::claim_next_job(&state, &owner).await {
            Ok(Some(job)) => {
                tracing::info!(job_id = %job.id, job_type = %job.job_type, attempt = job.attempt, "executing job");
                let lease = db::Lease {
                    owner: owner.clone(),
                    attempt: job.attempt,
                };
                run_on_its_own_thread(&state, job, lease, renew_every).await;
                continue;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!(error = %e, "job worker: failed to claim job");
            }
        }
        tokio::select! {
            () = tokio::time::sleep(POLL_INTERVAL) => {}
            Ok(_) = stop.wait_for(|stop| *stop) => return,
        }
    }
}

/// A stop no one will ever send.
fn never() -> watch::Receiver<bool> {
    watch::channel(false).1
}

/// Run one job on the blocking pool, renewing its lease while it runs. A job
/// has no deadline, so its guest never yields and holds whichever thread runs
/// it until it finishes. On a runtime worker that starves every task queued
/// behind it there, another job worker and the renewal included, so two
/// workers would run their jobs one after the other.
async fn run_on_its_own_thread(
    state: &AppState,
    job: super::Job,
    lease: db::Lease,
    renew_every: Duration,
) {
    let id = job.id.clone();
    let held = super::held::Held::new(&id, lease.clone());
    let renewal = tokio::spawn(renew_while_running(
        state.clone(),
        held.clone(),
        renew_every,
    ));
    let handle = tokio::runtime::Handle::current();
    let run = {
        let state = state.clone();
        tokio::task::spawn_blocking(move || {
            handle.block_on(super::held::run_under(
                held,
                execute_job_under(&state, &job, &lease),
            ))
        })
    };
    if let Err(e) = run.await {
        tracing::error!(job_id = %id, error = %e, "job worker: the job's thread ended abnormally");
    }
    renewal.abort();
}

/// Renew `held`'s lease every `every` until aborted. A lease that can no
/// longer be renewed was lost: the job was queued again or claimed elsewhere.
/// It is marked lost, so the run's writes change nothing, its `should_stop`
/// answers true, and a guest is interrupted at its next host import.
pub(crate) async fn renew_while_running(state: AppState, held: super::held::Held, every: Duration) {
    let id = &held.id;
    loop {
        tokio::time::sleep(every).await;
        match db::renew_lease(&state, id, &held.lease).await {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!(job_id = %id, owner = %held.lease.owner, "job worker: lost the job's lease; stopping its run");
                held.mark_lost();
                return;
            }
            Err(e) => tracing::warn!(job_id = %id, error = %e, "job worker: failed to renew lease"),
        }
    }
}

/// Start `count` job workers (at least one). Each claims jobs independently;
/// `db::claim_next_job` guarantees a job is claimed by exactly one worker.
pub fn spawn_workers(state: AppState, count: usize) -> Vec<tokio::task::JoinHandle<()>> {
    spawn_workers_until(state, count, never())
}

/// [`spawn_workers`], each run by [`run_worker_until`] with `stop`.
pub fn spawn_workers_until(
    state: AppState,
    count: usize,
    stop: watch::Receiver<bool>,
) -> Vec<tokio::task::JoinHandle<()>> {
    let count = count.max(1);
    tracing::info!(count, "starting job workers");
    (0..count)
        .map(|_| tokio::spawn(run_worker_until(state.clone(), stop.clone())))
        .collect()
}

/// Settle jobs whose worker stopped, now and then every [`RECOVER_EVERY`],
/// on a task of its own so busy workers don't hold it up.
pub fn spawn_recovery(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            recover(&state).await;
            tokio::time::sleep(RECOVER_EVERY).await;
        }
    })
}

/// Settle the jobs whose worker stopped, at startup and then every
/// [`RECOVER_EVERY`]. Only expired leases are touched, so
/// this is safe beside other instances' running jobs.
pub async fn recover(state: &AppState) {
    match db::recover_expired_jobs(state).await {
        Ok(0) => {}
        Ok(settled) => tracing::info!(settled, "job worker: settled jobs whose worker stopped"),
        Err(e) => tracing::error!(error = %e, "job worker: failed to recover jobs"),
    }
}

/// Run pending job `id` here and now, as a test fires it, rather than
/// leaving it to a worker. It is held under a lease like any claimed job,
/// so no other instance's recovery takes it while it runs. Returns `false`
/// when the job was not pending.
pub(crate) async fn run_now(state: &AppState, id: &str) -> Result<bool, crate::error::AppError> {
    let owner = instance_id();
    let Some(job) = db::hold_job(state, id, owner).await? else {
        return Ok(false);
    };
    let lease = db::Lease {
        owner: owner.to_string(),
        attempt: job.attempt,
    };
    let held = super::held::Held::new(&job.id, lease.clone());
    let renewal = tokio::spawn(renew_while_running(
        state.clone(),
        held.clone(),
        db::RENEW_EVERY,
    ));
    super::held::run_under(held, execute_job_under(state, &job, &lease)).await;
    renewal.abort();
    Ok(true)
}

async fn execute_job_under(state: &AppState, job: &super::Job, lease: &db::Lease) {
    let backend = state.db_backend;

    log_event(
        &state.db,
        EventLog {
            event_type: "job.started".to_string(),
            severity: Severity::Info,
            actor_did: Some(job.created_by.clone()),
            subject: Some(job.job_type.clone()),
            detail: serde_json::json!({
                "job_id": job.id,
                "job_type": job.job_type,
            }),
        },
        backend,
    )
    .await;

    if super::native::is_native(&job.job_type) {
        let outcome = match super::native::execute(state, job).await {
            super::native::NativeOutcome::Completed(v) => JobOutcome::Completed(v),
            super::native::NativeOutcome::Failed(e) => JobOutcome::Failed(e),
        };
        finalize(state, job, lease, outcome).await;
        return;
    }

    let lookup = scripts::ParsedTrigger::new(scripts::TriggerKind::JobRun, &job.job_type);
    let project = state.project.load_full();
    let script = match state.scripts.resolve_in(&project, &lookup).await {
        Some(s) => s,
        None => {
            let error = format!("no script found for trigger: {}", lookup.id());
            finalize(state, job, lease, JobOutcome::Failed(error)).await;
            return;
        }
    };

    let (claims, pds_auth) = if job.inherit_auth {
        let pds_auth = if let Some(auth) = repo::test_pds_auth(state) {
            auth
        } else if let (Some(api_client_id), Some(dpop_key_id)) =
            (&job.api_client_id, &job.dpop_key_id)
        {
            let encryption_key = match state.config.token_encryption_key.as_ref() {
                Some(k) => *k,
                None => {
                    let error = "inherit_auth with DPoP requires TOKEN_ENCRYPTION_KEY";
                    finalize(state, job, lease, JobOutcome::Failed(error.to_string())).await;
                    return;
                }
            };
            repo::PdsAuth::Dpop {
                api_client_id: api_client_id.clone(),
                dpop_key_id: dpop_key_id.clone(),
                encryption_key,
            }
        } else {
            match repo::get_oauth_session(state, &job.created_by).await {
                Ok(session) => repo::PdsAuth::OAuth(Arc::new(session)),
                Err(e) => {
                    let error = format!("failed to obtain PDS auth for {}: {e}", job.created_by);
                    finalize(state, job, lease, JobOutcome::Failed(error)).await;
                    return;
                }
            }
        };
        (
            Some(Arc::new(crate::auth::Claims::internal(
                job.created_by.clone(),
            ))),
            Some(Arc::new(pds_auth)),
        )
    } else {
        (None, None)
    };

    // A job that did not inherit its creator's auth has nothing to act as, so
    // its library calls carry no session at all.
    let caller_session = claims.zip(pds_auth).map(|(claims, pds_auth)| {
        Arc::new(crate::plugin::caller::CallerSession {
            did: job.created_by.clone(),
            delegate_did: None,
            claims,
            pds_auth,
            app_state: state.clone(),
        })
    });
    let has_pds_auth = caller_session.is_some();

    let trigger = match scripts::trigger_of(&script, Some(&job.id)) {
        Ok(trigger) => trigger,
        Err(error) => {
            finalize(state, job, lease, JobOutcome::Failed(error)).await;
            return;
        }
    };
    let run = dispatch(
        state,
        &Invocation {
            trigger_id: &script.id,
            trigger,
            language: &script.script_type,
            source: &script.body,
            input: &job.input,
            caller_did: Some(&job.created_by),
            has_pds_auth,
            // A job answers to whoever enqueued it rather than to a request, so
            // it has no method, collection, parameters, delegation or space of
            // its own.
            method: None,
            collection: None,
            params: None,
            delegate_did: None,
            space: None,
            origin: ScriptOrigin::of(&script),
        },
        caller_session,
    )
    .await;

    // A job's result is whatever the script returned, whatever kind of value
    // that was: the row has one column for it and nothing reads a job's
    // outcome by the shape of its value.
    let outcome = match run {
        Ok(ScriptExecuteOutput::Returned { value, .. }) => JobOutcome::Completed(value),
        Ok(ScriptExecuteOutput::Error {
            kind, raw, chunk, ..
        }) => JobOutcome::Failed(scripts::failure_text(
            kind,
            crate::error::file_of(chunk).as_deref(),
            &raw,
        )),
        Err(e) => JobOutcome::Failed(e.to_string()),
    };
    finalize(state, job, lease, outcome).await;
}

/// What a job's body produced, before pause/cancel is taken into account.
pub(crate) enum JobOutcome {
    Completed(serde_json::Value),
    Failed(String),
}

/// Apply pause/cancel semantics and record the terminal state.
///
/// Shared by the Lua and native paths so the two cannot drift apart on what
/// `pausing` and `cancelling` mean.
async fn finalize(state: &AppState, job: &super::Job, lease: &db::Lease, outcome: JobOutcome) {
    let backend = state.db_backend;
    let stop = db::should_stop(state, &job.id).await;

    let (recorded, event_type, severity, detail) = match (stop, &outcome) {
        (Some("pausing"), JobOutcome::Failed(error)) => {
            let recorded = db::finish(state, &job.id, lease, db::Finish::Paused)
                .await
                .unwrap_or(false);
            tracing::info!(job_id = %job.id, %error, "job paused (error during stop)");
            (
                recorded,
                "job.paused",
                Severity::Info,
                serde_json::json!({ "job_id": job.id }),
            )
        }
        (Some("pausing"), JobOutcome::Completed(_)) => {
            let recorded = db::finish(state, &job.id, lease, db::Finish::Paused)
                .await
                .unwrap_or(false);
            tracing::info!(job_id = %job.id, "job paused");
            (
                recorded,
                "job.paused",
                Severity::Info,
                serde_json::json!({ "job_id": job.id }),
            )
        }
        (Some("cancelling"), JobOutcome::Failed(error)) => {
            let recorded = db::finish(state, &job.id, lease, db::Finish::Cancelled)
                .await
                .unwrap_or(false);
            tracing::info!(job_id = %job.id, %error, "job cancelled (error during stop)");
            (
                recorded,
                "job.cancelled",
                Severity::Info,
                serde_json::json!({ "job_id": job.id }),
            )
        }
        (Some("cancelling"), JobOutcome::Completed(_)) => {
            let recorded = db::finish(state, &job.id, lease, db::Finish::Cancelled)
                .await
                .unwrap_or(false);
            tracing::info!(job_id = %job.id, "job cancelled");
            (
                recorded,
                "job.cancelled",
                Severity::Info,
                serde_json::json!({ "job_id": job.id }),
            )
        }
        (_, JobOutcome::Completed(result)) => {
            let recorded = db::finish(state, &job.id, lease, db::Finish::Completed(result))
                .await
                .unwrap_or(false);
            tracing::info!(job_id = %job.id, "job completed");
            (
                recorded,
                "job.completed",
                Severity::Info,
                serde_json::json!({ "job_id": job.id, "result": result }),
            )
        }
        (_, JobOutcome::Failed(error)) => {
            // Not necessarily a script: native jobs fail here too.
            tracing::error!(job_id = %job.id, %error, "job failed");
            let recorded = db::finish(state, &job.id, lease, db::Finish::Failed(error))
                .await
                .unwrap_or(false);
            (
                recorded,
                "job.failed",
                Severity::Error,
                serde_json::json!({ "job_id": job.id, "error": error }),
            )
        }
    };

    // A run that lost its lease recorded nothing: the run that took the job
    // over records the job's outcome, and logs it.
    if !recorded {
        tracing::warn!(job_id = %job.id, "job worker: the job was taken over; its outcome here is discarded");
        return;
    }

    log_event(
        &state.db,
        EventLog {
            event_type: event_type.to_string(),
            severity,
            actor_did: Some(job.created_by.clone()),
            subject: Some(job.job_type.clone()),
            detail,
        },
        backend,
    )
    .await;
}
