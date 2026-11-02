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
    /// Something in `PLUGIN_DIR`, or the directory itself, which hpy
    /// places plugins in. Only `happyview dev` watches it.
    Plugins,
    /// `happyview.lock`, whose plugins load only at boot.
    Lock,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Batch {
    config: bool,
    src_dir: bool,
    build_dir: bool,
    plugins: bool,
    lock: bool,
}

fn build_dir(root: &Path) -> PathBuf {
    root.join(".happyview").join("build")
}

/// Each input is watched on its own rather than the whole root
/// recursively, which would also cover `node_modules/`, `target/` and the
/// database that every request writes to. The root and `.happyview/` are
/// shallow watches: the first sees `happyview.toml` replaced by rename, the
/// second sees `build/` appear or be recreated.
///
/// `plugins` is `PLUGIN_DIR`, watched shallowly once it exists: hpy adds
/// and removes its entries whole, as symlinks or directories.
pub fn watched_paths(root: &Path, plugins: Option<&Path>) -> Vec<(PathBuf, RecursiveMode)> {
    let mut paths = vec![
        (root.join("src"), RecursiveMode::Recursive),
        (root.to_path_buf(), RecursiveMode::NonRecursive),
        (root.join(".happyview"), RecursiveMode::NonRecursive),
    ];
    if build_dir(root).is_dir() {
        paths.push((build_dir(root), RecursiveMode::NonRecursive));
    }
    if let Some(dir) = plugins.filter(|dir| dir.is_dir()) {
        paths.push((dir.to_path_buf(), RecursiveMode::NonRecursive));
    }
    paths
}

/// `plugins` is the watched `PLUGIN_DIR`, when there is one. Anything under
/// it counts, not only its entries, so a native watcher that reports the
/// files of a copied entry still loads it.
pub fn classify(root: &Path, plugins: Option<&Path>, path: &Path) -> Option<Change> {
    if plugins.is_some_and(|dir| path.starts_with(dir)) {
        return Some(Change::Plugins);
    }
    let rel = path.strip_prefix(root).ok()?;
    if rel == Path::new("happyview.toml") {
        return Some(Change::Config);
    }
    if rel == Path::new(".env.test") {
        return Some(Change::TestEnv);
    }
    if rel == Path::new("happyview.lock") {
        return Some(Change::Lock);
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
            Change::Plugins => batch.plugins = true,
            Change::Lock => batch.lock = true,
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
    plugins: Option<PathBuf>,
    on_change: impl Fn(Change, &Path) + Send + 'static,
) -> impl FnMut(notify::Result<Event>) + Send + 'static {
    move |result| match result {
        Ok(event) if matches!(event.kind, EventKind::Access(_)) => {}
        Ok(event) => {
            for path in &event.paths {
                if let Some(change) = classify(&root, plugins.as_deref(), path) {
                    on_change(change, path);
                }
            }
        }
        Err(e) => eprintln!("file watcher: {e}"),
    }
}

/// A watcher of the chosen kind, watching every path in `watched_paths`,
/// that hands each classified change, with its path, to `on_change`.
/// `plugins` is `PLUGIN_DIR` for `happyview dev`, and `None` otherwise.
pub(crate) fn start_watcher(
    root: &Path,
    plugins: Option<&Path>,
    mode: WatchMode,
    on_change: impl Fn(Change, &Path) + Send + 'static,
) -> Result<SharedWatcher, String> {
    let handler = forward(
        root.to_path_buf(),
        plugins.map(Path::to_path_buf),
        on_change,
    );
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
    for (path, recursive) in watched_paths(root, plugins) {
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

    let plugins = watched_plugin_dir(&state.config.plugin_dir);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Change>();
    spawn_interpreter_listener(&state, &tx);
    let watcher = start_watcher(&root, Some(&plugins), mode, move |change, _| {
        if serves(change) {
            let _ = tx.send(change);
        }
    })?;

    let rewatch: Weak<Mutex<AnyWatcher>> = Arc::downgrade(&watcher);
    let mut declared = state.config.project.clone();
    // What hpy placed the plugins from, to tell a real change from a rewrite.
    let lock_at_boot = std::fs::read(root.join("happyview.lock")).ok();
    tokio::spawn(async move {
        // The boot build is already installed; its types come first, in the
        // same task as every later pass so two never write at once.
        for line in refresh_types(&state, &root, true).await {
            out(line);
        }
        while let Some(batch) = next_batch(&mut rx).await {
            let lines = handle_batch(
                &state,
                &session,
                &rewatch,
                &plugins,
                &mut declared,
                lock_at_boot.as_deref(),
                batch,
            )
            .await;
            for line in lines {
                out(line);
            }
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

/// `PLUGIN_DIR` as the watcher reports it: absolute and canonical, so its
/// events can be told apart. It may not exist yet, since hpy creates it when
/// it first places a plugin, so only the part that does is resolved.
fn watched_plugin_dir(dir: &Path) -> PathBuf {
    let absolute = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    if let Ok(canonical) = std::fs::canonicalize(&absolute) {
        return canonical;
    }
    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent)
            .map(|parent| parent.join(name))
            .unwrap_or(absolute),
        _ => absolute,
    }
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
        if changed {
            watch_dir_again(rewatch, &path, recursive);
        }
    }
}

/// Watch `path` again if it is a directory now.
fn watch_dir_again<W: Watch>(rewatch: &Weak<Mutex<W>>, path: &Path, recursive: RecursiveMode) {
    if path.is_dir()
        && let Some(watcher) = rewatch.upgrade()
    {
        // Watching a path twice is harmless; a failure means the
        // directory went away again, and its reappearance retries.
        let _ = watcher.lock().unwrap().watch(path, recursive);
    }
}

/// One batch's work: watch again any directory that was recreated, load
/// and unload plugins if `PLUGIN_DIR` changed, reload the config if it
/// changed, say so if the lock changed, then build. Answers the plugins'
/// line, if they changed, the lock's, if it needs a restart, and the
/// build's.
///
/// `plugins` is the canonical `PLUGIN_DIR` the watcher uses; the registry is
/// synced against `state.config.plugin_dir` as given, since that is the
/// path boot loaded every file plugin's source from.
async fn handle_batch<W: Watch>(
    state: &AppState,
    session: &DevSession,
    rewatch: &Weak<Mutex<W>>,
    plugins: &Path,
    declared: &mut Option<Arc<crate::config::project::ProjectConfig>>,
    lock_at_boot: Option<&[u8]>,
    batch: Batch,
) -> Vec<String> {
    watch_again(rewatch, &session.root, batch.src_dir, batch.build_dir);
    let mut lines = Vec::new();
    if batch.plugins {
        // The directory may have been created or replaced since it was
        // last watched.
        watch_dir_again(rewatch, plugins, RecursiveMode::NonRecursive);
        lines.extend(
            crate::dev::plugins::sync(&state.plugin_registry, &state.config.plugin_dir).await,
        );
    }
    if batch.lock {
        let now = std::fs::read(session.root.join("happyview.lock")).ok();
        lines.extend(crate::dev::reload::lock_notice(
            lock_at_boot,
            now.as_deref(),
        ));
    }
    if batch.config
        && let Err(e) = crate::dev::reload::reload(state, session, declared).await
    {
        // The snapshot build below reads the same file and reports the same
        // error with the standing line.
        tracing::debug!("happyview.toml did not load: {e}");
    }
    lines.push(rebuild(state, &session.root).await);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn project_inputs_are_all_that_count() {
        let root = Path::new("/p");
        assert_eq!(
            classify(root, None, Path::new("/p/src/lexicons/a.json")),
            Some(Change::Source)
        );
        assert_eq!(
            classify(root, None, Path::new("/p/src")),
            Some(Change::SrcDir)
        );
        assert_eq!(
            classify(root, None, Path::new("/p/happyview.toml")),
            Some(Change::Config)
        );
        assert_eq!(
            classify(root, None, Path::new("/p/.happyview/build/registry.wasm")),
            Some(Change::Source)
        );
        assert_eq!(
            classify(root, None, Path::new("/p/.happyview/build")),
            Some(Change::BuildDir)
        );
        assert_eq!(
            classify(
                root,
                None,
                Path::new("/p/.happyview/build/registry.wasm.tmp")
            ),
            None
        );
        assert_eq!(
            classify(root, None, Path::new("/p/.happyview/build/nested/x.wasm")),
            None
        );
        assert_eq!(
            classify(root, None, Path::new("/p/.happyview/happyview.db-wal")),
            None
        );
        assert_eq!(
            classify(
                root,
                None,
                Path::new("/p/.happyview/plugins/lua/manifest.json")
            ),
            None
        );
        assert_eq!(classify(root, None, Path::new("/p/.env")), None);
        assert_eq!(
            classify(root, None, Path::new("/p/.env.test")),
            Some(Change::TestEnv)
        );
        assert_eq!(
            classify(root, None, Path::new("/p/happyview.lock")),
            Some(Change::Lock)
        );
        assert_eq!(classify(root, None, Path::new("/p/srcery/x.lua")), None);
        assert_eq!(
            classify(root, None, Path::new("/elsewhere/src/a.json")),
            None
        );
    }

    /// Under `happyview dev`, `PLUGIN_DIR`, its entries and anything in
    /// them are plugin changes, wherever the directory is.
    #[test]
    fn the_plugin_directory_is_its_own_input() {
        let root = Path::new("/p");
        for plugins in [
            Path::new("/p/.happyview/plugins"),
            Path::new("/elsewhere/plugins"),
        ] {
            for path in [
                plugins.to_path_buf(),
                plugins.join("happyview-db"),
                plugins.join("happyview-db/manifest.json"),
            ] {
                assert_eq!(
                    classify(root, Some(plugins), &path),
                    Some(Change::Plugins),
                    "{}",
                    path.display()
                );
            }
        }
        let plugins = Some(Path::new("/p/.happyview/plugins"));
        assert_eq!(
            classify(root, plugins, Path::new("/p/.happyview/pluginsx")),
            None
        );
        assert_eq!(
            classify(root, plugins, Path::new("/p/.happyview/build")),
            Some(Change::BuildDir)
        );
        assert_eq!(
            classify(root, plugins, Path::new("/p/src/a.json")),
            Some(Change::Source)
        );
    }

    #[test]
    fn the_plugin_directory_is_watched_once_it_exists() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let plugins = root.join(".happyview/plugins");
        assert!(
            !watched_paths(root, Some(&plugins))
                .iter()
                .any(|(p, _)| p == &plugins)
        );
        std::fs::create_dir_all(&plugins).unwrap();
        assert!(
            watched_paths(root, Some(&plugins))
                .contains(&(plugins.clone(), RecursiveMode::NonRecursive))
        );
        assert!(!watched_paths(root, None).iter().any(|(p, _)| p == &plugins));
    }

    /// A directory hpy has yet to create resolves through its parent, so
    /// its events match once it exists.
    #[test]
    fn a_missing_plugin_directory_resolves_through_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(
            watched_plugin_dir(&dir.path().join("plugins")),
            canonical.join("plugins")
        );
        std::fs::create_dir(dir.path().join("plugins")).unwrap();
        assert_eq!(
            watched_plugin_dir(&dir.path().join("plugins")),
            canonical.join("plugins")
        );
    }

    #[test]
    fn the_build_directory_is_watched_once_it_exists() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let without = watched_paths(root, None);
        assert!(without.contains(&(root.join("src"), RecursiveMode::Recursive)));
        assert!(without.contains(&(root.to_path_buf(), RecursiveMode::NonRecursive)));
        assert!(without.contains(&(root.join(".happyview"), RecursiveMode::NonRecursive)));
        assert!(!without.iter().any(|(p, _)| p.ends_with("build")));

        std::fs::create_dir_all(root.join(".happyview/build")).unwrap();
        assert!(
            watched_paths(root, None)
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
            None,
            WatchMode::Poll(Duration::from_millis(50)),
            move |change, _| {
                let _ = tx.send(change);
            },
        )
        .unwrap();
        assert!(matches!(&*watcher.lock().unwrap(), AnyWatcher::Poll(_)));

        // Writing the file also changes `src/`'s own mtime, which the poller
        // may report first as `SrcDir`; the scan order decides.
        std::fs::write(root.join("src/a.json"), "{}").unwrap();
        let change = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Some(Change::SrcDir) => continue,
                    other => return other,
                }
            }
        })
        .await;
        assert_eq!(
            change.expect("the poll watcher saw nothing"),
            Some(Change::Source)
        );

        let native = start_watcher(&root, None, WatchMode::Native, |_, _| {}).unwrap();
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
        tx.send(Change::Plugins).unwrap();
        tx.send(Change::Plugins).unwrap();
        tx.send(Change::Lock).unwrap();
        assert_eq!(
            next_batch(&mut rx).await,
            Some(Batch {
                config: true,
                src_dir: true,
                build_dir: true,
                plugins: true,
                lock: true,
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
        let mut handler = forward(PathBuf::from("/p"), None, move |change, _| {
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
        let mut handler = forward(PathBuf::from("/p"), None, move |change, _| {
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

    /// hpy removing one entry and placing another, then replacing the
    /// directory, is one plugin sync.
    #[tokio::test]
    async fn plugin_entries_are_one_batch() {
        use notify::event::{CreateKind, RemoveKind};
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Change>();
        let mut handler = forward(
            PathBuf::from("/p"),
            Some(PathBuf::from("/p/.happyview/plugins")),
            move |change, _| {
                let _ = tx.send(change);
            },
        );
        handler(event(
            EventKind::Remove(RemoveKind::Any),
            "/p/.happyview/plugins/happyview-sql",
        ));
        handler(event(
            EventKind::Create(CreateKind::Any),
            "/p/.happyview/plugins/happyview-db",
        ));
        handler(event(
            EventKind::Create(CreateKind::Folder),
            "/p/.happyview/plugins",
        ));
        assert_eq!(
            next_batch(&mut rx).await,
            Some(Batch {
                plugins: true,
                ..Batch::default()
            })
        );
        let again = tokio::time::timeout(DEBOUNCE * 3, next_batch(&mut rx)).await;
        assert!(again.is_err(), "a second batch: {again:?}");
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
            Change::Plugins,
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
        let mut handler = forward(PathBuf::from("/p"), None, move |change, path| {
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
        let plugins = root.join(".happyview/plugins");

        let src_and_build = Batch {
            src_dir: true,
            build_dir: true,
            ..Batch::default()
        };
        handle_batch(
            &state,
            &session,
            &rewatch,
            &plugins,
            &mut declared,
            None,
            src_and_build,
        )
        .await;
        assert_eq!(
            recorded.lock().unwrap().0,
            vec![(root.join("src"), RecursiveMode::Recursive)],
            "build/ does not exist, so only src/ is watched again"
        );

        recorded.lock().unwrap().0.clear();
        handle_batch(
            &state,
            &session,
            &rewatch,
            &plugins,
            &mut declared,
            None,
            Batch::default(),
        )
        .await;
        assert!(recorded.lock().unwrap().0.is_empty());
    }
    /// A batch with plugin changes watches `PLUGIN_DIR` again, loads what
    /// it holds, and says so before the build's line.
    #[tokio::test]
    async fn a_plugin_batch_loads_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let plugins = root.join(".happyview/plugins");
        let entry = plugins.join("happyview-db");
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(
            entry.join("manifest.json"),
            serde_json::json!({
                "id": "happyview-db", "name": "db", "version": "1.0.0-alpha.7",
                "api_version": "2", "plugin_type": "library", "capabilities": [],
                "wasm_file": "plugin.wasm",
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            entry.join("plugin.wasm"),
            wat::parse_str(r#"(module (memory (export "memory") 1))"#).unwrap(),
        )
        .unwrap();

        let mut config = crate::test_support::test_config(crate::db::DatabaseBackend::Sqlite);
        config.plugin_dir = plugins.clone();
        let state = crate::state::state_with_pool_on(
            config,
            crate::test_support::migrated_memory_pool().await,
            crate::db::DatabaseBackend::Sqlite,
        );
        let session = DevSession {
            root: root.clone(),
            public_url: String::new(),
            secrets: crate::dev::secrets::load_or_create(&root.join(".happyview")).unwrap(),
            watch_mode: WatchMode::Native,
        };
        let recorded = Arc::new(Mutex::new(Recorded::default()));
        let rewatch = Arc::downgrade(&recorded);
        let mut declared = None;

        let batch = Batch {
            plugins: true,
            ..Batch::default()
        };
        let lines = handle_batch(
            &state,
            &session,
            &rewatch,
            &plugins,
            &mut declared,
            None,
            batch,
        )
        .await;
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[0], "plugins: loaded happyview-db 1.0.0-alpha.7");
        assert_eq!(
            recorded.lock().unwrap().0,
            vec![(plugins.clone(), RecursiveMode::NonRecursive)]
        );
        assert!(state.plugin_registry.get("happyview-db").await.is_some());

        std::fs::remove_dir_all(&entry).unwrap();
        let batch = Batch {
            plugins: true,
            ..Batch::default()
        };
        let lines = handle_batch(
            &state,
            &session,
            &rewatch,
            &plugins,
            &mut declared,
            None,
            batch,
        )
        .await;
        assert_eq!(lines[0], "plugins: removed happyview-db");
        assert!(state.plugin_registry.get("happyview-db").await.is_none());
    }
}
