//! Rebuilding the project snapshot when its inputs change.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use notify_debouncer_mini::notify::{self, PollWatcher, RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{DebounceEventResult, Debouncer, new_debouncer_opt};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::AppState;
use crate::dev::{DevSession, report};

/// Long enough to fold an editor's write-then-rename, or a formatter's
/// second write, into one build; short enough that a save feels immediate.
const DEBOUNCE: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Source,
    Config,
    /// `src/` itself appeared or was replaced, so its watch has to be added
    /// again: inotify drops a watch whose directory is deleted.
    SrcDir,
    /// `.happyview/build` itself appeared or was replaced, so its watch has
    /// to be added again.
    BuildDir,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Batch {
    config: bool,
    src_dir: bool,
    build_dir: bool,
}

fn build_dir(root: &Path) -> PathBuf {
    root.join(".happyview").join("build")
}

/// Each input is watched on its own rather than the whole root
/// recursively, which would also cover `node_modules/`, `target/` and the
/// database that every request writes to. The root and `.happyview/` are
/// shallow watches: the first sees `happyview.toml` replaced by rename, the
/// second sees `build/` appear or be recreated.
pub fn watched_paths(root: &Path) -> Vec<(PathBuf, RecursiveMode)> {
    let mut paths = vec![
        (root.join("src"), RecursiveMode::Recursive),
        (root.to_path_buf(), RecursiveMode::NonRecursive),
        (root.join(".happyview"), RecursiveMode::NonRecursive),
    ];
    if build_dir(root).is_dir() {
        paths.push((build_dir(root), RecursiveMode::NonRecursive));
    }
    paths
}

pub fn classify(root: &Path, path: &Path) -> Option<Change> {
    let rel = path.strip_prefix(root).ok()?;
    if rel == Path::new("happyview.toml") {
        return Some(Change::Config);
    }
    if rel == Path::new("src") {
        return Some(Change::SrcDir);
    }
    if rel.starts_with("src") {
        return Some(Change::Source);
    }
    let build = Path::new(".happyview").join("build");
    if rel == build {
        return Some(Change::BuildDir);
    }
    // A module hpy finished writing: directly inside build/, ending .wasm.
    // Its temporary files and anything nested are not inputs.
    if rel.parent() == Some(build.as_path()) && rel.extension().is_some_and(|e| e == "wasm") {
        return Some(Change::Source);
    }
    None
}

/// Wait for a signal, then take every other one already queued. `None` when
/// every sender is gone.
async fn next_batch(rx: &mut UnboundedReceiver<Change>) -> Option<Batch> {
    let mut batch = Batch::default();
    let mut note = |change: Change| match change {
        Change::Config => batch.config = true,
        Change::SrcDir => batch.src_dir = true,
        Change::BuildDir => batch.build_dir = true,
        Change::Source => {}
    };
    note(rx.recv().await?);
    while let Ok(change) = rx.try_recv() {
        note(change);
    }
    Some(batch)
}

/// One build of the project at `root`, installed if it succeeds, and one
/// report either way.
pub async fn rebuild(state: &AppState, root: &Path) {
    let started = Instant::now();
    match crate::project::boot::build_and_install(state, root).await {
        Ok(installed) => {
            println!(
                "{}",
                report::built(&installed.changes, &installed.warnings, started.elapsed())
            );
            // Idempotent per collection, and a no-op unless [dev] backfill
            // is on, so a lexicon declared since boot gets its backfill now.
            crate::project::boot::start_declared_backfills(state).await;
        }
        Err(diagnostics) => println!("{}", report::failed(&diagnostics)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchMode {
    Native,
    Poll(Duration),
}

const POLL_MS_RANGE: std::ops::RangeInclusive<u64> = 50..=60_000;

/// `HAPPYVIEW_WATCH_POLL_MS`: unset is native events; otherwise whole
/// milliseconds in range, since a typo that silently fell back to native
/// events would look exactly like the failure polling is there to fix.
pub fn parse_poll_ms(raw: Option<&str>) -> Result<WatchMode, String> {
    let Some(raw) = raw else {
        return Ok(WatchMode::Native);
    };
    match raw.parse::<u64>() {
        Ok(ms) if POLL_MS_RANGE.contains(&ms) => Ok(WatchMode::Poll(Duration::from_millis(ms))),
        _ => Err(format!(
            "HAPPYVIEW_WATCH_POLL_MS must be whole milliseconds from {} to {}, got {raw:?}",
            POLL_MS_RANGE.start(),
            POLL_MS_RANGE.end()
        )),
    }
}

/// The two watchers are different types behind one debouncer, and the loop
/// only ever needs to add a watch.
enum AnyDebouncer {
    Native(Debouncer<RecommendedWatcher>),
    Poll(Debouncer<PollWatcher>),
}

/// Adding a watch is all the rebuild loop does with its debouncer, so a test
/// can stand in for one.
trait Watch {
    fn watch(&mut self, path: &Path, mode: RecursiveMode) -> notify::Result<()>;
}

impl Watch for AnyDebouncer {
    fn watch(&mut self, path: &Path, mode: RecursiveMode) -> notify::Result<()> {
        match self {
            Self::Native(d) => d.watcher().watch(path, mode),
            Self::Poll(d) => d.watcher().watch(path, mode),
        }
    }
}

type SharedDebouncer = Arc<Mutex<AnyDebouncer>>;

/// A debouncer of the chosen kind, watching every path in `watched_paths`,
/// that sends each classified change to `tx`.
fn start_watcher(
    root: &Path,
    mode: WatchMode,
    tx: UnboundedSender<Change>,
) -> Result<SharedDebouncer, String> {
    let filter_root = root.to_path_buf();
    let handler = move |result: DebounceEventResult| match result {
        Ok(events) => {
            for change in events
                .iter()
                .filter_map(|e| classify(&filter_root, &e.path))
            {
                let _ = tx.send(change);
            }
        }
        Err(e) => eprintln!("file watcher: {e}"),
    };
    let config = notify_debouncer_mini::Config::default().with_timeout(DEBOUNCE);
    let debouncer = match mode {
        WatchMode::Native => AnyDebouncer::Native(
            new_debouncer_opt::<_, RecommendedWatcher>(config, handler)
                .map_err(|e| format!("could not start the file watcher: {e}"))?,
        ),
        WatchMode::Poll(interval) => AnyDebouncer::Poll(
            new_debouncer_opt::<_, PollWatcher>(
                config.with_notify_config(notify::Config::default().with_poll_interval(interval)),
                handler,
            )
            .map_err(|e| format!("could not start the polling file watcher: {e}"))?,
        ),
    };
    let debouncer = Arc::new(Mutex::new(debouncer));
    for (path, recursive) in watched_paths(root) {
        debouncer
            .lock()
            .unwrap()
            .watch(&path, recursive)
            .map_err(|e| format!("could not watch {}: {e}", path.display()))?;
    }
    Ok(debouncer)
}

/// The only strong reference to the debouncer: dropping it stops the watch,
/// which drops the channel's sender and ends the rebuild loop.
pub struct WatchHandle {
    _debouncer: SharedDebouncer,
    mode: WatchMode,
}

impl WatchHandle {
    pub fn mode(&self) -> WatchMode {
        self.mode
    }
}

pub fn spawn(state: AppState, session: DevSession) -> Result<WatchHandle, String> {
    let root = session.root.clone();
    let mode = session.watch_mode;
    // A project scaffolded without src/ yet would otherwise have nothing to
    // watch until restarted. `.happyview/` already exists: it holds the
    // secrets and the database.
    std::fs::create_dir_all(root.join("src"))
        .map_err(|e| format!("could not create {}: {e}", root.join("src").display()))?;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Change>();
    let debouncer = start_watcher(&root, mode, tx)?;

    let rewatch: Weak<Mutex<AnyDebouncer>> = Arc::downgrade(&debouncer);
    let mut declared = state.config.project.clone();
    tokio::spawn(async move {
        while let Some(batch) = next_batch(&mut rx).await {
            handle_batch(&state, &session, &rewatch, &mut declared, batch).await;
        }
    });
    Ok(WatchHandle {
        _debouncer: debouncer,
        mode,
    })
}

/// One batch's work: watch again any directory that was recreated, reload
/// the config if it changed, then build.
async fn handle_batch<W: Watch>(
    state: &AppState,
    session: &DevSession,
    rewatch: &Weak<Mutex<W>>,
    declared: &mut Option<Arc<crate::config::project::ProjectConfig>>,
    batch: Batch,
) {
    let recreated = [
        (
            batch.src_dir,
            session.root.join("src"),
            RecursiveMode::Recursive,
        ),
        (
            batch.build_dir,
            build_dir(&session.root),
            RecursiveMode::NonRecursive,
        ),
    ];
    for (changed, path, recursive) in recreated {
        if changed
            && path.is_dir()
            && let Some(debouncer) = rewatch.upgrade()
        {
            // Watching a path twice is harmless; a failure means the
            // directory went away again, and its reappearance retries.
            let _ = debouncer.lock().unwrap().watch(&path, recursive);
        }
    }
    if batch.config
        && let Err(e) = crate::dev::reload::reload(state, session, declared).await
    {
        // The snapshot build below reads the same file and reports the same
        // error with the standing line.
        tracing::debug!("happyview.toml did not load: {e}");
    }
    rebuild(state, &session.root).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn project_inputs_are_all_that_count() {
        let root = Path::new("/p");
        assert_eq!(
            classify(root, Path::new("/p/src/lexicons/a.json")),
            Some(Change::Source)
        );
        assert_eq!(classify(root, Path::new("/p/src")), Some(Change::SrcDir));
        assert_eq!(
            classify(root, Path::new("/p/happyview.toml")),
            Some(Change::Config)
        );
        assert_eq!(
            classify(root, Path::new("/p/.happyview/build/registry.wasm")),
            Some(Change::Source)
        );
        assert_eq!(
            classify(root, Path::new("/p/.happyview/build")),
            Some(Change::BuildDir)
        );
        assert_eq!(
            classify(root, Path::new("/p/.happyview/build/registry.wasm.tmp")),
            None
        );
        assert_eq!(
            classify(root, Path::new("/p/.happyview/build/nested/x.wasm")),
            None
        );
        assert_eq!(
            classify(root, Path::new("/p/.happyview/happyview.db-wal")),
            None
        );
        assert_eq!(
            classify(root, Path::new("/p/.happyview/plugins/lua/manifest.json")),
            None
        );
        assert_eq!(classify(root, Path::new("/p/.env")), None);
        assert_eq!(classify(root, Path::new("/p/srcery/x.lua")), None);
        assert_eq!(classify(root, Path::new("/elsewhere/src/a.json")), None);
    }

    #[test]
    fn the_build_directory_is_watched_once_it_exists() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let without = watched_paths(root);
        assert!(without.contains(&(root.join("src"), RecursiveMode::Recursive)));
        assert!(without.contains(&(root.to_path_buf(), RecursiveMode::NonRecursive)));
        assert!(without.contains(&(root.join(".happyview"), RecursiveMode::NonRecursive)));
        assert!(!without.iter().any(|(p, _)| p.ends_with("build")));

        std::fs::create_dir_all(root.join(".happyview/build")).unwrap();
        assert!(
            watched_paths(root)
                .contains(&(root.join(".happyview/build"), RecursiveMode::NonRecursive))
        );
    }

    #[test]
    fn poll_interval_parses_or_is_refused() {
        assert_eq!(parse_poll_ms(None), Ok(WatchMode::Native));
        assert_eq!(
            parse_poll_ms(Some("250")),
            Ok(WatchMode::Poll(Duration::from_millis(250)))
        );
        assert_eq!(
            parse_poll_ms(Some("50")),
            Ok(WatchMode::Poll(Duration::from_millis(50)))
        );
        assert_eq!(
            parse_poll_ms(Some("60000")),
            Ok(WatchMode::Poll(Duration::from_secs(60)))
        );
        for bad in ["", "0", "49", "60001", "1s", "-5", " 250"] {
            let err = parse_poll_ms(Some(bad)).unwrap_err();
            assert!(err.contains("HAPPYVIEW_WATCH_POLL_MS"), "{err}");
        }
    }

    /// The poll watcher is the one built when asked for, and it delivers
    /// a change through the same classify path as native events.
    #[tokio::test]
    async fn the_poll_watcher_is_chosen_and_sees_a_change() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".happyview")).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Change>();
        let watcher = start_watcher(&root, WatchMode::Poll(Duration::from_millis(50)), tx).unwrap();
        assert!(matches!(&*watcher.lock().unwrap(), AnyDebouncer::Poll(_)));

        std::fs::write(root.join("src/a.json"), "{}").unwrap();
        let change = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await;
        assert_eq!(
            change.expect("the poll watcher saw nothing"),
            Some(Change::Source)
        );

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<Change>();
        let native = start_watcher(&root, WatchMode::Native, tx).unwrap();
        assert!(matches!(&*native.lock().unwrap(), AnyDebouncer::Native(_)));
    }

    /// Everything queued while a build ran becomes one more build, which
    /// reloads the config if any of it was a config change.
    #[tokio::test]
    async fn coalesces_queued_signals() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Change>();
        for _ in 0..10 {
            tx.send(Change::Source).unwrap();
        }
        tx.send(Change::Config).unwrap();
        tx.send(Change::SrcDir).unwrap();
        tx.send(Change::BuildDir).unwrap();
        assert_eq!(
            next_batch(&mut rx).await,
            Some(Batch {
                config: true,
                src_dir: true,
                build_dir: true
            })
        );
        assert!(
            rx.try_recv().is_err(),
            "the batch drained everything queued"
        );
        tx.send(Change::Source).unwrap();
        assert_eq!(next_batch(&mut rx).await, Some(Batch::default()));
        drop(tx);
        assert_eq!(next_batch(&mut rx).await, None);
    }

    #[derive(Default)]
    struct Recorded(Vec<(PathBuf, RecursiveMode)>);

    impl Watch for Recorded {
        fn watch(&mut self, path: &Path, mode: RecursiveMode) -> notify::Result<()> {
            self.0.push((path.to_path_buf(), mode));
            Ok(())
        }
    }

    /// A deleted and recreated `src/` or `.happyview/build` is watched again,
    /// so a branch switch that empties `src/` does not end rebuilds for the
    /// rest of the session. A directory still missing is not.
    #[tokio::test]
    async fn a_recreated_directory_is_watched_again() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let state = crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        );
        let session = DevSession {
            root: root.to_path_buf(),
            public_url: String::new(),
            secrets: crate::dev::secrets::load_or_create(&root.join(".happyview")).unwrap(),
            watch_mode: WatchMode::Native,
        };
        let recorded = Arc::new(Mutex::new(Recorded::default()));
        let rewatch = Arc::downgrade(&recorded);
        let mut declared = None;

        let src_and_build = Batch {
            src_dir: true,
            build_dir: true,
            ..Batch::default()
        };
        handle_batch(&state, &session, &rewatch, &mut declared, src_and_build).await;
        assert_eq!(
            recorded.lock().unwrap().0,
            vec![(root.join("src"), RecursiveMode::Recursive)],
            "build/ does not exist, so only src/ is watched again"
        );

        recorded.lock().unwrap().0.clear();
        handle_batch(&state, &session, &rewatch, &mut declared, Batch::default()).await;
        assert!(recorded.lock().unwrap().0.is_empty());
    }
}
