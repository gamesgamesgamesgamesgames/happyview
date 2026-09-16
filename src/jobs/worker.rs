use std::sync::Arc;
use std::time::Duration;

use mlua::LuaSerdeExt;

use crate::AppState;
use crate::db::adapt_sql;
use crate::event_log::{EventLog, Severity, log_event};
use crate::lua::{sandbox, scripts};
use crate::repo;

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
    let script = match scripts::resolve(state, &trigger_id).await {
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

    let (claims, pds_auth_arc) = if job.inherit_auth {
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

    let lua = match sandbox::create_sandbox() {
        Ok(l) => l,
        Err(e) => {
            let error = format!("failed to create Lua VM: {e}");
            let _ = db::set_error(state, &job.id, &error).await;
            return;
        }
    };

    lua.remove_hook();

    let state_arc = Arc::new(state.clone());

    if let Err(e) = crate::lua::db_api::register_db_api(&lua, state_arc.clone()) {
        let _ = db::set_error(state, &job.id, &format!("db api: {e}")).await;
        return;
    }
    if let Err(e) = crate::lua::http_api::register_http_api(&lua, state_arc.clone()) {
        let _ = db::set_error(state, &job.id, &format!("http api: {e}")).await;
        return;
    }
    if let Err(e) = crate::lua::xrpc_api::register_xrpc_api(
        &lua,
        state_arc.clone(),
        Some(job.created_by.clone()),
    ) {
        let _ = db::set_error(state, &job.id, &format!("xrpc api: {e}")).await;
        return;
    }
    if let Err(e) = crate::lua::atproto_api::register_atproto_api(
        &lua,
        state_arc.clone(),
        Some(&job.created_by),
    ) {
        let _ = db::set_error(state, &job.id, &format!("atproto api: {e}")).await;
        return;
    }
    if let Err(e) = crate::lua::spaces_api::register_spaces_write_api(
        &lua,
        state_arc.clone(),
        Some(&job.created_by),
    ) {
        let _ = db::set_error(state, &job.id, &format!("spaces write api: {e}")).await;
        return;
    }
    if let Err(e) = crate::lua::linked_repos_api::register_linked_repos_api(&lua, state_arc.clone())
    {
        let _ = db::set_error(state, &job.id, &format!("linked repos api: {e}")).await;
        return;
    }
    if let (Some(c), Some(p)) = (&claims, &pds_auth_arc)
        && let Err(e) = crate::lua::atproto_api::register_atproto_blob_api(
            &lua,
            state_arc.clone(),
            c.clone(),
            p.clone(),
        )
    {
        let _ = db::set_error(state, &job.id, &format!("blob api: {e}")).await;
        return;
    }
    if let Err(e) = crate::lua::jobs_api::register_jobs_api(
        &lua,
        state_arc.clone(),
        Some(crate::lua::jobs_api::JobsCaller {
            did: job.created_by.clone(),
            api_client_id: job.api_client_id.clone(),
            dpop_key_id: job.dpop_key_id.clone(),
        }),
    ) {
        let _ = db::set_error(state, &job.id, &format!("jobs api: {e}")).await;
        return;
    }
    if let Err(e) =
        crate::lua::record::register_record_api(&lua, state_arc.clone(), claims, pds_auth_arc, None)
    {
        let _ = db::set_error(state, &job.id, &format!("record api: {e}")).await;
        return;
    }
    if let Err(e) = crate::lua::scripts::register_log_event_api(
        &lua,
        &state_arc,
        &trigger_id,
        Some(&job.created_by),
    ) {
        let _ = db::set_error(state, &job.id, &format!("log api: {e}")).await;
        return;
    }
    if let Err(e) = crate::lua::jobs_api::register_job_context(
        &lua,
        state_arc.clone(),
        job.id.clone(),
        job.input.clone(),
    ) {
        let _ = db::set_error(state, &job.id, &format!("job context: {e}")).await;
        return;
    }

    let env_vars = load_env_vars(&state.db, backend).await;
    if let Err(e) = crate::lua::context::set_env_context(&lua, &env_vars) {
        let _ = db::set_error(state, &job.id, &format!("env context: {e}")).await;
        return;
    }

    if let Err(e) = lua.globals().set("caller_did", job.created_by.as_str()) {
        let _ = db::set_error(state, &job.id, &format!("caller_did: {e}")).await;
        return;
    }

    {
        let existing_log: mlua::Function = lua.globals().get("log").unwrap();
        let state_for_log = state_arc.clone();
        let job_id_for_log = job.id.clone();
        let dual_log = lua
            .create_async_function(move |_lua, msg: String| {
                let existing = existing_log.clone();
                let state = state_for_log.clone();
                let job_id = job_id_for_log.clone();
                async move {
                    let _ = existing.call_async::<()>(msg.clone()).await;
                    if let Err(e) = crate::jobs::logs::insert_log(
                        &state.db,
                        state.db_backend,
                        &job_id,
                        "info",
                        &msg,
                    )
                    .await
                    {
                        tracing::warn!(job_id = %job_id, error = %e, "dual log insert failed");
                    }
                    Ok(())
                }
            })
            .unwrap();
        let _ = lua.globals().set("log", dual_log);
    }

    if let Err(e) = lua.load(script.body.as_str()).exec() {
        let error = format!("script load failed: {e}");
        let _ = db::set_error(state, &job.id, &error).await;
        return;
    }

    let handle: mlua::Function = match lua.globals().get("handle") {
        Ok(f) => f,
        Err(e) => {
            let _ = db::set_error(state, &job.id, &format!("missing handle(): {e}")).await;
            return;
        }
    };

    let outcome = match handle.call_async::<mlua::Value>(()).await {
        Ok(result) => {
            JobOutcome::Completed(lua.from_value(result).unwrap_or(serde_json::json!(null)))
        }
        Err(e) => JobOutcome::Failed(format!("{e}")),
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

async fn load_env_vars(
    db: &sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
) -> std::collections::HashMap<String, String> {
    let sql = adapt_sql("SELECT key, value FROM happyview_script_variables", backend);
    crate::db::query_as::<(String, String)>(&sql)
        .fetch_all(db)
        .await
        .unwrap_or_default()
        .into_iter()
        .collect()
}
