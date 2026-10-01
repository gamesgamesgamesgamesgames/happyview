use std::sync::Arc;
use std::time::Duration;

use crate::AppState;
use crate::event_log::{EventLog, Severity, log_event};
use crate::lua::scripts;
use crate::plugin::ScriptExecuteOutput;
use crate::repo;
use crate::script::{Invocation, Trigger, dispatch};

use super::db;

const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Start the background job worker. Polls for pending jobs and
/// executes them one at a time.
pub async fn run_worker(state: AppState) {
    tracing::info!("job worker started");

    loop {
        match db::claim_next_job(&state).await {
            Ok(Some(job)) => {
                tracing::info!(job_id = %job.id, job_type = %job.job_type, "executing job");
                execute_job(&state, &job).await;
            }
            Ok(None) => {
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            Err(e) => {
                tracing::error!(error = %e, "job worker: failed to claim job");
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
    }
}

/// Resume jobs that were interrupted by a server restart.
pub async fn resume_interrupted_jobs(state: &AppState) {
    let jobs = db::find_interrupted_jobs(state).await;

    for job in jobs {
        match job.status.as_str() {
            "cancelling" => {
                tracing::info!(job_id = %job.id, "finalising cancelled job from previous run");
                let _ = db::set_status(state, &job.id, "cancelled").await;
            }
            "pausing" => {
                tracing::info!(job_id = %job.id, "finalising paused job from previous run");
                let _ = db::set_status(state, &job.id, "paused").await;
            }
            "running" => {
                tracing::info!(job_id = %job.id, "re-queuing interrupted job");
                let _ = db::set_status(state, &job.id, "pending").await;
            }
            _ => {}
        }
    }
}

async fn execute_job(state: &AppState, job: &super::Job) {
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
        finalize(state, job, outcome).await;
        return;
    }

    let trigger_id = format!("job.run:{}", job.job_type);
    let script = match scripts::resolve_native(state, &trigger_id).await {
        Some(s) => s,
        None => {
            let error = format!("no script found for trigger: {trigger_id}");
            tracing::error!(job_id = %job.id, %error);
            let _ = db::set_error(state, &job.id, &error).await;
            log_event(
                &state.db,
                EventLog {
                    event_type: "job.failed".to_string(),
                    severity: Severity::Error,
                    actor_did: Some(job.created_by.clone()),
                    subject: Some(job.job_type.clone()),
                    detail: serde_json::json!({
                        "job_id": job.id,
                        "error": error,
                    }),
                },
                backend,
            )
            .await;
            return;
        }
    };

    let (claims, pds_auth) = if job.inherit_auth {
        let pds_auth = if let (Some(api_client_id), Some(dpop_key_id)) =
            (&job.api_client_id, &job.dpop_key_id)
        {
            let encryption_key = match state.config.token_encryption_key.as_ref() {
                Some(k) => *k,
                None => {
                    let error = "inherit_auth with DPoP requires TOKEN_ENCRYPTION_KEY";
                    let _ = db::set_error(state, &job.id, error).await;
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
                    tracing::error!(job_id = %job.id, %error);
                    let _ = db::set_error(state, &job.id, &error).await;
                    log_event(
                        &state.db,
                        EventLog {
                            event_type: "job.failed".to_string(),
                            severity: Severity::Error,
                            actor_did: Some(job.created_by.clone()),
                            subject: Some(job.job_type.clone()),
                            detail: serde_json::json!({
                                "job_id": job.id,
                                "error": error,
                            }),
                        },
                        backend,
                    )
                    .await;
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

    let run = dispatch(
        state,
        &Invocation {
            trigger_id: &trigger_id,
            trigger: Trigger::Job { id: &job.id },
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
        },
        caller_session,
    )
    .await;

    // A job's result is whatever the script returned, whatever kind of value
    // that was: the row has one column for it and nothing reads a job's
    // outcome by the shape of its value.
    let outcome = match run {
        Ok(ScriptExecuteOutput::Returned { value, .. }) => JobOutcome::Completed(value),
        Ok(ScriptExecuteOutput::Error { kind, raw, .. }) => {
            JobOutcome::Failed(scripts::failure_text(kind, &raw))
        }
        Err(e) => JobOutcome::Failed(e.to_string()),
    };
    finalize(state, job, outcome).await;
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
async fn finalize(state: &AppState, job: &super::Job, outcome: JobOutcome) {
    let backend = state.db_backend;
    let stop = db::should_stop(state, &job.id).await;

    let (event_type, severity, detail) = match (stop, &outcome) {
        (Some("pausing"), JobOutcome::Failed(error)) => {
            let _ = db::set_status(state, &job.id, "paused").await;
            tracing::info!(job_id = %job.id, %error, "job paused (error during stop)");
            (
                "job.paused",
                Severity::Info,
                serde_json::json!({ "job_id": job.id }),
            )
        }
        (Some("pausing"), JobOutcome::Completed(_)) => {
            let _ = db::set_status(state, &job.id, "paused").await;
            tracing::info!(job_id = %job.id, "job paused");
            (
                "job.paused",
                Severity::Info,
                serde_json::json!({ "job_id": job.id }),
            )
        }
        (Some("cancelling"), JobOutcome::Failed(error)) => {
            let _ = db::set_status(state, &job.id, "cancelled").await;
            tracing::info!(job_id = %job.id, %error, "job cancelled (error during stop)");
            (
                "job.cancelled",
                Severity::Info,
                serde_json::json!({ "job_id": job.id }),
            )
        }
        (Some("cancelling"), JobOutcome::Completed(_)) => {
            let _ = db::set_status(state, &job.id, "cancelled").await;
            tracing::info!(job_id = %job.id, "job cancelled");
            (
                "job.cancelled",
                Severity::Info,
                serde_json::json!({ "job_id": job.id }),
            )
        }
        (_, JobOutcome::Completed(result)) => {
            let _ = db::set_result(state, &job.id, result).await;
            tracing::info!(job_id = %job.id, "job completed");
            (
                "job.completed",
                Severity::Info,
                serde_json::json!({ "job_id": job.id, "result": result }),
            )
        }
        (_, JobOutcome::Failed(error)) => {
            // Not necessarily a script: native jobs fail here too.
            tracing::error!(job_id = %job.id, %error, "job failed");
            let _ = db::set_error(state, &job.id, error).await;
            (
                "job.failed",
                Severity::Error,
                serde_json::json!({ "job_id": job.id, "error": error }),
            )
        }
    };

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
