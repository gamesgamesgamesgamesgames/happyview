//! The project at boot: built and installed before anything runs a script.

use std::path::Path;

use crate::AppState;

use super::build::{BuildOptions, build_snapshot, install};
use super::diagnostic::{self, Diagnostic};

/// What a boot-time build installed.
pub struct Installed {
    pub changes: Vec<String>,
    pub warnings: Vec<Diagnostic>,
    pub generation: u64,
}

/// Held from the start of a build until its snapshot is installed. A build
/// measures its changes against the snapshot installed when it starts, and
/// `install` bumps the generation by reading the current one, so two builds
/// that overlapped would both report the same changes and could install the
/// older files last. It is the only lock over a rebuild: boot, the
/// interpreter-change listener and the dev watcher all take it here.
static INSTALL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Which listener rebuilds the project when an interpreter changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// `spawn_rebuild_on_interpreter_change`, which logs each rebuild.
    Serve,
    /// None from here: `dev::watch` listens in its own loop and prints each
    /// rebuild, and a second listener would build every change twice.
    Dev,
}

/// The project's boot build, then the interpreter-change listener `mode`
/// calls for. Answers the boot build's outcome for the caller to report.
pub async fn start(
    state: &AppState,
    root: &Path,
    mode: Mode,
) -> Result<Installed, Vec<Diagnostic>> {
    let built = super::prepare::rebuild(state, root).await;
    if mode == Mode::Serve {
        super::prepare::spawn_rebuild_on_interpreter_change(state.clone());
    }
    built
}

/// Build the project at `root` and serve it. On failure nothing is installed
/// and every diagnostic comes back, so the caller can refuse to start.
/// Everything that rebuilds reaches this through `prepare::rebuild`.
pub async fn build_and_install(
    state: &AppState,
    root: &Path,
) -> Result<Installed, Vec<Diagnostic>> {
    let turn = INSTALL.lock().await;
    let report = build_snapshot(state, root, BuildOptions::default()).await?;
    let mut warnings = report.warnings;
    tracing::info!(
        root = %root.display(),
        lexicons = report.snapshot.lexicons.len(),
        scripts = report.snapshot.scripts.len(),
        modules = report.snapshot.modules.len(),
        "serving the project"
    );
    install(state, report.snapshot).await;
    // Read before the next build can install over it.
    let generation = state.project.load().generation;
    drop(turn);
    warnings.extend(shadow_warnings(state).await);
    Ok(Installed {
        changes: report.changes,
        warnings,
        generation,
    })
}

/// Dashboard lexicons and scripts the installed project overrides. Said
/// rather than silent, because the dashboard row still exists and an
/// operator editing it would otherwise wonder why nothing changes.
pub async fn shadow_warnings(state: &AppState) -> Vec<Diagnostic> {
    let mut out: Vec<Diagnostic> = state
        .lexicons
        .shadowed()
        .into_iter()
        .map(|(id, path)| {
            Diagnostic::warning(
                path,
                format!(
                    "overrides the dashboard lexicon {id}, which answers again if this file is removed"
                ),
            )
        })
        .collect();
    match state.scripts.list(None).await {
        Ok(scripts) => out.extend(
            scripts
                .into_iter()
                .filter(|script| script.shadows_stored)
                .filter_map(|script| {
                    let path = script.path?;
                    Some(Diagnostic::warning(
                        path,
                        format!(
                            "overrides the dashboard script {}, which answers again if this file is removed",
                            script.id
                        ),
                    ))
                }),
        ),
        Err(e) => tracing::warn!(
            error = ?e,
            "could not read which dashboard scripts the project overrides"
        ),
    }
    diagnostic::sort(&mut out);
    out
}

/// Starts a backfill for each project record lexicon declaring
/// `backfill = true` whose collection is indexed and has never had one. An upload backfills
/// on a lexicon's first revision only; for a file, "first" is a collection
/// the record source has never backfilled (`RecordSource::has_backfill`),
/// which survives restarts because the source keeps that record.
pub async fn start_declared_backfills(state: &AppState) -> Vec<String> {
    let snapshot = state.project.load_full();
    let mut started = Vec::new();
    for lexicon in snapshot.lexicons.values() {
        // An unindexed collection is never ingested, so a backfill would
        // fill a table nothing reads.
        if !lexicon.backfill
            || !lexicon.indexed
            || lexicon.parsed.lexicon_type != crate::lexicon::LexiconType::Record
        {
            continue;
        }
        let collection = &lexicon.parsed.id;
        if state.source.has_backfill(collection).await {
            continue;
        }
        match state
            .start_automatic_backfill(collection.clone(), "happyview.toml")
            .await
        {
            Ok(Some(_)) => started.push(collection.clone()),
            Ok(None) => {}
            Err(e) => tracing::warn!(
                collection = %collection,
                error = %e,
                "failed to start the declared backfill"
            ),
        }
    }
    started
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Two builds started together install one after the other: the later
    /// one measures its changes against the earlier one's snapshot, and
    /// each gets a generation of its own.
    #[tokio::test]
    async fn concurrent_builds_install_in_turn() {
        let state = crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        );
        let root = tempfile::tempdir().unwrap();
        let lexicon = root.path().join("src/lexicons/com/example/thing.json");
        std::fs::create_dir_all(lexicon.parent().unwrap()).unwrap();
        std::fs::write(
            &lexicon,
            r#"{"lexicon":1,"id":"com.example.thing","defs":{"main":{"type":"record","key":"tid","record":{"type":"object","properties":{}}}}}"#,
        )
        .unwrap();
        let before = state.project.load().generation;

        let held = INSTALL.lock().await;
        let both = async {
            tokio::join!(
                build_and_install(&state, root.path()),
                build_and_install(&state, root.path())
            )
        };
        tokio::pin!(both);
        assert!(
            tokio::time::timeout(Duration::from_millis(500), &mut both)
                .await
                .is_err(),
            "a build finished while another held the install lock"
        );
        assert_eq!(state.project.load().generation, before);
        drop(held);

        let (first, second) = both.await;
        let (first, second) = (first.unwrap(), second.unwrap());
        let mut generations = [first.generation, second.generation];
        generations.sort();
        assert_eq!(generations, [before + 1, before + 2]);
        let added: Vec<_> = [&first.changes, &second.changes]
            .into_iter()
            .flatten()
            .filter(|c| c.as_str() == "added lexicon com.example.thing")
            .collect();
        assert_eq!(added.len(), 1, "{:?} / {:?}", first.changes, second.changes);
    }
}
