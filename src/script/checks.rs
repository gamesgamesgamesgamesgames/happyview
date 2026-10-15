//! The checks a script body passes before it is stored or served. The admin
//! save path and the project build both call these, so a file cannot reach
//! production by a route the dashboard could not take.

use crate::codemod::{self, ScriptKind};
use crate::lua::NATIVE_LANGUAGE;
use crate::plugin::{
    ChunkRole, DEFAULT_CHUNK_NAME, ExecutionError, ImportRef, PluginExecutor, PluginRegistry,
    PreparedArtifact, ScriptValidateError, ScriptValidateOutput,
};
use crate::project::modules::is_relative;

/// The longest description a script may carry, from either source.
pub const MAX_DESCRIPTION_LEN: usize = 300;

/// The removed v2 globals a Lua body still reaches, in `needs_migration`'s
/// order. Empty when it reaches none, when the body is not Lua, and when the
/// scanner cannot read it: the interpreter's compile check owns the message
/// for a body that does not parse.
///
/// The kind comes from the trigger id, and an empty id (a local module, a
/// test) scans as the unknown kind, which still finds every removed name.
pub fn unmigrated_globals(trigger_id: &str, script_type: &str, body: &str) -> Vec<String> {
    if script_type != NATIVE_LANGUAGE {
        return Vec::new();
    }
    let remaining = codemod::needs_migration(body, ScriptKind::from_trigger_id(trigger_id));
    if remaining.is_empty() || remaining == [codemod::UNPARSEABLE] {
        return Vec::new();
    }
    remaining.into_iter().map(str::to_string).collect()
}

/// What a refusal for removed globals says, wherever it is reported.
pub fn unmigrated_message(globals: &[String]) -> String {
    format!(
        "script references removed globals: {} -- run the codemod first",
        globals.join(", ")
    )
}

/// What a body that passed was prepared into.
#[derive(Debug)]
pub struct Prepared {
    /// The interpreter that checked it, which is the one whose build the
    /// artifact is stamped with.
    pub interpreter_id: String,
    pub artifact: Option<PreparedArtifact>,
    pub imports: Vec<ImportRef>,
}

/// What the interpreter said about a body it could check.
#[derive(Debug)]
pub enum InterpreterCheck {
    Valid(Prepared),
    Refused(Vec<ScriptValidateError>),
}

/// Why a body could not be checked at all.
#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    /// Nothing installed claims the language, so nothing could run the body
    /// either. Carries the dispatcher's own sentence, since the gap and the
    /// fix are the ones a run reports.
    #[error("{0}")]
    NoInterpreter(String),
    /// The interpreter could not answer. The body may be fine.
    #[error("interpreter '{plugin_id}' could not check the script: {message}")]
    InterpreterFailed { plugin_id: String, message: String },
}

/// Ask the interpreter that claims `language` whether it can run `body` as
/// `chunk_name`, and hand back what it prepared. Preparing costs the parse
/// the check already does, and whoever asks is about to store or serve the
/// body.
pub async fn check_with_interpreter(
    registry: &PluginRegistry,
    executor: &PluginExecutor,
    language: &str,
    body: &str,
    chunk_name: &str,
    role: ChunkRole,
) -> Result<InterpreterCheck, CheckError> {
    let interpreter_id = claimant(registry, language).await?;
    let validated = executor
        .prepare_script(&interpreter_id, body, chunk_name, role)
        .await
        .map_err(|e| interpreter_failed(&interpreter_id, e))?;
    Ok(verdict(interpreter_id, validated))
}

/// [`check_with_interpreter`] for a body the dashboard is about to store,
/// keeping what was prepared in the prepared cache so the body's first run
/// parses nothing. Prepare and store are one executor call, so the entry
/// belongs to the build that prepared it even if the interpreter is replaced
/// in between. A body the save will refuse, for the interpreter's reasons or
/// for [`dashboard_import_refusal`]'s, leaves no entry.
pub async fn check_dashboard_body(
    registry: &PluginRegistry,
    executor: &PluginExecutor,
    language: &str,
    body: &str,
) -> Result<InterpreterCheck, CheckError> {
    let interpreter_id = claimant(registry, language).await?;
    let validated = executor
        .prepare_and_store_if(
            &interpreter_id,
            body,
            DEFAULT_CHUNK_NAME,
            ChunkRole::Handler,
            |out| out.valid && dashboard_import_refusal(&out.imports).is_none(),
        )
        .await
        .map_err(|e| interpreter_failed(&interpreter_id, e))?;
    Ok(verdict(interpreter_id, validated))
}

/// Drop the prepared cache's entry for a body no stored row runs any more,
/// after a save replaced it or a delete removed it. A language nothing claims
/// any more has nothing cached under a live interpreter to drop.
pub async fn forget_dashboard_body(registry: &PluginRegistry, language: &str, body: &str) {
    if let Ok(interpreter_id) = claimant(registry, language).await {
        registry.prepared().forget(&interpreter_id, body);
    }
}

/// Why a dashboard script may not import what it does: a relative specifier
/// names a file, and only a project has files for one to name. `None` when
/// every import is a library, which resolves the same wherever the script
/// lives.
pub fn dashboard_import_refusal(imports: &[ImportRef]) -> Option<String> {
    let files: Vec<String> = imports
        .iter()
        .filter(|import| is_relative(&import.specifier))
        .map(|import| format!("{} (line {})", import.specifier, import.line))
        .collect();
    if files.is_empty() {
        return None;
    }
    Some(format!(
        "dashboard scripts can't import files, only project scripts can: {}",
        files.join(", ")
    ))
}

/// The interpreter that claims `language`.
async fn claimant(registry: &PluginRegistry, language: &str) -> Result<String, CheckError> {
    registry
        .get_interpreter_by_language(language)
        .await
        .map(|interpreter| interpreter.info.id.clone())
        .ok_or_else(|| CheckError::NoInterpreter(crate::script::no_interpreter_message(language)))
}

fn interpreter_failed(plugin_id: &str, e: ExecutionError) -> CheckError {
    CheckError::InterpreterFailed {
        plugin_id: plugin_id.to_string(),
        message: e.to_string(),
    }
}

fn verdict(interpreter_id: String, validated: ScriptValidateOutput) -> InterpreterCheck {
    if validated.valid {
        InterpreterCheck::Valid(Prepared {
            interpreter_id,
            artifact: validated.artifact,
            imports: validated.imports,
        })
    } else {
        InterpreterCheck::Refused(validated.errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::loader::{built_fixture, load_from_file};

    #[test]
    fn a_lua_body_reaching_removed_globals_names_them() {
        let body = "function handle()\n  log(now())\n  return db.get(params.uri)\nend\n";
        assert_eq!(
            unmigrated_globals("xrpc.query:com.example.list", "lua", body),
            ["db", "log", "now", "params"]
        );
        assert_eq!(
            unmigrated_message(&["db".into(), "log".into()]),
            "script references removed globals: db, log -- run the codemod first"
        );
    }

    #[test]
    fn other_languages_and_unreadable_bodies_are_not_called_unmigrated() {
        assert!(
            unmigrated_globals("xrpc.query:com.example.list", "javascript", "params.q").is_empty()
        );
        assert!(
            unmigrated_globals("xrpc.query:com.example.list", "lua", "function handle( end")
                .is_empty()
        );
    }

    /// A body with no trigger, such as a local module, is scanned as the
    /// unknown kind rather than skipped.
    #[test]
    fn a_body_with_no_trigger_is_still_scanned() {
        assert_eq!(unmigrated_globals("", "lua", "return db.get(x)\n"), ["db"]);
    }

    #[tokio::test]
    async fn a_language_nobody_claims_is_named() {
        let state =
            crate::test_support::test_state_with_pool(crate::test_support::memory_pool().await);
        let err = check_with_interpreter(
            &state.plugin_registry,
            &state.plugin_executor(),
            "cobol",
            "x",
            crate::plugin::DEFAULT_CHUNK_NAME,
            crate::plugin::ChunkRole::Handler,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, CheckError::NoInterpreter(_)), "{err}");
        assert!(err.to_string().contains("cobol"), "{err}");
    }

    #[tokio::test]
    async fn the_interpreter_s_verdict_comes_back_with_its_lines() {
        let Some(dir) = built_fixture("interpreter_echo", "wasm32-unknown-unknown") else {
            return;
        };
        let state =
            crate::test_support::test_state_with_pool(crate::test_support::memory_pool().await);
        state
            .plugin_registry
            .register(load_from_file(&dir).await.unwrap())
            .await;
        let executor = state.plugin_executor();

        let ok = check_with_interpreter(
            &state.plugin_registry,
            &executor,
            "echo",
            "fine",
            crate::plugin::DEFAULT_CHUNK_NAME,
            crate::plugin::ChunkRole::Handler,
        )
        .await
        .unwrap();
        assert!(matches!(ok, InterpreterCheck::Valid(_)));

        let refused = check_with_interpreter(
            &state.plugin_registry,
            &executor,
            "echo",
            "invalid",
            crate::plugin::DEFAULT_CHUNK_NAME,
            crate::plugin::ChunkRole::Handler,
        )
        .await
        .unwrap();
        let InterpreterCheck::Refused(errors) = refused else {
            panic!("expected a refusal")
        };
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].line, Some(1));
    }
}
