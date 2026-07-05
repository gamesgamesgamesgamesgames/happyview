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
        let pds_auth = match repo::get_oauth_session(state, &job.created_by).await {
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
        Some(job.created_by.clone()),
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

    match handle.call_async::<mlua::Value>(()).await {
        Ok(result) => {
            let json_result: serde_json::Value =
                lua.from_value(result).unwrap_or(serde_json::json!(null));

            match db::should_stop(state, &job.id).await {
                Some("pausing") => {
                    let _ = db::set_status(state, &job.id, "paused").await;
                    tracing::info!(job_id = %job.id, "job paused");
                    log_event(
                        &state.db,
                        EventLog {
                            event_type: "job.paused".to_string(),
                            severity: Severity::Info,
                            actor_did: Some(job.created_by.clone()),
                            subject: Some(job.job_type.clone()),
                            detail: serde_json::json!({ "job_id": job.id }),
                        },
                        backend,
                    )
                    .await;
                }
                Some("cancelling") => {
                    let _ = db::set_status(state, &job.id, "cancelled").await;
                    tracing::info!(job_id = %job.id, "job cancelled");
                    log_event(
                        &state.db,
                        EventLog {
                            event_type: "job.cancelled".to_string(),
                            severity: Severity::Info,
                            actor_did: Some(job.created_by.clone()),
                            subject: Some(job.job_type.clone()),
                            detail: serde_json::json!({ "job_id": job.id }),
                        },
                        backend,
                    )
                    .await;
                }
                _ => {
                    let _ = db::set_result(state, &job.id, &json_result).await;
                    tracing::info!(job_id = %job.id, "job completed");
                    log_event(
                        &state.db,
                        EventLog {
                            event_type: "job.completed".to_string(),
                            severity: Severity::Info,
                            actor_did: Some(job.created_by.clone()),
                            subject: Some(job.job_type.clone()),
                            detail: serde_json::json!({
                                "job_id": job.id,
                                "result": json_result,
                            }),
                        },
                        backend,
                    )
                    .await;
                }
            }
        }
        Err(e) => {
            let error = format!("{e}");
            tracing::error!(job_id = %job.id, %error, "job script failed");
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
        }
    }
}

async fn load_env_vars(
    db: &sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
) -> std::collections::HashMap<String, String> {
    let sql = adapt_sql("SELECT key, value FROM happyview_script_variables", backend);
    sqlx::query_as::<_, (String, String)>(&sql)
        .fetch_all(db)
        .await
        .unwrap_or_default()
        .into_iter()
        .collect()
}
