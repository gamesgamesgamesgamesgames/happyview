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

/// Build the project at `root` and serve it. On failure nothing is installed
/// and every diagnostic comes back, so the caller can refuse to start.
pub async fn build_and_install(
    state: &AppState,
    root: &Path,
) -> Result<Installed, Vec<Diagnostic>> {
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
    warnings.extend(shadow_warnings(state).await);
    Ok(Installed {
        changes: report.changes,
        warnings,
        generation: state.project.load().generation,
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
/// `backfill = true` whose collection has never had one. An upload backfills
/// on a lexicon's first revision only; for a file, "first" is a collection
/// with no network backfill job on record, which survives restarts because
/// those rows are kept. A job for named accounts doesn't count: it fetched
/// those accounts' records, not the collection's.
pub async fn start_declared_backfills(state: &AppState) -> Vec<String> {
    let snapshot = state.project.load_full();
    let mut started = Vec::new();
    for lexicon in snapshot.lexicons.values() {
        if !lexicon.backfill || lexicon.parsed.lexicon_type != crate::lexicon::LexiconType::Record {
            continue;
        }
        let collection = &lexicon.parsed.id;
        let sql = crate::db::adapt_sql(
            "SELECT COUNT(*) FROM happyview_backfill_jobs WHERE collection = ? AND scope = 'network'",
            state.db_backend,
        );
        let seen: Result<(i64,), _> = crate::db::query_as(&sql)
            .bind(collection)
            .fetch_one(&state.db)
            .await;
        match seen {
            Ok((0,)) => {}
            Ok(_) => continue,
            Err(e) => {
                tracing::warn!(
                    collection = %collection,
                    error = %e,
                    "could not tell whether this collection was backfilled; not starting one"
                );
                continue;
            }
        }
        match crate::admin::backfill::start_automatic_backfill(
            state,
            collection.clone(),
            "happyview.toml",
        )
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
