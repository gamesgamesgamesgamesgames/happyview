//! Preparing a project snapshot: every handler, module and test through its
//! interpreter's `validate`, kept as the artifact a run loads, and every
//! literal relative import checked against the result.

use std::path::Path;

use crate::AppState;
use crate::catalog::path_string;
use crate::plugin::{ChunkRole, ImportRef, PreparedArtifact, ScriptErrorKind};
use crate::project::boot::Installed;
use crate::project::build::{BuildEnv, Diagnostic};
use crate::project::modules::{is_relative, resolve_module};
use crate::project::snapshot::{ProjectScript, ProjectSnapshot, ScriptBody};
use crate::script::checks::{self, InterpreterCheck};

/// The literal imports one prepared file reported.
#[derive(Debug)]
pub struct FileImports {
    /// The importing file's project-relative path, which is also the
    /// referrer its imports resolve against.
    pub path: String,
    pub language_id: String,
    pub imports: Vec<ImportRef>,
}

/// What one file prepared into.
struct Ready {
    artifact: Option<PreparedArtifact>,
    imports: Vec<ImportRef>,
}

async fn prepare_one(
    env: &BuildEnv,
    language: &str,
    source: &str,
    path: &str,
    role: ChunkRole,
) -> Result<Ready, Vec<Diagnostic>> {
    match checks::check_with_interpreter(&env.plugins, &env.executor, language, source, path, role)
        .await
    {
        Ok(InterpreterCheck::Valid(prepared)) => Ok(Ready {
            artifact: prepared.artifact,
            imports: prepared.imports,
        }),
        // An interpreter that predates roles is told nothing about what it is
        // reading and demands `handle` of every chunk. Refusing every module
        // and test for that would stop any project with a shared file from
        // building until the interpreter is upgraded; it runs from source
        // instead, with no imports to check.
        Ok(InterpreterCheck::Refused(errors))
            if !role.is_handler()
                && !errors.is_empty()
                && errors
                    .iter()
                    .all(|e| e.kind == ScriptErrorKind::MissingHandle) =>
        {
            Ok(Ready {
                artifact: None,
                imports: Vec::new(),
            })
        }
        Ok(InterpreterCheck::Refused(errors)) if errors.is_empty() => Err(vec![Diagnostic::error(
            path,
            "the interpreter refused the script without saying why",
        )]),
        Ok(InterpreterCheck::Refused(errors)) => Err(errors
            .into_iter()
            .map(|e| Diagnostic::error(path, e.message).at_line(e.line))
            .collect()),
        Err(e) => Err(vec![Diagnostic::error(path, e.to_string())]),
    }
}

/// Prepare one map of scripts in place, recording each file's imports.
async fn prepare_scripts<'s>(
    env: &BuildEnv,
    scripts: impl Iterator<Item = &'s mut ProjectScript>,
    role: ChunkRole,
    found: &mut Vec<FileImports>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for ProjectScript {
        path,
        language_id,
        body,
        ..
    } in scripts
    {
        let ScriptBody::Source { source, artifact } = body;
        let path = path_string(path);
        match prepare_one(env, language_id, source, &path, role).await {
            Ok(ready) => {
                *artifact = ready.artifact;
                found.push(FileImports {
                    path,
                    language_id: language_id.clone(),
                    imports: ready.imports,
                });
            }
            Err(mut errors) => diagnostics.append(&mut errors),
        }
    }
}

/// Prepare every handler, module and test in `snapshot` in place. The
/// artifact sits beside the very source it was prepared from, so a run that
/// sends one is sending what that text compiled to.
pub async fn prepare_files(
    env: &BuildEnv,
    snapshot: &mut ProjectSnapshot,
) -> (Vec<Diagnostic>, Vec<FileImports>) {
    let mut diagnostics = Vec::new();
    let mut found = Vec::new();

    prepare_scripts(
        env,
        snapshot.scripts.values_mut(),
        ChunkRole::Handler,
        &mut found,
        &mut diagnostics,
    )
    .await;
    prepare_scripts(
        env,
        snapshot.tests.values_mut(),
        ChunkRole::Test,
        &mut found,
        &mut diagnostics,
    )
    .await;
    for module in snapshot.modules.values_mut() {
        let path = path_string(&module.path);
        match prepare_one(
            env,
            &module.language_id,
            &module.source,
            &path,
            ChunkRole::Module,
        )
        .await
        {
            Ok(ready) => {
                module.artifact = ready.artifact;
                found.push(FileImports {
                    path,
                    language_id: module.language_id.clone(),
                    imports: ready.imports,
                });
            }
            Err(mut errors) => diagnostics.append(&mut errors),
        }
    }
    (diagnostics, found)
}

/// Each literal relative import must name a module `snapshot` holds, inside
/// `src/`, in a language the importing file's interpreter claims. A bare
/// name is a library and is not the build's to resolve.
///
/// Only meaningful once every file that should be in `snapshot` is: run
/// against a build that has already dropped a file for its own error, a
/// module that is merely broken would be reported as missing.
pub async fn check_imports(
    env: &BuildEnv,
    snapshot: &ProjectSnapshot,
    found: &[FileImports],
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for file in found {
        let relative: Vec<&ImportRef> = file
            .imports
            .iter()
            .filter(|i| is_relative(&i.specifier))
            .collect();
        if relative.is_empty() {
            continue;
        }
        let extensions: Vec<String> = env
            .plugins
            .get_interpreter_by_language(&file.language_id)
            .await
            .map(|p| p.extensions().into_iter().map(str::to_string).collect())
            .unwrap_or_default();
        for import in relative {
            if let Err(e) = resolve_module(
                snapshot,
                &import.specifier,
                &file.path,
                &extensions,
                &file.language_id,
            ) {
                out.push(
                    Diagnostic::error(file.path.as_str(), e.to_string()).at_line(Some(import.line)),
                );
            }
        }
    }
    out
}

/// Held for the length of a rebuild. Boot, the interpreter watcher and a
/// dev-mode file watcher may each ask for one, and `install` numbers a
/// generation by reading the last, so two rebuilds must not interleave.
static REBUILDING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Build the project at `root` and serve it: the one path every build that
/// installs goes through. Answers what `build_and_install` answers so each
/// caller reports the outcome its own way. A failed build installs nothing
/// and the previous snapshot keeps serving: its artifacts may now belong to
/// an interpreter build that is gone, and the stamp check at run time is
/// what makes serving it safe.
pub async fn rebuild(state: &AppState, root: &Path) -> Result<Installed, Vec<Diagnostic>> {
    let _rebuilding = REBUILDING.lock().await;
    crate::project::boot::build_and_install(state, root).await
}

/// Rebuild the served project whenever an interpreter registers or leaves.
/// The changes arrive on a `watch`, which keeps only the latest, and this
/// task rebuilds one at a time, so a burst of changes during a rebuild
/// collapses into a single further rebuild rather than overlapping ones.
pub fn spawn_rebuild_on_interpreter_change(state: AppState) -> tokio::task::JoinHandle<()> {
    let mut changes = state.plugin_registry.interpreter_changes();
    changes.mark_unchanged();
    tokio::spawn(async move {
        while changes.changed().await.is_ok() {
            let Some(root) = state.project.load().root.clone() else {
                continue;
            };
            match rebuild(&state, &root).await {
                Ok(installed) => tracing::info!(
                    root = %root.display(),
                    generation = installed.generation,
                    "project rebuilt for an interpreter change"
                ),
                Err(diagnostics) => {
                    for d in &diagnostics {
                        tracing::warn!("{d}");
                    }
                    tracing::warn!(
                        "the project could not be rebuilt for an interpreter change; the previous build is still being served, from source where its artifacts no longer match"
                    );
                }
            }
        }
    })
}
