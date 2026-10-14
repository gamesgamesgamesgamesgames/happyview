//! `--watch`: rerun the affected test files whenever the project changes.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc::UnboundedReceiver;

use crate::dev::watch::{
    Change, SharedWatcher, WatchMode, debounced, parse_poll_ms, start_watcher, watch_again,
};
use crate::project::snapshot::ProjectSnapshot;

use super::report::{exit_code, render};
use super::runner::{Runner, print_diagnostics};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Affected {
    All,
    /// Keys of the new snapshot's tests.
    Only(BTreeSet<String>),
}

/// Which test files a batch of changed paths reruns. Which handler or module
/// a test reaches is decided at run time by the triggers it fires, so no
/// static reading can say a change to anything but a test file leaves a test
/// alone: such a change reruns them all.
pub fn affected(changed: &[String], before: &ProjectSnapshot, after: &ProjectSnapshot) -> Affected {
    let is_test = |path: &String| before.tests.contains_key(path) || after.tests.contains_key(path);
    if !changed.iter().all(is_test) {
        return Affected::All;
    }
    Affected::Only(
        changed
            .iter()
            .filter(|path| after.tests.contains_key(*path))
            .cloned()
            .collect(),
    )
}

/// The project files a run's configuration is loaded from. A change to one
/// is loaded again before the build, as the dev server reloads its own.
const CONFIG_FILES: [&str; 2] = ["happyview.toml", ".env.test"];

pub struct WatchState {
    pub snapshot: Arc<ProjectSnapshot>,
    /// The exit code of the most recent run, which the command exits with
    /// if the watcher ever stops.
    pub code: u8,
    /// Changes whose build failed. They join the next batch, so the build
    /// that finally succeeds reruns what they affect as well as what the
    /// fixing save does.
    pending: BTreeSet<String>,
}

impl WatchState {
    pub fn new(snapshot: Arc<ProjectSnapshot>, code: u8) -> WatchState {
        WatchState {
            snapshot,
            code,
            pending: BTreeSet::new(),
        }
    }
}

/// One batch of changes: rebuild, and rerun what it affects. A failed build
/// keeps the previous snapshot as the one later changes are compared with,
/// as the dev server keeps serving it.
pub async fn step(
    runner: &mut Runner,
    state: &mut WatchState,
    changed: &[String],
    filter: Option<&str>,
    out: &mut (dyn Write + Send),
) {
    state.pending.extend(changed.iter().cloned());
    let changed: Vec<String> = state.pending.iter().cloned().collect();
    if changed
        .iter()
        .any(|path| CONFIG_FILES.contains(&path.as_str()))
        && let Err(e) = runner.reload_config()
    {
        // A refused load leaves the previous configuration installed, and
        // the build below reads the same file and reports why.
        tracing::debug!("the project configuration did not load: {e}");
    }
    let after = match runner.build().await {
        Ok((after, warnings)) => {
            print_diagnostics(&warnings, out);
            after
        }
        Err(diagnostics) => {
            print_diagnostics(&diagnostics, out);
            let _ = writeln!(out, "\nthe build failed; waiting for changes");
            return;
        }
    };
    state.pending.clear();
    let only = match affected(&changed, &state.snapshot, &after) {
        Affected::All => None,
        // Only deleted test files: nothing left to run.
        Affected::Only(keys) if keys.is_empty() => {
            state.snapshot = after;
            return;
        }
        Affected::Only(keys) => Some(keys),
    };
    let started = Instant::now();
    let results = runner.run(&after, only.as_ref(), filter).await;
    state.snapshot = after;
    if results.is_empty() {
        let _ = match filter {
            Some(filter) => writeln!(out, "no affected test file matches '{filter}'"),
            None => writeln!(out, "no test files: name one *.test.<ext> under src/"),
        };
    } else if let Ok(summary) = render(&results, started.elapsed(), out) {
        state.code = exit_code(&summary);
    }
    let _ = writeln!(out, "\nwaiting for changes");
}

/// Take changed paths from `changes` until every sender is gone, one build
/// and one run per batch. Each run is awaited to its end before the next
/// batch is read, so a save during a run waits for it instead of overlapping
/// it: a guest that spins cannot be interrupted before its deadline, and a
/// run abandoned mid-way would leave its files running beside the next.
pub async fn rerun(
    runner: &mut Runner,
    state: &mut WatchState,
    changes: &mut UnboundedReceiver<String>,
    filter: Option<&str>,
    out: &mut (dyn Write + Send),
) {
    while let Some(mut changed) = debounced(changes).await {
        changed.sort();
        changed.dedup();
        step(runner, state, &changed, filter, out).await;
    }
}

/// Watch the project at `root`, sending each changed input as the path that
/// keys it in the snapshot: relative to the root, joined by `/`. The watch
/// lasts as long as the returned watcher.
fn watch_project(
    root: &Path,
    mode: WatchMode,
) -> Result<(SharedWatcher, UnboundedReceiver<String>), String> {
    // Canonical, because the watcher reports canonical paths and a relative
    // or symlinked root would strip nothing from them.
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let (tx, changes) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (recreated_tx, mut recreated) = tokio::sync::mpsc::unbounded_channel::<Change>();
    let watched_root = root.clone();
    let watcher = start_watcher(&root, mode, move |change, path| {
        if matches!(change, Change::SrcDir | Change::BuildDir) {
            let _ = recreated_tx.send(change);
        }
        if let Ok(rel) = path.strip_prefix(&watched_root) {
            let _ = tx.send(crate::catalog::path_string(rel));
        }
    })?;
    // A recreated `src/` loses its watch. It is added again from a task, as
    // the dev server does, rather than from inside the watcher's own event
    // callback.
    let rewatch = Arc::downgrade(&watcher);
    tokio::spawn(async move {
        while let Some(batch) = debounced(&mut recreated).await {
            watch_again(
                &rewatch,
                &root,
                batch.contains(&Change::SrcDir),
                batch.contains(&Change::BuildDir),
            );
        }
    });
    Ok((watcher, changes))
}

/// `code` is the initial run's, which a session that sees no change exits
/// with.
pub async fn watch(
    runner: &mut Runner,
    snapshot: Arc<ProjectSnapshot>,
    code: u8,
    filter: Option<&str>,
    out: &mut (dyn Write + Send),
) -> u8 {
    let mode = match parse_poll_ms(crate::config::vars::var("HAPPYVIEW_WATCH_POLL_MS").as_deref()) {
        Ok(mode) => mode,
        Err(e) => {
            let _ = writeln!(out, "{e}");
            return 2;
        }
    };
    // Held for the session: dropping the watcher stops the watch and ends
    // the channel.
    let (watcher, mut changes) = match watch_project(&runner.root, mode) {
        Ok(watching) => watching,
        Err(e) => {
            let _ = writeln!(out, "{e}");
            return 2;
        }
    };
    let _ = writeln!(out, "\nwaiting for changes");
    let mut state = WatchState::new(snapshot, code);
    rerun(runner, &mut state, &mut changes, filter, out).await;
    drop(watcher);
    state.code
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::snapshot::{ProjectScript, ScriptBody};

    fn snapshot(tests: &[&str]) -> ProjectSnapshot {
        let mut snapshot = ProjectSnapshot::empty();
        for path in tests {
            snapshot.tests.insert(
                path.to_string(),
                ProjectScript {
                    trigger: String::new(),
                    path: std::path::PathBuf::from(path),
                    language_id: "lua".into(),
                    body: ScriptBody::Source {
                        source: "".into(),
                        artifact: None,
                    },
                    description: None,
                    outbound_xrpcs: Vec::new(),
                },
            );
        }
        snapshot
    }

    fn changed(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    /// What the watcher sends is the snapshot's own key for the file, and
    /// `.env.test` is one of the inputs it reports.
    #[tokio::test]
    async fn the_watcher_sends_the_snapshot_key() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/lib")).unwrap();
        std::fs::create_dir_all(root.join(".happyview")).unwrap();
        let (_watcher, mut changes) =
            watch_project(root, WatchMode::Poll(std::time::Duration::from_millis(50))).unwrap();

        async fn until(changes: &mut UnboundedReceiver<String>, key: &str) {
            let seen = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while let Some(path) = changes.recv().await {
                    if path == key {
                        return;
                    }
                }
            })
            .await;
            assert!(seen.is_ok(), "the watcher never sent {key}");
        }
        std::fs::write(root.join("src/lib/a.test.echo"), "x").unwrap();
        until(&mut changes, "src/lib/a.test.echo").await;
        std::fs::write(root.join(".env.test"), "A=1\n").unwrap();
        until(&mut changes, ".env.test").await;
    }

    #[test]
    fn only_test_files_changed_reruns_only_those() {
        let before = snapshot(&["src/a.test.lua", "src/b.test.lua"]);
        let after = snapshot(&["src/a.test.lua", "src/b.test.lua"]);
        assert_eq!(
            affected(&changed(&["src/a.test.lua"]), &before, &after),
            Affected::Only(BTreeSet::from(["src/a.test.lua".to_string()]))
        );
    }

    #[test]
    fn a_new_test_file_runs_and_a_deleted_one_is_dropped() {
        let before = snapshot(&["src/a.test.lua"]);
        let after = snapshot(&["src/b.test.lua"]);
        assert_eq!(
            affected(
                &changed(&["src/a.test.lua", "src/b.test.lua"]),
                &before,
                &after
            ),
            Affected::Only(BTreeSet::from(["src/b.test.lua".to_string()]))
        );
    }

    #[test]
    fn any_other_change_reruns_everything() {
        let s = snapshot(&["src/a.test.lua"]);
        for path in [
            "src/lib/format.lua",
            "src/lexicons/x/y.json",
            "happyview.toml",
            ".happyview/build/m.wasm",
            "src",
        ] {
            assert_eq!(affected(&changed(&[path]), &s, &s), Affected::All, "{path}");
        }
        assert_eq!(
            affected(&changed(&["src/a.test.lua", "src/lib/format.lua"]), &s, &s),
            Affected::All
        );
    }
}
