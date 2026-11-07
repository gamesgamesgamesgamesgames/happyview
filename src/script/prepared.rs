//! Choosing what a run may load: the artifact its interpreter prepared from
//! this exact source when one is held, the source otherwise.

use std::sync::Arc;

use crate::AppState;
use crate::catalog::path_string;
use crate::plugin::{
    ChunkRole, DEFAULT_CHUNK_NAME, ModuleScope, PreparedArtifact, ScriptExecuteInput,
};
use crate::project::snapshot::ScriptBody;

use super::input::{Invocation, ScriptOrigin};

/// Fill `input`'s chunk name and candidate artifact for this run and answer
/// the scope its imports resolve in.
///
/// The source stays in place. Which build will load the artifact is known
/// only once `execute_script_in` has instantiated one, so that is where an
/// artifact is either kept and the source cleared, or dropped.
pub(crate) async fn attach_prepared(
    state: &AppState,
    interpreter_id: &str,
    inv: &Invocation<'_>,
    input: &mut ScriptExecuteInput,
) -> ModuleScope {
    match inv.origin {
        ScriptOrigin::Project { snapshot, script } => {
            input.chunk_name = path_string(&script.path);
            // An interpreter runs a usable artifact and ignores the source
            // beside it, so an artifact goes only with the text it was
            // prepared from.
            if let ScriptBody::Source {
                source,
                artifact: Some(artifact),
            } = &script.body
                && **source == *inv.source
            {
                input.artifact = Some(artifact.clone());
            }
            ModuleScope::Project(Arc::clone(snapshot))
        }
        ScriptOrigin::Dashboard => {
            input.chunk_name = DEFAULT_CHUNK_NAME.to_string();
            if let Some(artifact) = dashboard_artifact(state, interpreter_id, inv.source).await {
                input.artifact = Some((*artifact).clone());
            }
            ModuleScope::Dashboard
        }
    }
}

/// The cached artifact for a dashboard script, preparing it on a miss. A
/// row loaded at boot was never saved in this process, so its first run is
/// where it gets prepared. The cache is keyed by the source's hash, which is
/// what ties an artifact to the text it came from.
async fn dashboard_artifact(
    state: &AppState,
    interpreter_id: &str,
    source: &str,
) -> Option<Arc<PreparedArtifact>> {
    if let Some(cached) = state
        .plugin_registry
        .prepared()
        .lookup(interpreter_id, source)
    {
        return cached;
    }
    match state
        .plugin_executor()
        .prepare_and_store(
            interpreter_id,
            source,
            DEFAULT_CHUNK_NAME,
            ChunkRole::Handler,
        )
        .await
    {
        Ok(out) => out.artifact.map(Arc::new),
        // Not stored: an interpreter that did not answer may answer next
        // time, and the run itself will report whatever is wrong.
        Err(e) => {
            tracing::warn!(interpreter = %interpreter_id, error = %e, "could not prepare a dashboard script; running it from source");
            None
        }
    }
}
