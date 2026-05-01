use axum::Json;
use axum::response::{IntoResponse, Response};
use mlua::LuaSerdeExt;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use crate::AppState;
use crate::auth::Claims;
use crate::db::{DatabaseBackend, adapt_sql};
use crate::error::{AppError, ScriptErrorType, parse_lua_line};
use crate::event_log::{EventLog, Severity, log_event};
use crate::lexicon::ParsedLexicon;
use crate::repo;

use super::atproto_api;
use super::context;
use super::db_api;
use super::http_api;
use super::record;
use super::sandbox;

/// Load all script variables from the database as a key-value map.
async fn load_env_vars(db: &sqlx::AnyPool, backend: DatabaseBackend) -> HashMap<String, String> {
    let sql = adapt_sql("SELECT key, value FROM script_variables", backend);
    sqlx::query_as::<_, (String, String)>(&sql)
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

    let pds_auth = if let Some(client_key) = claims.client_key() {
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
        repo::PdsAuth::Dpop {
            api_client_id,
            encryption_key: *encryption_key,
        }
    } else {
        match repo::get_oauth_session(state, claims.did()).await {
            Ok(s) => repo::PdsAuth::OAuth(Arc::new(s)),
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
        }
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
    let pds_auth_arc = Arc::new(pds_auth);

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

    if let Err(e) = record::register_record_api(
        &lua,
        state_arc.clone(),
        Some(claims_arc),
        Some(pds_auth_arc),
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
            let app_error = if msg.contains("execution limit") {
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

    // Register the Record API in no-auth mode. Queries don't have a PDS
    // auth context — the local-only methods (Record.load, :save_local,
    // :delete_local, Record.delete_local) work; PDS-touching variants
    // error with the no-PDS-auth message.
    if let Err(e) = record::register_record_api_no_auth(&lua, state_arc) {
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
            let app_error = if msg.contains("execution limit") {
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
