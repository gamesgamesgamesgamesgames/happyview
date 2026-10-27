//! Rebuilding the project snapshot when its inputs change.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use notify::{Event, EventKind, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::AppState;
use crate::dev::{DevSession, report};

/// How long the inputs must stay quiet before a build. Long enough to fold
/// an editor's write-then-rename, or a formatter's second write, into one
/// build; short enough that a save feels immediate.
const DEBOUNCE: Duration = Duration::from_millis(100);

/// The longest a batch may keep growing. Something that writes under `src/`
/// without pause would otherwise never leave the inputs quiet, and no build
/// would ever start.
const MAX_BATCH: Duration = Duration::from_secs(1);

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
    /// `.env.test`, which only a test run reads. The dev server ignores it.
    TestEnv,
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
    if rel == Path::new(".env.test") {
        return Some(Change::TestEnv);
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

/// Wait for a signal, then take every other one until none has arrived for
/// `DEBOUNCE`, or `MAX_BATCH` has passed. `None` when every sender is gone.
pub(crate) async fn debounced<T>(rx: &mut UnboundedReceiver<T>) -> Option<Vec<T>> {
    let mut batch = vec![rx.recv().await?];
    let cutoff = tokio::time::Instant::now() + MAX_BATCH;
    loop {
        let quiet = (tokio::time::Instant::now() + DEBOUNCE).min(cutoff);
        match tokio::time::timeout_at(quiet, rx.recv()).await {
            Ok(Some(signal)) => batch.push(signal),
            _ => break,
        }
    }
    Some(batch)
}

async fn next_batch(rx: &mut UnboundedReceiver<Change>) -> Option<Batch> {
    let mut batch = Batch::default();
    for change in debounced(rx).await? {
        match change {
            Change::Config => batch.config = true,
            Change::SrcDir => batch.src_dir = true,
            Change::BuildDir => batch.build_dir = true,
            Change::Source | Change::TestEnv => {}
        }
    }
    Some(batch)
}

/// One build of the project at `root`, installed if it succeeds, and the
/// line that reports it either way.
pub async fn rebuild(state: &AppState, root: &Path) -> String {
    let started = Instant::now();
    match crate::project::prepare::rebuild(state, root).await {
        Ok(installed) => {
            // Idempotent per collection, and a no-op unless [dev] backfill
            // is on, so a lexicon declared since boot gets its backfill now.
            crate::project::boot::start_declared_backfills(state).await;
            report::built(&installed.changes, &installed.warnings, started.elapsed())
        }
        Err(diagnostics) => report::failed(&diagnostics),
    }
}

/// Editor declarations for the snapshot now serving, under
/// `.happyview/types/`. A failed build left the previous snapshot in place,
/// whose types are already current, so a pass after one writes nothing.
/// Only what changed is reported, except on the first pass, which also says
/// which interpreters offer no types and what the input could not describe.
pub async fn refresh_types(state: &AppState, root: &Path, first: bool) -> Vec<String> {
    use crate::project::declarations::command::{Outcome, default_out, generate};
    let generated = generate(
        &state.plugin_registry,
        &state.plugin_executor(),
        &state.project.load_full(),
        &default_out(root),
    )
    .await;
    let mut lines = Vec::new();
    if first || generated.wrote() || generated.failed() {
        lines.extend(generated.notes.iter().map(|note| format!("types: {note}")));
    }
    for (language_id, outcome) in &generated.languages {
        let shown = match outcome {
            Outcome::Wrote(_) | Outcome::Failed(_) => true,
            Outcome::Skipped(_) => first,
            Outcome::UpToDate => false,
        };
        if shown {
            lines.push(format!("types: {language_id}: {outcome}"));
        }
    }
    lines
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

/// The two watchers are different types, and the loop only ever needs to
/// add a watch.
pub(crate) enum AnyWatcher {
    Native(RecommendedWatcher),
    Poll(PollWatcher),
}

/// Adding a watch is all the rebuild loop does with its watcher, so a test
/// can stand in for one.
pub(crate) trait Watch {
    fn watch(&mut self, path: &Path, mode: RecursiveMode) -> notify::Result<()>;
}

impl Watch for AnyWatcher {
    fn watch(&mut self, path: &Path, mode: RecursiveMode) -> notify::Result<()> {
        match self {
            Self::Native(w) => Watcher::watch(w, path, mode),
            Self::Poll(w) => Watcher::watch(w, path, mode),
        }
    }
}

pub(crate) type SharedWatcher = Arc<Mutex<AnyWatcher>>;

/// The watcher's event handler: each classified change, with the path it
/// came from, to `on_change`, except for reads. inotify reports every open
/// and close, and a build opens every directory under `src/`, so counting a
/// read as a change would schedule the next build from the last one,
/// forever. The kind is filtered here, before anything is collapsed, because
/// it is gone once events become a `Change`.
fn forward(
    root: PathBuf,
    on_change: impl Fn(Change, &Path) + Send + 'static,
) -> impl FnMut(notify::Result<Event>) + Send + 'static {
    move |result| match result {
        Ok(event) if matches!(event.kind, EventKind::Access(_)) => {}
        Ok(event) => {
            for path in &event.paths {
                if let Some(change) = classify(&root, path) {
                    on_change(change, path);
                }
            }
        }
        Err(e) => eprintln!("file watcher: {e}"),
    }
}

/// A watcher of the chosen kind, watching every path in `watched_paths`,
/// that hands each classified change, with its path, to `on_change`.
pub(crate) fn start_watcher(
    root: &Path,
    mode: WatchMode,
    on_change: impl Fn(Change, &Path) + Send + 'static,
) -> Result<SharedWatcher, String> {
    let handler = forward(root.to_path_buf(), on_change);
    let watcher = match mode {
        WatchMode::Native => AnyWatcher::Native(
            RecommendedWatcher::new(handler, notify::Config::default())
                .map_err(|e| format!("could not start the file watcher: {e}"))?,
        ),
        WatchMode::Poll(interval) => AnyWatcher::Poll(
            PollWatcher::new(
                handler,
                notify::Config::default().with_poll_interval(interval),
            )
            .map_err(|e| format!("could not start the polling file watcher: {e}"))?,
        ),
    };
    let watcher = Arc::new(Mutex::new(watcher));
    for (path, recursive) in watched_paths(root) {
        watcher
            .lock()
            .unwrap()
            .watch(&path, recursive)
            .map_err(|e| format!("could not watch {}: {e}", path.display()))?;
    }
    Ok(watcher)
}

/// The only strong reference to the watcher: dropping it stops the watch,
/// which drops the channel's sender and ends the rebuild loop.
pub struct WatchHandle {
    _watcher: SharedWatcher,
    mode: WatchMode,
}

impl WatchHandle {
    pub fn mode(&self) -> WatchMode {
        self.mode
    }
}

pub fn spawn(state: AppState, session: DevSession) -> Result<WatchHandle, String> {
    spawn_with(state, session, |line| println!("{line}"))
}

/// `spawn`, with each build's line handed to `out` rather than printed.
pub fn spawn_with(
    state: AppState,
    session: DevSession,
    out: impl Fn(String) + Send + 'static,
) -> Result<WatchHandle, String> {
    let root = session.root.clone();
    let mode = session.watch_mode;
    // A project scaffolded without src/ yet would otherwise have nothing to
    // watch until restarted. `.happyview/` already exists: it holds the
    // secrets and the database.
    std::fs::create_dir_all(root.join("src"))
        .map_err(|e| format!("could not create {}: {e}", root.join("src").display()))?;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Change>();
    spawn_interpreter_listener(&state, &tx);
    let watcher = start_watcher(&root, mode, move |change, _| {
        if serves(change) {
            let _ = tx.send(change);
        }
    })?;

    let rewatch: Weak<Mutex<AnyWatcher>> = Arc::downgrade(&watcher);
    let mut declared = state.config.project.clone();
    tokio::spawn(async move {
        // The boot build is already installed; its types come first, in the
        // same task as every later pass so two never write at once.
        for line in refresh_types(&state, &root, true).await {
            out(line);
        }
        while let Some(batch) = next_batch(&mut rx).await {
            let line = handle_batch(&state, &session, &rewatch, &mut declared, batch).await;
            out(line);
            for line in refresh_types(&state, &root, false).await {
                out(line);
            }
        }
    });
    Ok(WatchHandle {
        _watcher: watcher,
        mode,
    })
}

/// Whether a change is one the dev server rebuilds for.
fn serves(change: Change) -> bool {
    change != Change::TestEnv
}

/// An interpreter installed or removed changes which files are scripts and
/// what their prepared artifacts are, so it is a project input like a save.
/// The sender is held weakly, so dropping the watch still ends the loop.
fn spawn_interpreter_listener(state: &AppState, tx: &UnboundedSender<Change>) {
    let mut changes = state.plugin_registry.interpreter_changes();
    changes.mark_unchanged();
    let tx = tx.downgrade();
    tokio::spawn(async move {
        while changes.changed().await.is_ok() {
            let Some(tx) = tx.upgrade() else { break };
            if tx.send(Change::Source).is_err() {
                break;
            }
        }
    });
}

/// Watch again whichever of `src/` and `.happyview/build` was recreated.
pub(crate) fn watch_again<W: Watch>(
    rewatch: &Weak<Mutex<W>>,
    root: &Path,
    src_dir: bool,
    build_dir_recreated: bool,
) {
    let recreated = [
        (src_dir, root.join("src"), RecursiveMode::Recursive),
        (
            build_dir_recreated,
            build_dir(root),
            RecursiveMode::NonRecursive,
        ),
    ];
    for (changed, path, recursive) in recreated {
        if changed
            && path.is_dir()
            && let Some(watcher) = rewatch.upgrade()
        {
            // Watching a path twice is harmless; a failure means the
            // directory went away again, and its reappearance retries.
            let _ = watcher.lock().unwrap().watch(&path, recursive);
        }
    }
}

/// One batch's work: watch again any directory that was recreated, reload
/// the config if it changed, then build. Answers the build's line.
async fn handle_batch<W: Watch>(
    state: &AppState,
    session: &DevSession,
    rewatch: &Weak<Mutex<W>>,
    declared: &mut Option<Arc<crate::config::project::ProjectConfig>>,
    batch: Batch,
) -> String {
    watch_again(rewatch, &session.root, batch.src_dir, batch.build_dir);
    if batch.config
        && let Err(e) = crate::dev::reload::reload(state, session, declared).await
    {
        // The snapshot build below reads the same file and reports the same
        // error with the standing line.
        tracing::debug!("happyview.toml did not load: {e}");
    }
    rebuild(state, &session.root).await
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
        assert_eq!(
            classify(root, Path::new("/p/.env.test")),
            Some(Change::TestEnv)
        );
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
        let watcher = start_watcher(
            &root,
            WatchMode::Poll(Duration::from_millis(50)),
            move |change, _| {
                let _ = tx.send(change);
            },
        )
        .unwrap();
        assert!(matches!(&*watcher.lock().unwrap(), AnyWatcher::Poll(_)));

        std::fs::write(root.join("src/a.json"), "{}").unwrap();
        let change = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await;
        assert_eq!(
            change.expect("the poll watcher saw nothing"),
            Some(Change::Source)
        );

        let native = start_watcher(&root, WatchMode::Native, |_, _| {}).unwrap();
        assert!(matches!(&*native.lock().unwrap(), AnyWatcher::Native(_)));
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

    fn event(kind: EventKind, path: &str) -> notify::Result<Event> {
        Ok(Event::new(kind).add_path(PathBuf::from(path)))
    }

    /// What inotify reports for a build reading `src/`: opens and closes,
    /// which a macOS host never produces. None of them is a build.
    #[tokio::test]
    async fn reads_are_not_changes() {
        use notify::event::{AccessKind, AccessMode};
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Change>();
        let mut handler = forward(PathBuf::from("/p"), move |change, _| {
            let _ = tx.send(change);
        });
        for path in ["/p/src", "/p/src/lexicons", "/p/src/lexicons/a.json"] {
            handler(event(
                EventKind::Access(AccessKind::Open(AccessMode::Any)),
                path,
            ));
            handler(event(
                EventKind::Access(AccessKind::Close(AccessMode::Read)),
                path,
            ));
            handler(event(EventKind::Access(AccessKind::Read), path));
        }
        let built = tokio::time::timeout(DEBOUNCE * 3, next_batch(&mut rx)).await;
        assert!(built.is_err(), "a read was taken for a change: {built:?}");
    }

    /// A modify, a create, a remove and a rename, mixed with reads, are one
    /// build.
    #[tokio::test]
    async fn changes_are_one_build() {
        use notify::event::{
            AccessKind, AccessMode, CreateKind, DataChange, ModifyKind, RemoveKind, RenameMode,
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Change>();
        let mut handler = forward(PathBuf::from("/p"), move |change, _| {
            let _ = tx.send(change);
        });
        let a = "/p/src/lexicons/a.json";
        handler(event(
            EventKind::Access(AccessKind::Open(AccessMode::Any)),
            a,
        ));
        handler(event(
            EventKind::Modify(ModifyKind::Data(DataChange::Any)),
            a,
        ));
        handler(event(EventKind::Create(CreateKind::File), "/p/src/b.lua"));
        handler(event(EventKind::Remove(RemoveKind::File), "/p/src/c.lua"));
        handler(event(
            EventKind::Modify(ModifyKind::Name(RenameMode::To)),
            "/p/happyview.toml",
        ));
        handler(event(
            EventKind::Access(AccessKind::Close(AccessMode::Read)),
            a,
        ));

        assert_eq!(
            next_batch(&mut rx).await,
            Some(Batch {
                config: true,
                ..Batch::default()
            })
        );
        let again = tokio::time::timeout(DEBOUNCE * 3, next_batch(&mut rx)).await;
        assert!(again.is_err(), "a second build: {again:?}");
    }

    /// `.env.test` is a test run's input, not the dev server's.
    #[test]
    fn the_dev_server_ignores_the_test_env() {
        assert!(!serves(Change::TestEnv));
        for change in [
            Change::Source,
            Change::Config,
            Change::SrcDir,
            Change::BuildDir,
        ] {
            assert!(serves(change), "{change:?}");
        }
    }

    /// The path rides along with its change, so a caller can tell which
    /// file was saved.
    #[test]
    fn each_change_carries_its_path() {
        use notify::event::{DataChange, ModifyKind};
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        let mut handler = forward(PathBuf::from("/p"), move |change, path| {
            record.lock().unwrap().push((change, path.to_path_buf()));
        });
        handler(Ok(Event::new(EventKind::Modify(ModifyKind::Data(
            DataChange::Any,
        )))
        .add_path(PathBuf::from("/p/src/a.test.lua"))
        .add_path(PathBuf::from("/p/node_modules/x.js"))
        .add_path(PathBuf::from("/p/happyview.toml"))));
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                (Change::Source, PathBuf::from("/p/src/a.test.lua")),
                (Change::Config, PathBuf::from("/p/happyview.toml")),
            ]
        );
    }

    #[tokio::test]
    async fn unending_changes_still_build() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Change>();
        let writer = tokio::spawn(async move {
            while tx.send(Change::Source).is_ok() {
                tokio::time::sleep(DEBOUNCE / 4).await;
            }
        });
        let built = tokio::time::timeout(MAX_BATCH * 2, next_batch(&mut rx)).await;
        writer.abort();
        assert!(built.is_ok(), "no build while changes kept arriving");
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
