use axum::Json;
use axum::response::{IntoResponse, Response};
use mlua::LuaSerdeExt;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use crate::AppState;
use crate::auth::Claims;
use crate::db::{DatabaseBackend, adapt_sql};
use crate::error::{AppError, LUA_AUTH_ERROR_PREFIX, ScriptErrorType, parse_lua_line};
use crate::event_log::{EventLog, Severity, log_event};
use crate::lexicon::ParsedLexicon;
use crate::repo;
use crate::telemetry::counters::Counters;

use super::atproto_api;
use super::context;
use super::db_api;
use super::http_api;
use super::record;
use super::sandbox;

struct ScriptTimingGuard {
    counters: Arc<Counters>,
    start: Instant,
}

impl Drop for ScriptTimingGuard {
    fn drop(&mut self) {
        let elapsed_us = u64::try_from(self.start.elapsed().as_micros()).unwrap_or(u64::MAX);
        crate::telemetry::counters::add_saturating(&self.counters.script_runtime_us, elapsed_us);
        self.counters
            .script_executions
            .fetch_add(1, Ordering::Relaxed);
    }
}

/// Load all script variables from the database as a key-value map.
async fn load_env_vars(db: &sqlx::AnyPool, backend: DatabaseBackend) -> HashMap<String, String> {
    let sql = adapt_sql("SELECT key, value FROM happyview_script_variables", backend);
    crate::db::query_as::<(String, String)>(&sql)
        .fetch_all(db)
        .await
        .unwrap_or_default()
        .into_iter()
        .collect()
}

/// Execute a Lua script for a procedure endpoint.
#[allow(clippy::too_many_arguments)]
pub async fn execute_procedure_script(
    state: &AppState,
    method: &str,
    claims: &Claims,
    input: &Value,
    params: &std::collections::HashMap<String, Value>,
    lexicon: &ParsedLexicon,
    script: &str,
    space_ctx: Option<&context::SpaceContext>,
    delegate_did: Option<&str>,
) -> Result<Response, AppError> {
    let start = Instant::now();
    let _script_timing = ScriptTimingGuard {
        counters: state.telemetry_counters.clone(),
        start,
    };
    let backend = state.db_backend;
    let span = tracing::info_span!(
        "script.execute",
        method = method,
        script_type = "procedure",
        caller_did = %claims.did(),
    );
    span.in_scope(|| tracing::info!("script execution started"));
    let collection = lexicon.target_collection.as_deref().unwrap_or_default();

    // Capture script source and input for error logging before anything is consumed.
    let script_source = script.to_string();
    let input_json = input.clone();

    let pds_auth: Option<repo::PdsAuth> = if let Some(client_key) = claims.client_key() {
        let encryption_key = state
            .config
            .token_encryption_key
            .as_ref()
            .ok_or_else(|| AppError::Internal("TOKEN_ENCRYPTION_KEY not configured".into()))?;
        let api_client_id = match repo::get_dpop_client_id(state, client_key).await {
            Ok(id) => id,
            Err(e) => {
                let error_message = format!("{e}");
                log_event(
                    &state.db,
                    EventLog {
                        event_type: "script.error".to_string(),
                        severity: Severity::Error,
                        actor_did: Some(claims.did().to_string()),
                        subject: Some(method.to_string()),
                        detail: serde_json::json!({
                            "error": error_message,
                            "script_source": script_source,
                            "input": input_json,
                            "caller_did": claims.did(),
                            "method": method,
                            "duration_ms": start.elapsed().as_millis() as u64,
                        }),
                    },
                    backend,
                )
                .await;
                return Err(e);
            }
        };
        let dpop_key_id = claims
            .dpop_key_id()
            .ok_or_else(|| AppError::Internal("DPoP key ID not available in claims".into()))?
            .to_string();
        Some(repo::PdsAuth::Dpop {
            api_client_id,
            dpop_key_id,
            encryption_key: *encryption_key,
        })
    } else {
        repo::get_oauth_session(state, claims.did())
            .await
            .ok()
            .map(|s| repo::PdsAuth::OAuth(Arc::new(s)))
    };

    let lua = match sandbox::create_sandbox() {
        Ok(l) => l,
        Err(e) => {
            let error_message = format!("failed to create Lua VM: {e}");
            log_event(
                &state.db,
                EventLog {
                    event_type: "script.error".to_string(),
                    severity: Severity::Error,
                    actor_did: Some(claims.did().to_string()),
                    subject: Some(method.to_string()),
                    detail: serde_json::json!({
                        "error": error_message,
                        "script_source": script_source,
                        "input": input_json,
                        "caller_did": claims.did(),
                        "method": method,
                        "duration_ms": start.elapsed().as_millis() as u64,
                    }),
                },
                backend,
            )
            .await;
            return Err(AppError::Internal(error_message));
        }
    };

    let state_arc = Arc::new(state.clone());
    let claims_arc = Arc::new(claims.clone());
    let pds_auth_arc = pds_auth.map(Arc::new);

    if let Err(e) = db_api::register_db_api(&lua, state_arc.clone()) {
        let error_message = format!("failed to register db API: {e}");
        log_event(
            &state.db,
            EventLog {
                event_type: "script.error".to_string(),
                severity: Severity::Error,
                actor_did: Some(claims.did().to_string()),
                subject: Some(method.to_string()),
                detail: serde_json::json!({
                    "error": error_message,
                    "script_source": script_source,
                    "input": input_json,
                    "caller_did": claims.did(),
                    "method": method,
                    "duration_ms": start.elapsed().as_millis() as u64,
                }),
            },
            backend,
        )
        .await;
        return Err(AppError::Internal(error_message));
    }

    if let Err(e) = http_api::register_http_api(&lua, state_arc.clone()) {
        let error_message = format!("failed to register http API: {e}");
        log_event(
            &state.db,
            EventLog {
                event_type: "script.error".to_string(),
                severity: Severity::Error,
                actor_did: Some(claims.did().to_string()),
                subject: Some(method.to_string()),
                detail: serde_json::json!({
                    "error": error_message,
                    "script_source": script_source,
                    "input": input_json,
                    "caller_did": claims.did(),
                    "method": method,
                    "duration_ms": start.elapsed().as_millis() as u64,
                }),
            },
            backend,
        )
        .await;
        return Err(AppError::Internal(error_message));
    }

    if let Err(e) =
        super::xrpc_api::register_xrpc_api(&lua, state_arc.clone(), Some(claims.did().to_string()))
    {
        let error_message = format!("failed to register xrpc API: {e}");
        log_event(
            &state.db,
            EventLog {
                event_type: "script.error".to_string(),
                severity: Severity::Error,
                actor_did: Some(claims.did().to_string()),
                subject: Some(method.to_string()),
                detail: serde_json::json!({
                    "error": error_message,
                    "script_source": script_source,
                    "input": input_json,
                    "caller_did": claims.did(),
                    "method": method,
                    "duration_ms": start.elapsed().as_millis() as u64,
                }),
            },
            backend,
        )
        .await;
        return Err(AppError::Internal(error_message));
    }

    if let Err(e) = atproto_api::register_atproto_api(&lua, state_arc.clone(), Some(claims.did())) {
        let error_message = format!("failed to register atproto API: {e}");
        log_event(
            &state.db,
            EventLog {
                event_type: "script.error".to_string(),
                severity: Severity::Error,
                actor_did: Some(claims.did().to_string()),
                subject: Some(method.to_string()),
                detail: serde_json::json!({
                    "error": error_message,
                    "script_source": script_source,
                    "input": input_json,
                    "caller_did": claims.did(),
                    "method": method,
                    "duration_ms": start.elapsed().as_millis() as u64,
                }),
            },
            backend,
        )
        .await;
        return Err(AppError::Internal(error_message));
    }

    if let Err(e) = crate::lua::spaces_api::register_spaces_write_api(
        &lua,
        state_arc.clone(),
        Some(claims.did()),
    ) {
        let error_message = format!("failed to register spaces write API: {e}");
        log_event(
            &state.db,
            EventLog {
                event_type: "script.error".to_string(),
                severity: Severity::Error,
                actor_did: Some(claims.did().to_string()),
                subject: Some(method.to_string()),
                detail: serde_json::json!({
                    "error": error_message,
                    "script_source": script_source,
                    "input": input_json,
                    "caller_did": claims.did(),
                    "method": method,
                    "duration_ms": start.elapsed().as_millis() as u64,
                }),
            },
            backend,
        )
        .await;
        return Err(AppError::Internal(error_message));
    }

    if let Err(e) = crate::lua::linked_repos_api::register_linked_repos_api(&lua, state_arc.clone())
    {
        let error_message = format!("failed to register linked repos API: {e}");
        log_event(
            &state.db,
            EventLog {
                event_type: "script.error".to_string(),
                severity: Severity::Error,
                actor_did: Some(claims.did().to_string()),
                subject: Some(method.to_string()),
                detail: serde_json::json!({
                    "error": error_message,
                    "script_source": script_source,
                    "input": input_json,
                    "caller_did": claims.did(),
                    "method": method,
                    "duration_ms": start.elapsed().as_millis() as u64,
                }),
            },
            backend,
        )
        .await;
        return Err(AppError::Internal(error_message));
    }

    if let Some(ref pds_auth) = pds_auth_arc
        && let Err(e) = atproto_api::register_atproto_blob_api(
            &lua,
            state_arc.clone(),
            claims_arc.clone(),
            pds_auth.clone(),
        )
    {
        let error_message = format!("failed to register atproto blob API: {e}");
        log_event(
            &state.db,
            EventLog {
                event_type: "script.error".to_string(),
                severity: Severity::Error,
                actor_did: Some(claims.did().to_string()),
                subject: Some(method.to_string()),
                detail: serde_json::json!({
                    "error": error_message,
                    "script_source": script_source,
                    "input": input_json,
                    "caller_did": claims.did(),
                    "method": method,
                    "duration_ms": start.elapsed().as_millis() as u64,
                }),
            },
            backend,
        )
        .await;
        return Err(AppError::Internal(error_message));
    }

    if let Err(e) = super::jobs_api::register_jobs_api(
        &lua,
        state_arc.clone(),
        Some(super::jobs_api::JobsCaller {
            did: claims.did().to_string(),
            api_client_id: pds_auth_arc.as_ref().and_then(|a| match a.as_ref() {
                repo::PdsAuth::Dpop { api_client_id, .. } => Some(api_client_id.clone()),
                _ => None,
            }),
            dpop_key_id: pds_auth_arc.as_ref().and_then(|a| match a.as_ref() {
                repo::PdsAuth::Dpop { dpop_key_id, .. } => Some(dpop_key_id.clone()),
                _ => None,
            }),
        }),
    ) {
        let error_message = format!("failed to register jobs API: {e}");
        log_event(
            &state.db,
            EventLog {
                event_type: "script.error".to_string(),
                severity: Severity::Error,
                actor_did: Some(claims.did().to_string()),
                subject: Some(method.to_string()),
                detail: serde_json::json!({
                    "error": error_message,
                    "script_source": script_source,
                    "input": input_json,
                    "caller_did": claims.did(),
                    "method": method,
                    "duration_ms": start.elapsed().as_millis() as u64,
                }),
            },
            backend,
        )
        .await;
        return Err(AppError::Internal(error_message));
    }

    if let Err(e) = record::register_record_api(
        &lua,
        state_arc.clone(),
        Some(claims_arc),
        pds_auth_arc,
        delegate_did.map(|s| s.to_string()),
    ) {
        let error_message = format!("failed to register Record API: {e}");
        log_event(
            &state.db,
            EventLog {
                event_type: "script.error".to_string(),
                severity: Severity::Error,
                actor_did: Some(claims.did().to_string()),
                subject: Some(method.to_string()),
                detail: serde_json::json!({
                    "error": error_message,
                    "script_source": script_source,
                    "input": input_json,
                    "caller_did": claims.did(),
                    "method": method,
                    "duration_ms": start.elapsed().as_millis() as u64,
                }),
            },
            backend,
        )
        .await;
        return Err(AppError::Internal(error_message));
    }

    // Override the sandbox's tracing-only `log()` with a version that
    // also writes a `script.log` row to `event_logs` so operators can
    // see script output from the dashboard. The xrpc trigger id is
    // computed from the lexicon's id + procedure type.
    let trigger_id = format!("xrpc.procedure:{}", lexicon.id);
    if let Err(e) =
        super::scripts::register_log_event_api(&lua, &state_arc, &trigger_id, Some(claims.did()))
    {
        let error_message = format!("failed to register log API: {e}");
        log_event(
            &state.db,
            EventLog {
                event_type: "script.error".to_string(),
                severity: Severity::Error,
                actor_did: Some(claims.did().to_string()),
                subject: Some(method.to_string()),
                detail: serde_json::json!({
                    "error": error_message,
                    "script_source": script_source,
                    "input": input_json,
                    "caller_did": claims.did(),
                    "method": method,
                    "duration_ms": start.elapsed().as_millis() as u64,
                }),
            },
            backend,
        )
        .await;
        return Err(AppError::Internal(error_message));
    }

    if let Err(e) = context::set_procedure_context(
        &lua,
        method,
        input,
        params,
        claims.did(),
        collection,
        space_ctx,
        delegate_did,
    ) {
        let error_message = format!("failed to set context: {e}");
        log_event(
            &state.db,
            EventLog {
                event_type: "script.error".to_string(),
                severity: Severity::Error,
                actor_did: Some(claims.did().to_string()),
                subject: Some(method.to_string()),
                detail: serde_json::json!({
                    "error": error_message,
                    "script_source": script_source,
                    "input": input_json,
                    "caller_did": claims.did(),
                    "method": method,
                    "duration_ms": start.elapsed().as_millis() as u64,
                }),
            },
            backend,
        )
        .await;
        return Err(AppError::Internal(error_message));
    }

    if let Err(e) = context::set_env_context(&lua, &load_env_vars(&state.db, backend).await) {
        let error_message = format!("failed to set env context: {e}");
        log_event(
            &state.db,
            EventLog {
                event_type: "script.error".to_string(),
                severity: Severity::Error,
                actor_did: Some(claims.did().to_string()),
                subject: Some(method.to_string()),
                detail: serde_json::json!({
                    "error": error_message,
                    "script_source": script_source,
                    "input": input_json,
                    "caller_did": claims.did(),
                    "method": method,
                    "duration_ms": start.elapsed().as_millis() as u64,
                }),
            },
            backend,
        )
        .await;
        return Err(AppError::Internal(error_message));
    }

    if let Err(e) = lua.load(script).exec() {
        let error_message = format!("{e}");
        tracing::error!(method, error = %e, "lua script load failed");
        log_event(
            &state.db,
            EventLog {
                event_type: "script.error".to_string(),
                severity: Severity::Error,
                actor_did: Some(claims.did().to_string()),
                subject: Some(method.to_string()),
                detail: serde_json::json!({
                    "error": error_message,
                    "script_source": script_source,
                    "input": input_json,
                    "caller_did": claims.did(),
                    "method": method,
                    "duration_ms": start.elapsed().as_millis() as u64,
                }),
            },
            backend,
        )
        .await;
        let (line, clean_msg) = parse_lua_line(&error_message);
        return Err(AppError::ScriptError {
            error_type: ScriptErrorType::Syntax,
            message: clean_msg,
            method: method.to_string(),
            line,
        });
    }

    let handle: mlua::Function = match lua.globals().get("handle") {
        Ok(f) => f,
        Err(e) => {
            let error_message = format!("{e}");
            tracing::error!(method, error = %e, "lua script missing handle function");
            log_event(
                &state.db,
                EventLog {
                    event_type: "script.error".to_string(),
                    severity: Severity::Error,
                    actor_did: Some(claims.did().to_string()),
                    subject: Some(method.to_string()),
                    detail: serde_json::json!({
                        "error": error_message,
                        "script_source": script_source,
                        "input": input_json,
                        "caller_did": claims.did(),
                        "method": method,
                        "duration_ms": start.elapsed().as_millis() as u64,
                    }),
                },
                backend,
            )
            .await;
            return Err(AppError::ScriptError {
                error_type: ScriptErrorType::MissingHandle,
                message: "script does not define a handle() function".to_string(),
                method: method.to_string(),
                line: None,
            });
        }
    };

    let result: mlua::Value = match handle.call_async(()).await {
        Ok(r) => r,
        Err(e) => {
            let msg = e.to_string();
            tracing::error!(method, error = %msg, "lua script execution failed");
            let (line, clean_msg) = parse_lua_line(&msg);
            let app_error = if msg.contains(LUA_AUTH_ERROR_PREFIX)
                || clean_msg.contains(LUA_AUTH_ERROR_PREFIX)
            {
                let auth_msg = clean_msg
                    .strip_prefix(LUA_AUTH_ERROR_PREFIX)
                    .unwrap_or(&clean_msg)
                    .to_string();
                AppError::Auth(auth_msg)
            } else if msg.contains("execution limit") {
                AppError::ScriptError {
                    error_type: ScriptErrorType::Timeout,
                    message: "script exceeded execution time limit".to_string(),
                    method: method.to_string(),
                    line,
                }
            } else {
                AppError::ScriptError {
                    error_type: ScriptErrorType::Runtime,
                    message: clean_msg,
                    method: method.to_string(),
                    line,
                }
            };
            log_event(
                &state.db,
                EventLog {
                    event_type: "script.error".to_string(),
                    severity: Severity::Error,
                    actor_did: Some(claims.did().to_string()),
                    subject: Some(method.to_string()),
                    detail: serde_json::json!({
                        "error": msg,
                        "script_source": script_source,
                        "input": input_json,
                        "caller_did": claims.did(),
                        "method": method,
                        "duration_ms": start.elapsed().as_millis() as u64,
                    }),
                },
                backend,
            )
            .await;
            return Err(app_error);
        }
    };

    let json_value: Value = match lua.from_value(result) {
        Ok(v) => v,
        Err(e) => {
            let error_message = format!("{e}");
            tracing::error!(method, error = %e, "failed to convert lua result to JSON");
            log_event(
                &state.db,
                EventLog {
                    event_type: "script.error".to_string(),
                    severity: Severity::Error,
                    actor_did: Some(claims.did().to_string()),
                    subject: Some(method.to_string()),
                    detail: serde_json::json!({
                        "error": error_message,
                        "script_source": script_source,
                        "input": input_json,
                        "caller_did": claims.did(),
                        "method": method,
                        "duration_ms": start.elapsed().as_millis() as u64,
                    }),
                },
                backend,
            )
            .await;
            return Err(AppError::ScriptError {
                error_type: ScriptErrorType::Runtime,
                message: error_message,
                method: method.to_string(),
                line: None,
            });
        }
    };

    span.in_scope(|| {
        tracing::info!(
            duration_ms = start.elapsed().as_millis() as u64,
            "script execution completed"
        );
    });
    log_event(
        &state.db,
        EventLog {
            event_type: "script.executed".to_string(),
            severity: Severity::Info,
            actor_did: Some(claims.did().to_string()),
            subject: Some(method.to_string()),
            detail: serde_json::json!({
                "method": method,
                "caller_did": claims.did(),
                "duration_ms": start.elapsed().as_millis() as u64,
                "response_size": json_value.to_string().len(),
                "input": input_json,
                "response": json_value,
            }),
        },
        backend,
    )
    .await;

    Ok(Json(json_value).into_response())
}

/// Execute a Lua script for a query endpoint.
pub async fn execute_query_script(
    state: &AppState,
    method: &str,
    params: &HashMap<String, serde_json::Value>,
    lexicon: &ParsedLexicon,
    script: &str,
    claims: Option<&Claims>,
    space_ctx: Option<&context::SpaceContext>,
) -> Result<Response, AppError> {
    let start = Instant::now();
    let _script_timing = ScriptTimingGuard {
        counters: state.telemetry_counters.clone(),
        start,
    };
    let backend = state.db_backend;
    let span = tracing::info_span!("script.execute", method = method, script_type = "query",);
    span.in_scope(|| tracing::info!("script execution started"));
    let collection = lexicon.target_collection.as_deref().unwrap_or_default();

    // Capture script source for error logging.
    let script_source = script.to_string();

    let lua = match sandbox::create_sandbox() {
        Ok(l) => l,
        Err(e) => {
            let error_message = format!("failed to create Lua VM: {e}");
            log_event(
                &state.db,
                EventLog {
                    event_type: "script.error".to_string(),
                    severity: Severity::Error,
                    actor_did: None,
                    subject: Some(method.to_string()),
                    detail: serde_json::json!({
                        "error": error_message,
                        "script_source": script_source,
                        "method": method,
                        "duration_ms": start.elapsed().as_millis() as u64,
                    }),
                },
                backend,
            )
            .await;
            return Err(AppError::Internal(error_message));
        }
    };

    let state_arc = Arc::new(state.clone());

    if let Err(e) = db_api::register_db_api(&lua, state_arc.clone()) {
        let error_message = format!("failed to register db API: {e}");
        log_event(
            &state.db,
            EventLog {
                event_type: "script.error".to_string(),
                severity: Severity::Error,
                actor_did: None,
                subject: Some(method.to_string()),
                detail: serde_json::json!({
                    "error": error_message,
                    "script_source": script_source,
                    "method": method,
                    "duration_ms": start.elapsed().as_millis() as u64,
                }),
            },
            backend,
        )
        .await;
        return Err(AppError::Internal(error_message));
    }

    if let Err(e) = http_api::register_http_api(&lua, state_arc.clone()) {
        let error_message = format!("failed to register http API: {e}");
        log_event(
            &state.db,
            EventLog {
                event_type: "script.error".to_string(),
                severity: Severity::Error,
                actor_did: None,
                subject: Some(method.to_string()),
                detail: serde_json::json!({
                    "error": error_message,
                    "script_source": script_source,
                    "method": method,
                    "duration_ms": start.elapsed().as_millis() as u64,
                }),
            },
            backend,
        )
        .await;
        return Err(AppError::Internal(error_message));
    }

    if let Err(e) = super::xrpc_api::register_xrpc_api(
        &lua,
        state_arc.clone(),
        claims.map(|c| c.did().to_string()),
    ) {
        let error_message = format!("failed to register xrpc API: {e}");
        log_event(
            &state.db,
            EventLog {
                event_type: "script.error".to_string(),
                severity: Severity::Error,
                actor_did: None,
                subject: Some(method.to_string()),
                detail: serde_json::json!({
                    "error": error_message,
                    "script_source": script_source,
                    "method": method,
                    "duration_ms": start.elapsed().as_millis() as u64,
                }),
            },
            backend,
        )
        .await;
        return Err(AppError::Internal(error_message));
    }

    if let Err(e) =
        atproto_api::register_atproto_api(&lua, state_arc.clone(), claims.map(|c| c.did()))
    {
        let error_message = format!("failed to register atproto API: {e}");
        log_event(
            &state.db,
            EventLog {
                event_type: "script.error".to_string(),
                severity: Severity::Error,
                actor_did: None,
                subject: Some(method.to_string()),
                detail: serde_json::json!({
                    "error": error_message,
                    "script_source": script_source,
                    "method": method,
                    "duration_ms": start.elapsed().as_millis() as u64,
                }),
            },
            backend,
        )
        .await;
        return Err(AppError::Internal(error_message));
    }

    if let Err(e) = crate::lua::spaces_api::register_spaces_write_api(
        &lua,
        state_arc.clone(),
        claims.map(|c| c.did()),
    ) {
        let error_message = format!("failed to register spaces write API: {e}");
        log_event(
            &state.db,
            EventLog {
                event_type: "script.error".to_string(),
                severity: Severity::Error,
                actor_did: None,
                subject: Some(method.to_string()),
                detail: serde_json::json!({
                    "error": error_message,
                    "script_source": script_source,
                    "method": method,
                    "duration_ms": start.elapsed().as_millis() as u64,
                }),
            },
            backend,
        )
        .await;
        return Err(AppError::Internal(error_message));
    }

    if let Err(e) = crate::lua::linked_repos_api::register_linked_repos_api(&lua, state_arc.clone())
    {
        let error_message = format!("failed to register linked repos API: {e}");
        log_event(
            &state.db,
            EventLog {
                event_type: "script.error".to_string(),
                severity: Severity::Error,
                actor_did: None,
                subject: Some(method.to_string()),
                detail: serde_json::json!({
                    "error": error_message,
                    "script_source": script_source,
                    "method": method,
                    "duration_ms": start.elapsed().as_millis() as u64,
                }),
            },
            backend,
        )
        .await;
        return Err(AppError::Internal(error_message));
    }

    // Register the Record API in no-auth mode. Queries don't have a PDS
    // auth context — the local-only methods (Record.load, :save_local,
    // :delete_local, Record.delete_local) work; PDS-touching variants
    // error with the no-PDS-auth message.
    if let Err(e) = record::register_record_api_no_auth(&lua, state_arc.clone()) {
        let error_message = format!("failed to register Record API: {e}");
        log_event(
            &state.db,
            EventLog {
                event_type: "script.error".to_string(),
                severity: Severity::Error,
                actor_did: None,
                subject: Some(method.to_string()),
                detail: serde_json::json!({
                    "error": error_message,
                    "script_source": script_source,
                    "method": method,
                    "duration_ms": start.elapsed().as_millis() as u64,
                }),
            },
            backend,
        )
        .await;
        return Err(AppError::Internal(error_message));
    }

    // Override the sandbox's tracing-only `log()` with a version that
    // also writes a `script.log` row to `event_logs`.
    let trigger_id = format!("xrpc.query:{}", lexicon.id);
    if let Err(e) = super::scripts::register_log_event_api(
        &lua,
        &state_arc,
        &trigger_id,
        claims.map(|c| c.did()),
    ) {
        let error_message = format!("failed to register log API: {e}");
        log_event(
            &state.db,
            EventLog {
                event_type: "script.error".to_string(),
                severity: Severity::Error,
                actor_did: None,
                subject: Some(method.to_string()),
                detail: serde_json::json!({
                    "error": error_message,
                    "script_source": script_source,
                    "method": method,
                    "duration_ms": start.elapsed().as_millis() as u64,
                }),
            },
            backend,
        )
        .await;
        return Err(AppError::Internal(error_message));
    }

    if let Err(e) = context::set_query_context(
        &lua,
        method,
        params,
        collection,
        claims.map(|c| c.did()),
        space_ctx,
    ) {
        let error_message = format!("failed to set context: {e}");
        log_event(
            &state.db,
            EventLog {
                event_type: "script.error".to_string(),
                severity: Severity::Error,
                actor_did: None,
                subject: Some(method.to_string()),
                detail: serde_json::json!({
                    "error": error_message,
                    "script_source": script_source,
                    "method": method,
                    "duration_ms": start.elapsed().as_millis() as u64,
                }),
            },
            backend,
        )
        .await;
        return Err(AppError::Internal(error_message));
    }

    if let Err(e) = context::set_env_context(&lua, &load_env_vars(&state.db, backend).await) {
        let error_message = format!("failed to set env context: {e}");
        log_event(
            &state.db,
            EventLog {
                event_type: "script.error".to_string(),
                severity: Severity::Error,
                actor_did: None,
                subject: Some(method.to_string()),
                detail: serde_json::json!({
                    "error": error_message,
                    "script_source": script_source,
                    "method": method,
                    "duration_ms": start.elapsed().as_millis() as u64,
                }),
            },
            backend,
        )
        .await;
        return Err(AppError::Internal(error_message));
    }

    if let Err(e) = lua.load(script).exec() {
        let error_message = format!("{e}");
        tracing::error!(method, error = %e, "lua script load failed");
        log_event(
            &state.db,
            EventLog {
                event_type: "script.error".to_string(),
                severity: Severity::Error,
                actor_did: None,
                subject: Some(method.to_string()),
                detail: serde_json::json!({
                    "error": error_message,
                    "script_source": script_source,
                    "method": method,
                    "duration_ms": start.elapsed().as_millis() as u64,
                }),
            },
            backend,
        )
        .await;
        let (line, clean_msg) = parse_lua_line(&error_message);
        return Err(AppError::ScriptError {
            error_type: ScriptErrorType::Syntax,
            message: clean_msg,
            method: method.to_string(),
            line,
        });
    }

    let handle: mlua::Function = match lua.globals().get("handle") {
        Ok(f) => f,
        Err(e) => {
            let error_message = format!("{e}");
            tracing::error!(method, error = %e, "lua script missing handle function");
            log_event(
                &state.db,
                EventLog {
                    event_type: "script.error".to_string(),
                    severity: Severity::Error,
                    actor_did: None,
                    subject: Some(method.to_string()),
                    detail: serde_json::json!({
                        "error": error_message,
                        "script_source": script_source,
                        "method": method,
                        "duration_ms": start.elapsed().as_millis() as u64,
                    }),
                },
                backend,
            )
            .await;
            return Err(AppError::ScriptError {
                error_type: ScriptErrorType::MissingHandle,
                message: "script does not define a handle() function".to_string(),
                method: method.to_string(),
                line: None,
            });
        }
    };

    let result: mlua::Value = match handle.call_async(()).await {
        Ok(r) => r,
        Err(e) => {
            let msg = e.to_string();
            tracing::error!(method, error = %msg, "lua script execution failed");
            let (line, clean_msg) = parse_lua_line(&msg);
            let app_error = if msg.contains(LUA_AUTH_ERROR_PREFIX)
                || clean_msg.contains(LUA_AUTH_ERROR_PREFIX)
            {
                let auth_msg = clean_msg
                    .strip_prefix(LUA_AUTH_ERROR_PREFIX)
                    .unwrap_or(&clean_msg)
                    .to_string();
                AppError::Auth(auth_msg)
            } else if msg.contains("execution limit") {
                AppError::ScriptError {
                    error_type: ScriptErrorType::Timeout,
                    message: "script exceeded execution time limit".to_string(),
                    method: method.to_string(),
                    line,
                }
            } else {
                AppError::ScriptError {
                    error_type: ScriptErrorType::Runtime,
                    message: clean_msg,
                    method: method.to_string(),
                    line,
                }
            };
            log_event(
                &state.db,
                EventLog {
                    event_type: "script.error".to_string(),
                    severity: Severity::Error,
                    actor_did: None,
                    subject: Some(method.to_string()),
                    detail: serde_json::json!({
                        "error": msg,
                        "script_source": script_source,
                        "method": method,
                        "duration_ms": start.elapsed().as_millis() as u64,
                    }),
                },
                backend,
            )
            .await;
            return Err(app_error);
        }
    };

    let json_value: Value = match lua.from_value(result) {
        Ok(v) => v,
        Err(e) => {
            let error_message = format!("{e}");
            tracing::error!(method, error = %e, "failed to convert lua result to JSON");
            log_event(
                &state.db,
                EventLog {
                    event_type: "script.error".to_string(),
                    severity: Severity::Error,
                    actor_did: None,
                    subject: Some(method.to_string()),
                    detail: serde_json::json!({
                        "error": error_message,
                        "script_source": script_source,
                        "method": method,
                        "duration_ms": start.elapsed().as_millis() as u64,
                    }),
                },
                backend,
            )
            .await;
            return Err(AppError::ScriptError {
                error_type: ScriptErrorType::Runtime,
                message: error_message,
                method: method.to_string(),
                line: None,
            });
        }
    };

    span.in_scope(|| {
        tracing::info!(
            duration_ms = start.elapsed().as_millis() as u64,
            "script execution completed"
        );
    });
    log_event(
        &state.db,
        EventLog {
            event_type: "script.executed".to_string(),
            severity: Severity::Info,
            actor_did: None,
            subject: Some(method.to_string()),
            detail: serde_json::json!({
                "method": method,
                "duration_ms": start.elapsed().as_millis() as u64,
                "response_size": json_value.to_string().len(),
                "params": params,
                "response": json_value,
            }),
        },
        backend,
    )
    .await;

    Ok(Json(json_value).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexicon::{LexiconType, ProcedureAction};
    use crate::test_support::{memory_pool, test_state_with_pool};

    fn query_lexicon() -> ParsedLexicon {
        ParsedLexicon {
            id: "com.example.probe".to_string(),
            lexicon_type: LexiconType::Query,
            record_key: None,
            parameters: None,
            input: None,
            output: None,
            record_schema: None,
            raw: serde_json::json!({ "id": "com.example.probe" }),
            revision: 1,
            target_collection: Some("com.example.probe".to_string()),
            action: ProcedureAction::Upsert,
            token_cost: None,
            space_type: None,
            space_name: None,
            space_collections: None,
        }
    }

    #[tokio::test]
    async fn successful_script_execution_moves_the_script_counters() {
        let state = test_state_with_pool(memory_pool().await);
        let lexicon = query_lexicon();
        let params = HashMap::new();
        let counters = state.telemetry_counters.clone();

        let result = execute_query_script(
            &state,
            "com.example.probe",
            &params,
            &lexicon,
            "function handle() return { ok = true } end",
            None,
            None,
        )
        .await;

        assert!(
            result.is_ok(),
            "script should have executed: {:?}",
            result.err()
        );
        assert_eq!(counters.script_executions.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn a_script_missing_handle_still_moves_the_script_counters() {
        let state = test_state_with_pool(memory_pool().await);
        let lexicon = query_lexicon();
        let params = HashMap::new();
        let counters = state.telemetry_counters.clone();

        let result = execute_query_script(
            &state,
            "com.example.probe",
            &params,
            &lexicon,
            "local unused = 1",
            None,
            None,
        )
        .await;

        assert!(
            result.is_err(),
            "script has no handle() function, so this must error"
        );
        assert_eq!(counters.script_executions.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn a_lua_runtime_error_still_moves_the_script_counters() {
        // `handle()` runs and raises — a different early return than the
        // "missing handle" case (this one comes from `handle.call_async`
        // failing, not from `lua.globals().get("handle")` failing).
        let state = test_state_with_pool(memory_pool().await);
        let lexicon = query_lexicon();
        let params = HashMap::new();
        let counters = state.telemetry_counters.clone();

        let result = execute_query_script(
            &state,
            "com.example.probe",
            &params,
            &lexicon,
            "function handle() error('boom') end",
            None,
            None,
        )
        .await;

        assert!(result.is_err());
        assert_eq!(counters.script_executions.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn script_counters_accumulate_across_calls_on_the_same_counters() {
        let state = test_state_with_pool(memory_pool().await);
        let lexicon = query_lexicon();
        let params = HashMap::new();
        let counters = state.telemetry_counters.clone();

        for _ in 0..20 {
            let _ = execute_query_script(
                &state,
                "com.example.probe",
                &params,
                &lexicon,
                "function handle() return {} end",
                None,
                None,
            )
            .await;
        }

        assert_eq!(counters.script_executions.load(Ordering::Relaxed), 20);
        assert!(
            counters.script_runtime_us.load(Ordering::Relaxed) > 0,
            "20 script executions should accumulate measurable wall-clock time"
        );
    }
}
