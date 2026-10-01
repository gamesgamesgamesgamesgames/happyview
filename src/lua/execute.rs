use axum::Json;
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use crate::AppState;
use crate::auth::Claims;
use crate::error::{AppError, LUA_AUTH_ERROR_PREFIX, ScriptErrorType};
use crate::event_log::{EventLog, Severity, log_event};
use crate::lexicon::ParsedLexicon;
use crate::plugin::{ScriptExecuteOutput, ScriptSpace};
use crate::repo;
use crate::script::{DispatchError, Invocation, Trigger, dispatch};
use crate::telemetry::counters::Counters;

use super::context;

/// What a caller is told when the budget ended the run. The limit is the
/// host's, so the sentence is the host's rather than whatever text the
/// interpreter raised on its way out.
const EXECUTION_LIMIT_MESSAGE: &str = "script exceeded execution time limit";

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

/// The text after the auth-error prefix, wherever it sits in the message.
/// An interpreter wraps a raised error in context of its own, so the prefix is
/// not guaranteed to lead the string — `strip_prefix` would then miss it and
/// the whole wrapped message, prefix included, would leak into the 401 body.
fn auth_message_after_prefix(msg: &str) -> String {
    match msg.find(LUA_AUTH_ERROR_PREFIX) {
        Some(i) => msg[i + LUA_AUTH_ERROR_PREFIX.len()..].to_string(),
        None => msg.to_string(),
    }
}

/// The error a caller sees for a run that produced no value.
///
/// An `AUTH_ERROR:`-prefixed message is a credential failure raised through the
/// script, and is read ahead of every other kind so that it keeps its 401
/// instead of arriving as a script failure.
fn caller_error(
    method: &str,
    error_type: ScriptErrorType,
    message: String,
    line: Option<u32>,
) -> AppError {
    if message.contains(LUA_AUTH_ERROR_PREFIX) {
        return AppError::Auth(auth_message_after_prefix(&message));
    }
    AppError::ScriptError {
        error_type,
        message: match error_type {
            ScriptErrorType::Timeout => EXECUTION_LIMIT_MESSAGE.to_string(),
            _ => message,
        },
        method: method.to_string(),
        line,
    }
}

/// One run's outcome as a runner needs it: the value to serialise, or the
/// interpreter's unparsed text for the event log beside the error the caller
/// sees.
///
/// `value_kind` is not read here: an XRPC response is the value, and a script
/// that returned nothing answers `null` rather than a third outcome.
fn returned_value(
    method: &str,
    outcome: Result<ScriptExecuteOutput, DispatchError>,
) -> Result<Value, (String, AppError)> {
    match outcome {
        Ok(ScriptExecuteOutput::Returned { value, .. }) => Ok(value),
        Ok(ScriptExecuteOutput::Error {
            kind,
            message,
            line,
            raw,
        }) => Err((raw, caller_error(method, kind.into(), message, line))),
        Err(e) => {
            let raw = e.to_string();
            let error_type = match &e {
                DispatchError::Execution(e) => e.script_error_type(),
                DispatchError::NoInterpreter { .. } => ScriptErrorType::Runtime,
            };
            Err((raw.clone(), caller_error(method, error_type, raw, None)))
        }
    }
}

/// Execute a procedure endpoint's script.
#[allow(clippy::too_many_arguments)]
pub async fn execute_procedure_script(
    state: &AppState,
    method: &str,
    claims: &Claims,
    input: &Value,
    params: &HashMap<String, Value>,
    lexicon: &ParsedLexicon,
    script: &str,
    language: &str,
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

    let claims_arc = Arc::new(claims.clone());
    let caller_session = pds_auth.map(|pds_auth| {
        Arc::new(crate::plugin::caller::CallerSession {
            did: claims.did().to_string(),
            delegate_did: delegate_did.map(|s| s.to_string()),
            claims: claims_arc.clone(),
            pds_auth: Arc::new(pds_auth),
            app_state: state.clone(),
        })
    });
    let has_pds_auth = caller_session.is_some();
    let trigger_id = format!("xrpc.procedure:{}", lexicon.id);
    let space = space_ctx.map(ScriptSpace::from);

    let outcome = dispatch(
        state,
        &Invocation {
            trigger_id: &trigger_id,
            trigger: Trigger::XrpcProcedure,
            language,
            source: script,
            input: &input_json,
            caller_did: Some(claims.did()),
            has_pds_auth,
            method: Some(method),
            collection: Some(collection),
            params: Some(params),
            delegate_did,
            space: space.as_ref(),
        },
        caller_session,
    )
    .await;

    let json_value = match returned_value(method, outcome) {
        Ok(value) => value,
        Err((raw, error)) => {
            tracing::error!(method, error = %raw, "script execution failed");
            log_event(
                &state.db,
                EventLog {
                    event_type: "script.error".to_string(),
                    severity: Severity::Error,
                    actor_did: Some(claims.did().to_string()),
                    subject: Some(method.to_string()),
                    detail: serde_json::json!({
                        "error": raw,
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
            return Err(error);
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

/// Execute a query endpoint's script.
#[allow(clippy::too_many_arguments)]
pub async fn execute_query_script(
    state: &AppState,
    method: &str,
    params: &HashMap<String, Value>,
    lexicon: &ParsedLexicon,
    script: &str,
    language: &str,
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

    // A query's parameters are the first argument of `handle`, which is why
    // `ctx.params` carries nothing: the same values twice would leave a script
    // author guessing which one a runner fills.
    let input_json = Value::Object(params.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
    let trigger_id = format!("xrpc.query:{}", lexicon.id);
    let space = space_ctx.map(ScriptSpace::from);

    let outcome = dispatch(
        state,
        &Invocation {
            trigger_id: &trigger_id,
            trigger: Trigger::XrpcQuery,
            language,
            source: script,
            input: &input_json,
            caller_did: claims.map(|c| c.did()),
            has_pds_auth: false,
            method: Some(method),
            collection: Some(collection),
            params: None,
            delegate_did: None,
            space: space.as_ref(),
        },
        None,
    )
    .await;

    let json_value = match returned_value(method, outcome) {
        Ok(value) => value,
        Err((raw, error)) => {
            tracing::error!(method, error = %raw, "script execution failed");
            log_event(
                &state.db,
                EventLog {
                    event_type: "script.error".to_string(),
                    severity: Severity::Error,
                    actor_did: None,
                    subject: Some(method.to_string()),
                    detail: serde_json::json!({
                        "error": raw,
                        "script_source": script_source,
                        "method": method,
                        "duration_ms": start.elapsed().as_millis() as u64,
                    }),
                },
                backend,
            )
            .await;
            return Err(error);
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
    use crate::plugin::{ExecutionError, ScriptErrorKind, ScriptValueKind};
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

    fn failed(kind: ScriptErrorKind, message: &str) -> Result<ScriptExecuteOutput, DispatchError> {
        Ok(ScriptExecuteOutput::Error {
            kind,
            message: message.to_string(),
            line: Some(7),
            raw: format!("[string \"script\"]:7: {message}"),
        })
    }

    #[test]
    fn auth_message_survives_a_prefix_that_is_not_leading() {
        let wrapped = format!(
            "runtime error: caller.create_record: Plugin returned error: AUTH_REQUIRED - {}DPoP session not found",
            LUA_AUTH_ERROR_PREFIX
        );
        assert_eq!(
            auth_message_after_prefix(&wrapped),
            "DPoP session not found"
        );
    }

    #[test]
    fn auth_message_falls_back_to_the_whole_string_without_a_prefix() {
        assert_eq!(
            auth_message_after_prefix("no prefix here"),
            "no prefix here"
        );
    }

    #[test]
    fn a_returned_value_is_the_body_whatever_its_kind() {
        for value_kind in [
            ScriptValueKind::Object,
            ScriptValueKind::None,
            ScriptValueKind::Other,
        ] {
            let value = serde_json::json!({ "ok": true });
            let body = returned_value(
                "com.example.probe",
                Ok(ScriptExecuteOutput::Returned {
                    value: value.clone(),
                    value_kind,
                }),
            )
            .unwrap_or_else(|(raw, _)| panic!("{value_kind:?}: {raw}"));
            assert_eq!(body, value, "{value_kind:?}");
        }
    }

    /// Each kind reaches its own `ScriptErrorType`, which is what the status
    /// and the `errorType` field are read from.
    #[test]
    fn every_error_kind_keeps_its_type_its_line_and_its_message() {
        for (kind, expected) in [
            (ScriptErrorKind::Syntax, ScriptErrorType::Syntax),
            (ScriptErrorKind::Runtime, ScriptErrorType::Runtime),
            (ScriptErrorKind::Memory, ScriptErrorType::Memory),
            (
                ScriptErrorKind::MissingHandle,
                ScriptErrorType::MissingHandle,
            ),
        ] {
            let (raw, error) = returned_value(
                "com.example.probe",
                failed(kind, "attempt to index a nil value (local 't')"),
            )
            .expect_err("a failed run has no value");
            assert_eq!(
                raw, "[string \"script\"]:7: attempt to index a nil value (local 't')",
                "{kind:?}"
            );
            match error {
                AppError::ScriptError {
                    error_type,
                    message,
                    method,
                    line,
                } => {
                    assert_eq!(error_type, expected, "{kind:?}");
                    assert_eq!(message, "attempt to index a nil value (local 't')");
                    assert_eq!(method, "com.example.probe");
                    assert_eq!(line, Some(7));
                }
                other => panic!("{kind:?}: expected a script error, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_timeout_carries_the_hosts_own_sentence() {
        let (_, error) = returned_value(
            "com.example.probe",
            failed(ScriptErrorKind::Timeout, "interrupt"),
        )
        .expect_err("a failed run has no value");
        match error {
            AppError::ScriptError {
                error_type: ScriptErrorType::Timeout,
                message,
                ..
            } => assert_eq!(message, EXECUTION_LIMIT_MESSAGE),
            other => panic!("expected a timeout, got {other:?}"),
        }
    }

    #[test]
    fn an_auth_error_message_recovers_its_401_whatever_the_kind() {
        for kind in [ScriptErrorKind::Runtime, ScriptErrorKind::Timeout] {
            let message = format!("runtime error: {LUA_AUTH_ERROR_PREFIX}DPoP session not found");
            let (_, error) = returned_value("com.example.probe", failed(kind, &message))
                .expect_err("a failed run has no value");
            match error {
                AppError::Auth(message) => assert_eq!(message, "DPoP session not found"),
                other => panic!("{kind:?}: expected an auth error, got {other:?}"),
            }
        }
    }

    /// An interpreter that does not answer at all is read through the same two
    /// limits a run inside it reports.
    #[test]
    fn an_interpreter_that_does_not_answer_keeps_the_two_limits_apart() {
        for (error, expected) in [
            (ExecutionError::Timeout, ScriptErrorType::Timeout),
            (
                ExecutionError::MemoryLimit {
                    requested: 2,
                    ceiling: 1,
                },
                ScriptErrorType::Memory,
            ),
            (
                ExecutionError::MissingExport("execute".into()),
                ScriptErrorType::Runtime,
            ),
        ] {
            let raw = error.to_string();
            let (logged, app_error) =
                returned_value("com.example.probe", Err(DispatchError::Execution(error)))
                    .expect_err("a run that did not happen has no value");
            assert_eq!(logged, raw);
            match app_error {
                AppError::ScriptError {
                    error_type, line, ..
                } => {
                    assert_eq!(error_type, expected, "{raw}");
                    assert_eq!(
                        line, None,
                        "an interpreter that did not answer names no line"
                    );
                }
                other => panic!("{raw}: expected a script error, got {other:?}"),
            }
        }
    }

    /// The language is in the text an operator reads, since the fix is to
    /// install the interpreter that claims it.
    #[test]
    fn a_missing_interpreter_names_the_language_it_was_asked_for() {
        let (raw, _) = returned_value(
            "com.example.probe",
            Err(DispatchError::NoInterpreter {
                language: "typescript".into(),
            }),
        )
        .expect_err("a run that did not happen has no value");
        assert!(raw.contains("typescript"), "{raw}");
    }

    /// The guard reports a run whatever became of it, so a counter cannot be
    /// lost to a path that returns early.
    #[tokio::test]
    async fn a_run_that_reached_no_interpreter_still_moves_the_script_counters() {
        let state = test_state_with_pool(memory_pool().await);
        let lexicon = query_lexicon();
        let counters = state.telemetry_counters.clone();

        execute_query_script(
            &state,
            "com.example.probe",
            &HashMap::new(),
            &lexicon,
            "function handle() return {} end",
            "lua",
            None,
            None,
        )
        .await
        .expect_err("no interpreter is installed, so the run cannot happen");

        assert_eq!(counters.script_executions.load(Ordering::Relaxed), 1);
    }
}
