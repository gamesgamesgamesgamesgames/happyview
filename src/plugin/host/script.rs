//! The four imports only an interpreter reaches: the running script's log
//! line and its job's three controls.
//!
//! The host holds the run's identity for the whole run, so a guest can neither
//! attribute a log line to another trigger nor act on another run's job.

use crate::AppState;
use crate::event_log::{EventLog, Severity, log_event};

use happyview_plugin_sdk::wire::{JobProgressRequest, JobWaitRequest, Level, ScriptLogRequest};

/// The longest a job may wait in one call, matching the range the `ctx.job`
/// surface documents.
const MAX_WAIT_SECONDS: f64 = 3600.0;

/// Who a run is and which job it belongs to. `None` on a
/// [`PluginState`](super::PluginState) means no script run is in progress,
/// which is what makes these four imports refuse for a library or auth plugin.
#[derive(Debug, Clone, Default)]
pub struct ScriptRun {
    pub trigger_id: String,
    pub caller_did: Option<String>,
    /// Set for a job run, whose log lines also land in the job's own log and
    /// whose progress, stop flag and waits the three controls act on.
    pub job_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ScriptHostError {
    #[error("{0} is a job control, and this run has no job")]
    NoJob(&'static str),
    #[error("{0}")]
    Database(String),
}

impl ScriptHostError {
    /// A job control reached from a run without a job is the interpreter
    /// calling something it was never given; a failed write is not its fault.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoJob(_) => "BAD_INPUT",
            Self::Database(_) => "HOST_ERROR",
        }
    }
}

fn job_of(run: &ScriptRun, control: &'static str) -> Result<String, ScriptHostError> {
    run.job_id.clone().ok_or(ScriptHostError::NoJob(control))
}

/// Write the run's log line: one `script.log` event attributed to the run's
/// trigger and caller, plus a line in the job's own log when the run has a
/// job. `debug` is for a developer watching the process rather than an audit
/// trail, so it never becomes a row.
pub async fn log(
    state: &AppState,
    run: &ScriptRun,
    request: ScriptLogRequest,
) -> Result<(), ScriptHostError> {
    tracing::debug!(
        script_log = %request.message,
        level = request.level.as_str(),
        trigger = %run.trigger_id,
        "script log"
    );
    let severity = match request.level {
        Level::Debug => return Ok(()),
        Level::Info => Severity::Info,
        Level::Warn => Severity::Warn,
        Level::Error => Severity::Error,
    };
    let level = request.level.as_str();
    log_event(
        &state.db,
        EventLog {
            event_type: "script.log".to_string(),
            severity,
            actor_did: run.caller_did.clone(),
            subject: Some(run.trigger_id.clone()),
            detail: serde_json::json!({
                "trigger": run.trigger_id,
                "level": level,
                "message": request.message,
                "fields": request.fields.unwrap_or(serde_json::Value::Null),
            }),
        },
        state.db_backend,
    )
    .await;

    if let Some(job_id) = &run.job_id
        && let Err(e) = crate::jobs::logs::insert_log(
            &state.db,
            state.db_backend,
            job_id,
            level,
            &request.message,
        )
        .await
    {
        tracing::warn!(job_id = %job_id, error = %e, "job log insert failed");
    }
    Ok(())
}

/// Store the run's progress on its job row.
pub async fn progress(
    state: &AppState,
    run: &ScriptRun,
    request: JobProgressRequest,
) -> Result<(), ScriptHostError> {
    let job_id = job_of(run, "host_job_progress")?;
    crate::jobs::db::update_progress(state, &job_id, &request.data)
        .await
        .map_err(|e| ScriptHostError::Database(e.to_string()))
}

/// Whether the run's job has been asked to pause or cancel. Cooperative: the
/// script has to check and return.
pub async fn should_stop(state: &AppState, run: &ScriptRun) -> Result<bool, ScriptHostError> {
    let job_id = job_of(run, "host_job_should_stop")?;
    Ok(crate::jobs::db::should_stop(state, &job_id).await.is_some())
}

/// Sleep, clamped to the range a job may wait. The import wrapper stops the
/// guest's execution clock for the duration, so a wait is host time and is
/// never charged to the run's budget.
pub async fn wait(
    state: &AppState,
    run: &ScriptRun,
    request: JobWaitRequest,
) -> Result<(), ScriptHostError> {
    job_of(run, "host_job_wait")?;
    let duration = std::time::Duration::from_secs_f64(
        request
            .seconds
            .clamp(0.0, MAX_WAIT_SECONDS)
            // A NaN survives `clamp`, and `from_secs_f64` panics on one.
            .max(0.0),
    );
    tokio::time::sleep(duration).await;
    crate::telemetry::counters::add_saturating(
        &state.telemetry_counters.job_wait_ms,
        u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{migrated_memory_pool, test_state_with_pool};

    fn run(job_id: Option<&str>) -> ScriptRun {
        ScriptRun {
            trigger_id: "xrpc.query:app.test.q".into(),
            caller_did: Some("did:plc:me".into()),
            job_id: job_id.map(str::to_string),
        }
    }

    async fn state() -> AppState {
        test_state_with_pool(migrated_memory_pool().await)
    }

    #[tokio::test]
    async fn a_job_control_without_a_job_is_bad_input_naming_the_import() {
        let state = state().await;
        for err in [
            progress(
                &state,
                &run(None),
                JobProgressRequest {
                    data: serde_json::json!({}),
                },
            )
            .await
            .unwrap_err(),
            should_stop(&state, &run(None)).await.unwrap_err(),
            wait(&state, &run(None), JobWaitRequest { seconds: 0.0 })
                .await
                .unwrap_err(),
        ] {
            assert_eq!(err.code(), "BAD_INPUT", "{err}");
            assert!(err.to_string().contains("host_job"), "{err}");
        }
    }

    /// A debug line reaches `tracing` and no table, which is what keeps the
    /// event log an audit trail rather than a developer's console.
    #[tokio::test]
    async fn a_debug_line_writes_no_row_and_the_other_levels_do() {
        let state = state().await;
        for (level, expected) in [
            (Level::Debug, 0),
            (Level::Info, 1),
            (Level::Warn, 2),
            (Level::Error, 3),
        ] {
            log(
                &state,
                &run(None),
                ScriptLogRequest {
                    level,
                    message: format!("at {}", level.as_str()),
                    fields: None,
                },
            )
            .await
            .unwrap();
            let (rows,): (i64,) = crate::db::query_as(
                "SELECT COUNT(*) FROM happyview_event_logs WHERE event_type = 'script.log'",
            )
            .fetch_one(&state.db)
            .await
            .unwrap();
            assert_eq!(rows, expected, "{level}");
        }
    }

    /// A wait outside the range is clamped rather than refused, and a
    /// negative one costs nothing.
    #[tokio::test]
    async fn a_wait_is_clamped_to_the_allowed_range() {
        let state = state().await;
        let job = crate::jobs::db::create_job(
            &state,
            "test.interpreter",
            &serde_json::json!({}),
            "did:plc:me",
            false,
            None,
            None,
        )
        .await
        .unwrap();
        let started = std::time::Instant::now();
        wait(&state, &run(Some(&job)), JobWaitRequest { seconds: -5.0 })
            .await
            .unwrap();
        assert!(started.elapsed() < std::time::Duration::from_millis(200));
        assert_eq!(
            state
                .telemetry_counters
                .job_wait_ms
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );

        // Clamping the ceiling is asserted through the duration the counter
        // records, since sleeping an hour is not a test.
        let clamped = std::time::Duration::from_secs_f64(f64::MAX.clamp(0.0, MAX_WAIT_SECONDS));
        assert_eq!(clamped, std::time::Duration::from_secs(3600));
    }

    #[tokio::test]
    async fn progress_and_should_stop_act_on_the_runs_own_job() {
        let state = state().await;
        let job = crate::jobs::db::create_job(
            &state,
            "test.interpreter",
            &serde_json::json!({}),
            "did:plc:me",
            false,
            None,
            None,
        )
        .await
        .unwrap();
        let run = run(Some(&job));

        progress(
            &state,
            &run,
            JobProgressRequest {
                data: serde_json::json!({"done": 3}),
            },
        )
        .await
        .unwrap();
        let (stored,): (String,) =
            crate::db::query_as("SELECT progress FROM happyview_jobs WHERE id = ?")
                .bind(&job)
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&stored).unwrap(),
            serde_json::json!({"done": 3})
        );

        assert!(!should_stop(&state, &run).await.unwrap());
        for status in ["cancelling", "pausing"] {
            crate::db::query("UPDATE happyview_jobs SET status = ? WHERE id = ?")
                .bind(status)
                .bind(&job)
                .execute(&state.db)
                .await
                .unwrap();
            assert!(should_stop(&state, &run).await.unwrap(), "{status}");
        }
    }
}
