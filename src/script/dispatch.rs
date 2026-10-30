//! Finding the interpreter a script's language names, and running it there.

use std::sync::Arc;

use crate::AppState;
use crate::plugin::caller::CallerSession;
use crate::plugin::{ExecutionError, ScriptExecuteOutput};
use crate::project::snapshot::ScriptBody;

use super::input::{Invocation, ScriptOrigin, build_input};

/// Why a dispatch produced no result at all. A script that ran and failed is
/// not here: that arrives as [`ScriptExecuteOutput::Error`], which each runner
/// renders in its own fail mode.
///
/// The boundary text from the host or the interpreter may not cross is the
/// HTTP response envelope rather than the audience: a response body carries a
/// correlation id, while the text itself travels through a log line and the
/// rows a run writes. An admin endpoint is no exception, which is why
/// `validate_body_for_type`'s interpreter failure and a dead-letter retry
/// both answer an id.
#[derive(Debug)]
pub enum DispatchError {
    /// Nothing installed claims the script's language, so there is nothing to
    /// run it. Each runner decides what that means for its caller.
    NoInterpreter {
        language: String,
    },
    Execution(ExecutionError),
}

impl std::fmt::Display for DispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoInterpreter { language } => f.write_str(&no_interpreter_message(language)),
            Self::Execution(e) => e.fmt(f),
        }
    }
}

/// What an operator is told when nothing installed claims `language`.
///
/// Five surfaces say it — a query's and a procedure's 503, a dead-letter row,
/// a job's error column, and a save's refusal — and all five are read by the
/// one person who can act on it, so they say the same thing in the same words
/// rather than each describing the gap from its own side.
pub fn no_interpreter_message(language: &str) -> String {
    format!("no interpreter installed for '{language}' scripts; install one from the plugins page")
}

impl std::error::Error for DispatchError {}

impl From<ExecutionError> for DispatchError {
    fn from(e: ExecutionError) -> Self {
        Self::Execution(e)
    }
}

/// Run one invocation through the interpreter that claims its language,
/// lending it `caller`'s credentials where the runner has them.
///
/// The artifact attached here is a candidate: `execute_script_in` decides
/// against the build it actually instantiates whether the run loads it or the
/// source.
///
/// The wall clock is not set here. `execute_script_as` arms it per run and
/// lifts it for a job, so a budget cannot be set once and then outlive the run
/// it was meant for.
///
/// A compiled module is its own interpreter, so it runs as itself rather than
/// through the interpreter its language would name. It is read from the
/// snapshot the run pinned, so a rebuild mid-run cannot swap its bytes.
pub async fn dispatch(
    state: &AppState,
    inv: &Invocation<'_>,
    caller: Option<Arc<CallerSession>>,
) -> Result<ScriptExecuteOutput, DispatchError> {
    if let ScriptOrigin::Project { snapshot, script } = inv.origin
        && let ScriptBody::Module { module, hash } = &script.body
    {
        // The build only emits a module script beside its module, so a miss
        // is a snapshot that broke its own invariant.
        let compiled = snapshot.script_modules.get(hash).ok_or_else(|| {
            DispatchError::Execution(ExecutionError::PluginNotFound(
                crate::project::script_module::module_plugin_id(module),
            ))
        })?;
        let input = build_input(state, inv).await;
        return Ok(state
            .plugin_executor()
            .execute_compiled_as(
                compiled.plugin.clone(),
                &compiled.source_path,
                &input,
                caller,
            )
            .await?);
    }
    let interpreter = state
        .plugin_registry
        .get_interpreter_by_language(inv.language)
        .await
        .ok_or_else(|| DispatchError::NoInterpreter {
            language: inv.language.to_string(),
        })?;
    let mut input = build_input(state, inv).await;
    let modules =
        super::prepared::attach_prepared(state, &interpreter.info.id, inv, &mut input).await;
    Ok(state
        .plugin_executor()
        .execute_script_in(&interpreter.info.id, &input, caller, modules)
        .await?)
}
