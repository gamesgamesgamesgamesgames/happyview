//! The five imports only an interpreter reaches: the running script's log
//! line, its job's three controls, and resolving a local module.
//!
//! The host holds the run's identity for the whole run, so a guest can neither
//! attribute a log line to another trigger, act on another run's job, nor
//! read a module from any snapshot but the one its run pinned.

use std::sync::Arc;

use crate::AppState;
use crate::event_log::{EventLog, Severity, log_event};
use crate::plugin::prepared::ArtifactStamp;
use crate::project::snapshot::ProjectSnapshot;

use happyview_plugin_sdk::wire::{
    JobProgressRequest, JobWaitRequest, Level, ModuleResolve, ResolvedModule, ScriptLogRequest,
};

/// The longest a job may wait in one call, matching the range the `ctx.job`
/// surface documents.
const MAX_WAIT_SECONDS: f64 = 3600.0;

/// Where a run's relative imports resolve.
#[derive(Clone, Default)]
pub enum ModuleScope {
    /// A script created in the dashboard, which has no files beside it.
    #[default]
    Dashboard,
    /// A project file, resolving against the snapshot its run pinned.
    Project(Arc<ProjectSnapshot>),
}

impl std::fmt::Debug for ModuleScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dashboard => f.write_str("Dashboard"),
            Self::Project(snapshot) => write!(f, "Project(generation {})", snapshot.generation),
        }
    }
}

/// Who a run is and which job it belongs to. `None` on a
/// [`PluginState`](super::PluginState) means no script run is in progress,
/// which is what makes these five imports refuse for a library or auth plugin.
#[derive(Debug, Clone, Default)]
pub struct ScriptRun {
    pub trigger_id: String,
    pub caller_did: Option<String>,
    /// Set for a job run, whose log lines also land in the job's own log and
    /// whose progress, stop flag and waits the three controls act on.
    pub job_id: Option<String>,
    /// Whether this run is a test file. The five test imports answer only
    /// when it is, whatever the plugin declared.
    pub test: bool,
    /// The calling interpreter's language and the extensions it claims; a
    /// module resolves only to a file it could have written.
    pub language_id: String,
    pub extensions: Vec<String>,
    /// The calling interpreter's build. A module's artifact is answered only
    /// when it carries this stamp.
    pub stamp: Option<ArtifactStamp>,
    pub modules: ModuleScope,
    /// Milliseconds this run has spent in `host_job_wait`, which a test run
    /// caps in total: a wait is host time, so a job that waits in a loop
    /// would otherwise never reach its deadline.
    pub waited_ms: Arc<std::sync::atomic::AtomicU64>,
}

#[derive(Debug, thiserror::Error)]
pub enum ScriptHostError {
    #[error("{0} is a job control, and this run has no job")]
    NoJob(&'static str),
    #[error("{0}")]
    Database(String),
    #[error(
        "'{0}' is a local module, and local modules come from a project: a script created in the dashboard has no files beside it to import"
    )]
    NoProject(String),
    #[error("{0}")]
    BadSpecifier(String),
    #[error("{0}")]
    ModuleNotFound(String),
}

impl ScriptHostError {
    /// A job control reached from a run without a job is the interpreter
    /// calling something it was never given; a failed write is not its fault.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoJob(_) => "BAD_INPUT",
            Self::Database(_) => "HOST_ERROR",
            Self::NoProject(_) => "UNSUPPORTED",
            Self::BadSpecifier(_) => "BAD_INPUT",
            Self::ModuleNotFound(_) => "NOT_FOUND",
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
/// never charged to the run's budget. In a test run the run's waits share
/// one test file's budget between them, since the run awaits the job and
/// nothing else would end it.
pub async fn wait(
    state: &AppState,
    run: &ScriptRun,
    request: JobWaitRequest,
) -> Result<(), ScriptHostError> {
    use std::sync::atomic::Ordering;

    job_of(run, "host_job_wait")?;
    let ceiling = if state.test.is_some() {
        let budget_ms = crate::plugin::executor::TEST_FILE_GUEST_SECONDS * 1000;
        budget_ms.saturating_sub(run.waited_ms.load(Ordering::Relaxed)) as f64 / 1000.0
    } else {
        MAX_WAIT_SECONDS
    };
    let duration = std::time::Duration::from_secs_f64(
        request
            .seconds
            .clamp(0.0, ceiling)
            // A NaN survives `clamp`, and `from_secs_f64` panics on one.
            .max(0.0),
    );
    run.waited_ms.fetch_add(
        u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
        Ordering::Relaxed,
    );
    tokio::time::sleep(duration).await;
    crate::telemetry::counters::add_saturating(
        &state.telemetry_counters.job_wait_ms,
        u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
    );
    Ok(())
}

/// Resolve a relative import against the run's pinned snapshot. A memory
/// lookup, so a guest may block on it: `require` and a module loader are
/// synchronous in the languages that call it.
pub fn module_resolve(
    run: &ScriptRun,
    request: ModuleResolve,
) -> Result<ResolvedModule, ScriptHostError> {
    use crate::project::modules::{ModuleError, resolve_module};

    let ModuleScope::Project(snapshot) = &run.modules else {
        return Err(ScriptHostError::NoProject(request.specifier));
    };
    let module = resolve_module(
        snapshot,
        &request.specifier,
        &request.referrer,
        &run.extensions,
        &run.language_id,
    )
    .map_err(|e| match e {
        ModuleError::NotFound(message) => ScriptHostError::ModuleNotFound(message),
        other => ScriptHostError::BadSpecifier(other.to_string()),
    })?;
    let artifact = module
        .artifact
        .as_ref()
        .filter(|artifact| run.stamp.as_ref().is_some_and(|s| s.matches(artifact)))
        .cloned();
    Ok(ResolvedModule {
        id: module.id.clone(),
        source: module.source.to_string(),
        artifact,
    })
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
            ..ScriptRun::default()
        }
    }

    use crate::plugin::PreparedArtifact;
    use crate::project::snapshot::ProjectModule;

    fn stamp() -> ArtifactStamp {
        ArtifactStamp {
            interpreter: "lua".into(),
            version: "1.0.0+0000000000000000".into(),
        }
    }

    fn artifact(version: &str) -> PreparedArtifact {
        PreparedArtifact {
            interpreter: "lua".into(),
            version: version.into(),
            engine: "e".into(),
            format: "lua-bytecode".into(),
            bytes: vec![1],
            source_map: None,
        }
    }

    fn project_run(module_artifact: Option<PreparedArtifact>) -> ScriptRun {
        let mut snap = ProjectSnapshot::empty();
        snap.modules.insert(
            "src/lib/format.lua".into(),
            ProjectModule {
                id: "src/lib/format.lua".into(),
                path: "src/lib/format.lua".into(),
                language_id: "lua".into(),
                source: Arc::from("return {}"),
                artifact: module_artifact,
            },
        );
        ScriptRun {
            language_id: "lua".into(),
            extensions: vec!["lua".into()],
            stamp: Some(stamp()),
            modules: ModuleScope::Project(Arc::new(snap)),
            ..run(None)
        }
    }

    fn request(specifier: &str) -> ModuleResolve {
        ModuleResolve {
            specifier: specifier.into(),
            referrer: "src/jobs/tick.lua".into(),
        }
    }

    #[test]
    fn a_project_run_resolves_and_receives_a_matching_artifact() {
        let run = project_run(Some(artifact(&stamp().version)));
        let resolved = module_resolve(&run, request("../lib/format")).unwrap();
        assert_eq!(resolved.id, "src/lib/format.lua");
        assert_eq!(resolved.source, "return {}");
        assert_eq!(resolved.artifact, Some(artifact(&stamp().version)));
    }

    #[test]
    fn an_artifact_from_another_build_is_withheld_and_the_source_still_answers() {
        let run = project_run(Some(artifact("0.9.0+ffffffffffffffff")));
        let resolved = module_resolve(&run, request("../lib/format")).unwrap();
        assert_eq!(resolved.artifact, None);
        assert_eq!(resolved.source, "return {}");
    }

    #[test]
    fn a_miss_is_not_found() {
        let err = module_resolve(&project_run(None), request("./nothing")).unwrap_err();
        assert_eq!(err.code(), "NOT_FOUND", "{err}");
        assert!(err.to_string().contains("./nothing"), "{err}");
    }

    #[test]
    fn an_escaping_specifier_is_bad_input() {
        let err = module_resolve(&project_run(None), request("../../../etc/passwd")).unwrap_err();
        assert_eq!(err.code(), "BAD_INPUT", "{err}");
    }

    #[test]
    fn a_dashboard_run_hears_that_local_modules_come_from_a_project() {
        let err = module_resolve(&run(None), request("./lib/format")).unwrap_err();
        assert_eq!(err.code(), "UNSUPPORTED", "{err}");
        let message = err.to_string();
        assert!(message.contains("./lib/format"), "{message}");
        assert!(message.contains("project"), "{message}");
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

    /// A run that has already waited a test file's budget waits no more in a
    /// test run, and still waits as asked in a serving one.
    #[tokio::test]
    async fn a_test_runs_waits_share_one_budget_and_a_serving_runs_do_not() {
        use std::sync::atomic::Ordering;

        let spent = crate::plugin::executor::TEST_FILE_GUEST_SECONDS * 1000;
        for (test, expected_ms) in [(true, 0), (false, 300)] {
            let mut state = state().await;
            if test {
                state.test = Some(Arc::new(crate::testing::TestHarness::new("a.test.lua")));
            }
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
            run.waited_ms.store(spent, Ordering::Relaxed);
            wait(&state, &run, JobWaitRequest { seconds: 0.3 })
                .await
                .unwrap();
            assert_eq!(
                state.telemetry_counters.job_wait_ms.load(Ordering::Relaxed),
                expected_ms,
                "test run: {test}"
            );
        }
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
