use axum::response::Response;
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
use crate::plugin::{ExecutionError, ScriptExecuteOutput, ScriptSpace};
use crate::repo;
use crate::script::{DispatchError, Invocation, dispatch};
use crate::telemetry::counters::Counters;

use super::context;
use super::scripts::{ResolvedScript, trigger_of};

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

/// The 401 an `AUTH_ERROR:` prefix recovers: a library's credential failure,
/// raised through the script, which a caller can act on rather than report.
///
/// Every text a failure carries is searched, because which of them keeps the
/// prefix is the interpreter's choice: one that trims its `message` to the
/// line's own text may still carry the prefix in `raw`, and a 401 that
/// degraded to a 500 would read as this instance being broken.
fn auth_error(texts: &[&str]) -> Option<AppError> {
    texts
        .iter()
        .find(|text| text.contains(LUA_AUTH_ERROR_PREFIX))
        .map(|text| AppError::Auth(auth_message_after_prefix(text)))
}

/// A failure the script owns: its kind, its own text and its line reach the
/// caller, which is what [`AppError::ScriptError`] exists to allow. A spent
/// budget carries [`EXECUTION_LIMIT_MESSAGE`] instead, since the limit is the
/// host's rather than the script's.
fn script_error(
    method: &str,
    error_type: ScriptErrorType,
    message: String,
    line: Option<u32>,
) -> AppError {
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
        }) => {
            let error = auth_error(&[&message, &raw])
                .unwrap_or_else(|| script_error(method, kind.into(), message, line));
            Err((raw, error))
        }
        // An endpoint whose interpreter is absent cannot answer until an
        // operator installs one, which is what 503 says. Answering anything
        // else would hand a caller a reply the script never shaped; a
        // correlation id would hide a sentence that names only the language
        // the row asked for and nothing about this instance.
        Err(e @ DispatchError::NoInterpreter { .. }) => {
            let raw = e.to_string();
            Err((raw.clone(), AppError::ServerMisconfigured(raw)))
        }
        // A run that produced no result at all is told apart by whose failure
        // it describes, not by how bad it is. The two limits describe the
        // script's own run and keep its envelope; every other way an
        // interpreter fails to answer describes this instance.
        //
        // "Your script failed" and "we could not run anything" are also
        // different claims, so they do not share an envelope.
        Err(e) => {
            let raw = e.to_string();
            let error = auth_error(&[&raw]).unwrap_or_else(|| match &e {
                DispatchError::Execution(ExecutionError::Timeout) => {
                    script_error(method, ScriptErrorType::Timeout, raw.clone(), None)
                }
                DispatchError::Execution(ExecutionError::MemoryLimit { .. }) => {
                    script_error(method, ScriptErrorType::Memory, raw.clone(), None)
                }
                _ => AppError::Internal(raw.clone()),
            });
            Err((raw, error))
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
    script: &ResolvedScript,
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
    let script_source = script.body.clone();
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
    let space = space_ctx.map(ScriptSpace::from);

    let outcome = dispatch(
        state,
        &Invocation {
            trigger_id: &script.id,
            trigger: trigger_of(script, None).map_err(AppError::Internal)?,
            language: &script.script_type,
            source: &script.body,
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

    // The lexicon's declared output encoding decides whether this answers
    // JSON or bytes, so a method declaring `application/json` is never
    // examined for the byte forms and cannot be reinterpreted.
    let answer = crate::script::response::resolve(
        &state.db,
        backend,
        method,
        &crate::script::encoding::of_output(lexicon),
        json_value,
    )
    .await?;

    span.in_scope(|| {
        tracing::info!(
            duration_ms = start.elapsed().as_millis() as u64,
            "script execution completed"
        );
    });
    if state.verbose_event_logging.load(Ordering::Relaxed) {
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
                    "response_size": answer.response_size(),
                    "input": input_json,
                    "response": answer.log_detail(),
                }),
            },
            backend,
        )
        .await;
    }

    Ok(answer.into_response())
}

/// Execute a query endpoint's script.
#[allow(clippy::too_many_arguments)]
pub async fn execute_query_script(
    state: &AppState,
    method: &str,
    params: &HashMap<String, Value>,
    lexicon: &ParsedLexicon,
    script: &ResolvedScript,
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
    let script_source = script.body.clone();

    // A query's parameters are the first argument of `handle`, which is why
    // `ctx.params` carries nothing: the same values twice would leave a script
    // author guessing which one a runner fills.
    let input_json = Value::Object(params.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
    let space = space_ctx.map(ScriptSpace::from);

    let outcome = dispatch(
        state,
        &Invocation {
            trigger_id: &script.id,
            trigger: trigger_of(script, None).map_err(AppError::Internal)?,
            language: &script.script_type,
            source: &script.body,
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

    // The lexicon's declared output encoding decides whether this answers
    // JSON or bytes, so a method declaring `application/json` is never
    // examined for the byte forms and cannot be reinterpreted.
    let answer = crate::script::response::resolve(
        &state.db,
        backend,
        method,
        &crate::script::encoding::of_output(lexicon),
        json_value,
    )
    .await?;

    span.in_scope(|| {
        tracing::info!(
            duration_ms = start.elapsed().as_millis() as u64,
            "script execution completed"
        );
    });
    if state.verbose_event_logging.load(Ordering::Relaxed) {
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
                    "response_size": answer.response_size(),
                    "params": params,
                    "response": answer.log_detail(),
                }),
            },
            backend,
        )
        .await;
    }

    Ok(answer.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexicon::{LexiconType, ProcedureAction};
    use crate::plugin::{ExecutionError, ScriptErrorKind, ScriptValueKind};
    use crate::test_support::{memory_pool, migrated_memory_pool, test_state_with_pool};
    use axum::response::IntoResponse;

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

    /// A query row bound to the lexicon above, as a resolve would answer it.
    fn query_script(body: &str) -> ResolvedScript {
        ResolvedScript {
            id: "xrpc.query:com.example.probe".to_string(),
            trigger: crate::lua::TriggerKind::XrpcQuery,
            script_type: "lua".to_string(),
            body: body.to_string(),
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

    /// An interpreter that trims its `message` keeps the prefix in `raw`, and
    /// the 401 has to survive either spelling.
    #[test]
    fn an_auth_error_is_recovered_from_the_unparsed_text_alone() {
        let (_, error) = returned_value(
            "com.example.probe",
            Ok(ScriptExecuteOutput::Error {
                kind: ScriptErrorKind::Runtime,
                message: "caller.create_record failed".into(),
                line: Some(4),
                raw: format!(
                    "[string \"script\"]:4: {LUA_AUTH_ERROR_PREFIX}DPoP session not found"
                ),
            }),
        )
        .expect_err("a failed run has no value");
        match error {
            AppError::Auth(message) => assert_eq!(message, "DPoP session not found"),
            other => panic!("expected an auth error, got {other:?}"),
        }
    }

    /// The two limits describe the script's own run, so they keep its envelope
    /// and its `errorType`.
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

    /// A failure that describes this instance rather than the script reaches a
    /// caller as a correlation id and nothing else. Its text is an operator's:
    /// it names wasmtime internals, a missing export, a plugin id — none of
    /// which is a stranger's to read, and none of which a script author could
    /// act on. The text still reaches the operator, through the log line
    /// `AppError::Internal` writes and through the `script.error` row.
    #[tokio::test]
    async fn a_host_origin_failure_answers_a_correlation_id_rather_than_its_text() {
        const SECRET: &str = "wasm trap: wasm backtrace at /srv/happyview/probe.wasm";
        for error in [
            DispatchError::Execution(ExecutionError::Trap(wasmtime::Error::msg(SECRET))),
            DispatchError::Execution(ExecutionError::Instantiation(anyhow::anyhow!(SECRET))),
            DispatchError::Execution(ExecutionError::MissingExport(SECRET.into())),
            DispatchError::Execution(ExecutionError::NotAnInterpreter(SECRET.into())),
            DispatchError::Execution(ExecutionError::InvalidResponse(SECRET.into())),
        ] {
            let label = error.to_string();
            let (logged, app_error) = returned_value("com.example.probe", Err(error))
                .expect_err("a run that did not happen has no value");
            assert!(
                logged.contains(SECRET),
                "{label}: the operator's record must keep the text"
            );

            let response = app_error.into_response();
            assert_eq!(
                response.status(),
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "{label}"
            );
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("a body");
            let body: Value = serde_json::from_slice(&body).expect("a JSON body");
            assert!(
                body["correlationId"].is_string(),
                "{label}: {body} carries no correlation id"
            );
            assert!(
                !body.to_string().contains(SECRET),
                "{label}: {body} leaks the host's own text"
            );
        }
    }

    /// An absent interpreter is the one host-origin failure whose text a
    /// caller gets in full. It names no internals — only the language the row
    /// asked for — and an operator reading a correlation id would learn less
    /// than the sentence itself says.
    #[tokio::test]
    async fn an_absent_interpreter_answers_a_503_naming_the_language() {
        let (logged, app_error) = returned_value(
            "com.example.probe",
            Err(DispatchError::NoInterpreter {
                language: "typescript".into(),
            }),
        )
        .expect_err("a run that did not happen has no value");
        assert!(logged.contains("typescript"), "{logged}");

        let response = app_error.into_response();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("a body");
        let body: Value = serde_json::from_slice(&body).expect("a JSON body");
        assert_eq!(body["error"], "ServerMisconfigured");
        let message = body["message"].as_str().expect("a message");
        assert!(message.contains("typescript"), "{body}");
        assert!(message.contains("plugins page"), "{body}");
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
            &query_script("function handle() return {} end"),
            None,
            None,
        )
        .await
        .expect_err("no interpreter is installed, so the run cannot happen");

        assert_eq!(counters.script_executions.load(Ordering::Relaxed), 1);
    }

    async fn executed_event_count(state: &AppState) -> i64 {
        let row: (i64,) = crate::db::query_as(
            "SELECT COUNT(*) FROM happyview_event_logs WHERE event_type = 'script.executed'",
        )
        .fetch_one(&state.db)
        .await
        .expect("count script.executed events");
        row.0
    }

    /// Query and procedure scripts run on every XRPC call, and `script.executed`
    /// stores the full request and response, so the row is gated like the other
    /// per-request telemetry.
    #[tokio::test]
    #[ignore = "needs an interpreter plugin, and v3 ships none to tests"]
    async fn script_executed_is_not_logged_while_verbose_logging_is_off() {
        let state = test_state_with_pool(migrated_memory_pool().await);
        let lexicon = query_lexicon();
        let params = HashMap::new();

        let result = execute_query_script(
            &state,
            "com.example.probe",
            &params,
            &lexicon,
            &query_script("function handle() return { ok = true } end"),
            None,
            None,
        )
        .await;

        assert!(
            result.is_ok(),
            "script should have executed: {:?}",
            result.err()
        );
        assert_eq!(
            executed_event_count(&state).await,
            0,
            "script.executed must not be logged while verbose event logging is off"
        );
    }

    #[tokio::test]
    #[ignore = "needs an interpreter plugin, and v3 ships none to tests"]
    async fn script_executed_is_logged_when_verbose_logging_is_on() {
        let state = test_state_with_pool(migrated_memory_pool().await);
        state.verbose_event_logging.store(true, Ordering::Relaxed);
        let lexicon = query_lexicon();
        let params = HashMap::new();

        let result = execute_query_script(
            &state,
            "com.example.probe",
            &params,
            &lexicon,
            &query_script("function handle() return { ok = true } end"),
            None,
            None,
        )
        .await;

        assert!(
            result.is_ok(),
            "script should have executed: {:?}",
            result.err()
        );
        assert_eq!(
            executed_event_count(&state).await,
            1,
            "script.executed must still be logged when verbose event logging is on"
        );
    }
}
