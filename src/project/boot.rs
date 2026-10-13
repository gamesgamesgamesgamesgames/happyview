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
