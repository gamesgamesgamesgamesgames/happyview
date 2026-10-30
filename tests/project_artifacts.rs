//! The snapshot build prepares every project file through its interpreter,
//! refuses a literal import that names nothing, and rebuilds when an
//! interpreter changes. The echo fixture claims `.echo`.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::app::TestApp;
use common::echo_interpreter;
use happyview::project::build::{BuildEnv, BuildOptions, build_snapshot, install};
use happyview::project::diagnostic::{Diagnostic, has_errors};
use happyview::project::prepare::{
    check_imports, prepare_files, spawn_rebuild_on_interpreter_change,
};
use happyview::project::snapshot::{ProjectModule, ProjectScript, ProjectSnapshot, ScriptBody};

async fn app() -> Option<TestApp> {
    if std::env::var("TEST_DATABASE_URL").is_err() {
        eprintln!("skipped (TEST_DATABASE_URL not set)");
        return None;
    }
    if !echo_interpreter::is_built() {
        eprintln!("skipping: {}", echo_interpreter::BUILD);
        return None;
    }
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .register(echo_interpreter::plugin_with_extensions("echo", &["echo"]))
        .await;
    Some(app)
}

fn script(path: &str, source: &str) -> ProjectScript {
    ProjectScript {
        trigger: format!(
            "job.run:{}",
            Path::new(path).file_stem().unwrap().to_string_lossy()
        ),
        path: path.into(),
        language_id: "echo".into(),
        body: ScriptBody::Source {
            source: Arc::from(source),
            artifact: None,
        },
        description: None,
        outbound_xrpcs: Vec::new(),
    }
}

fn module(path: &str, source: &str) -> ProjectModule {
    ProjectModule {
        id: path.into(),
        path: path.into(),
        language_id: "echo".into(),
        source: Arc::from(source),
        artifact: None,
    }
}

fn snapshot(scripts: Vec<ProjectScript>, modules: Vec<ProjectModule>) -> ProjectSnapshot {
    let mut snap = ProjectSnapshot::empty();
    for s in scripts {
        snap.scripts.insert(s.trigger.clone(), s);
    }
    for m in modules {
        snap.modules.insert(m.id.clone(), m);
    }
    snap
}

/// Prepare every file in `snap` and check its literal imports, in the order
/// and on the condition `build_snapshot_with` does.
async fn prepare_snapshot(app: &TestApp, snap: &mut ProjectSnapshot) -> Vec<Diagnostic> {
    let env = BuildEnv::from_state(&app.state);
    let (mut diagnostics, found) = prepare_files(&env, snap).await;
    if !has_errors(&diagnostics) {
        diagnostics.extend(check_imports(&env, snap, &found).await);
    }
    diagnostics
}

#[tokio::test]
async fn every_handler_and_module_gets_a_stamped_artifact() {
    let Some(app) = app().await else { return };
    let mut snap = snapshot(
        vec![script(
            "src/jobs/tick.echo",
            "import ../lib/format\nvalue:none",
        )],
        // A module needs no handle: `no-handle` would refuse a handler.
        vec![module("src/lib/format.echo", "no-handle, and a module")],
    );
    let diagnostics = prepare_snapshot(&app, &mut snap).await;
    assert!(diagnostics.is_empty(), "{diagnostics:?}");

    let stamp = app
        .state
        .plugin_registry
        .artifact_stamp("interpreter_echo")
        .unwrap();
    let ScriptBody::Source {
        artifact: Some(handler),
        ..
    } = &snap.scripts["job.run:tick"].body
    else {
        panic!("the handler was not prepared");
    };
    assert!(stamp.matches(handler));
    assert_eq!(handler.bytes, b"import ../lib/format\nvalue:none");
    let module = snap.modules["src/lib/format.echo"]
        .artifact
        .as_ref()
        .expect("module prepared");
    assert!(stamp.matches(module));
}

#[tokio::test]
async fn a_literal_import_of_nothing_is_reported_at_its_line() {
    let Some(app) = app().await else { return };
    let mut snap = snapshot(
        vec![script(
            "src/jobs/tick.echo",
            "first\nsecond\nimport ./missing",
        )],
        vec![],
    );
    let diagnostics = prepare_snapshot(&app, &mut snap).await;
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0].path, PathBuf::from("src/jobs/tick.echo"));
    assert_eq!(diagnostics[0].line, Some(3));
    assert!(
        diagnostics[0].message.contains("./missing"),
        "{}",
        diagnostics[0].message
    );
}

/// A bare name is a library: the build has no business resolving it.
#[tokio::test]
async fn a_bare_import_is_not_checked() {
    let Some(app) = app().await else { return };
    let mut snap = snapshot(
        vec![script("src/jobs/tick.echo", "import happyview.record")],
        vec![],
    );
    assert!(prepare_snapshot(&app, &mut snap).await.is_empty());
}

/// A module in another language is not one this file can import, even at
/// the path the specifier names.
#[tokio::test]
async fn an_import_of_another_language_s_module_is_refused() {
    let Some(app) = app().await else { return };
    let mut as_lua = echo_interpreter::plugin_with_extensions("lua", &["lua"]);
    as_lua.info.id = "echo-as-lua".into();
    as_lua.manifest.as_mut().unwrap().id = "echo-as-lua".into();
    app.state.plugin_registry.register(as_lua).await;

    let mut format = module("src/lib/format.lua", "local M = {}");
    format.language_id = "lua".into();
    let mut snap = snapshot(
        vec![script("src/jobs/tick.echo", "import ../lib/format")],
        vec![format],
    );
    let diagnostics = prepare_snapshot(&app, &mut snap).await;
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0].path, PathBuf::from("src/jobs/tick.echo"));
    assert_eq!(diagnostics[0].line, Some(1));
    assert!(
        diagnostics[0].message.contains("../lib/format"),
        "{}",
        diagnostics[0].message
    );
}

/// A module that fails to validate is reported for what it is, not also as
/// missing from the file that imports it.
#[tokio::test]
async fn an_invalid_module_is_not_also_reported_as_missing() {
    let Some(app) = app().await else { return };
    let mut snap = snapshot(
        vec![script("src/jobs/tick.echo", "import ../lib/format")],
        vec![module("src/lib/format.echo", "invalid")],
    );
    let diagnostics = prepare_snapshot(&app, &mut snap).await;
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0].path, PathBuf::from("src/lib/format.echo"));
}

#[tokio::test]
async fn an_invalid_file_is_reported_with_its_path_and_line() {
    let Some(app) = app().await else { return };
    let mut snap = snapshot(vec![script("src/jobs/tick.echo", "invalid syntax")], vec![]);
    let diagnostics = prepare_snapshot(&app, &mut snap).await;
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0].path, PathBuf::from("src/jobs/tick.echo"));
    assert_eq!(diagnostics[0].line, Some(1));
}

/// An interpreter that predates roles demands `handle` of a module too. The
/// module still builds, from source, while a handler without one is still
/// refused.
#[tokio::test]
async fn an_interpreter_that_predates_roles_still_builds_a_module() {
    let Some(app) = app().await else { return };
    let mut snap = snapshot(
        vec![script("src/jobs/tick.echo", "value:none")],
        vec![module("src/lib/format.echo", "old-sdk module")],
    );
    let diagnostics = prepare_snapshot(&app, &mut snap).await;
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert!(snap.modules["src/lib/format.echo"].artifact.is_none());

    let mut snap = snapshot(
        vec![script("src/jobs/tick.echo", "old-sdk handler")],
        vec![],
    );
    let diagnostics = prepare_snapshot(&app, &mut snap).await;
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert!(
        diagnostics[0].message.contains("handle"),
        "{}",
        diagnostics[0].message
    );
}

fn project_dir(files: &[(&str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("hv-artifacts-{}", uuid::Uuid::new_v4()));
    for (path, body) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    root
}

/// The call site in `build_snapshot_with`: a real build of a real tree comes
/// out prepared, modules included, and a broken import fails the build.
#[tokio::test]
async fn build_snapshot_prepares_what_it_builds_and_refuses_a_broken_import() {
    let Some(app) = app().await else { return };
    let root = project_dir(&[
        ("src/jobs/tick.echo", "import ../lib/format\nvalue:none"),
        ("src/lib/format.echo", "no-handle, a module"),
    ]);
    let report = build_snapshot(&app.state, &root, BuildOptions::default())
        .await
        .unwrap_or_else(|d| panic!("{d:?}"));
    let ScriptBody::Source { artifact, .. } = &report.snapshot.scripts["job.run:tick"].body else {
        panic!("the handler is a source file");
    };
    assert!(artifact.is_some());
    assert!(
        report.snapshot.modules["src/lib/format.echo"]
            .artifact
            .is_some()
    );

    std::fs::write(
        root.join("src/jobs/tick.echo"),
        "import ../lib/nothing\nvalue:none",
    )
    .unwrap();
    let errors = build_snapshot(&app.state, &root, BuildOptions::default())
        .await
        .err()
        .expect("a missing module fails the build");
    assert!(
        errors
            .iter()
            .any(|d| d.path == Path::new("src/jobs/tick.echo") && d.line == Some(1)),
        "{errors:?}"
    );
    std::fs::remove_dir_all(root).ok();
}

/// The caller hears the outcome rather than a log line: the caller prints it.
#[tokio::test]
async fn rebuild_answers_what_it_installed_or_why_it_could_not() {
    let Some(app) = app().await else { return };
    let root = project_dir(&[("src/jobs/tick.echo", "value:none")]);
    let installed = happyview::project::prepare::rebuild(&app.state, &root)
        .await
        .unwrap_or_else(|d| panic!("{d:?}"));
    assert_eq!(installed.generation, app.state.project.load().generation);

    std::fs::write(root.join("src/jobs/tick.echo"), "invalid syntax").unwrap();
    let errors = happyview::project::prepare::rebuild(&app.state, &root)
        .await
        .err()
        .expect("an invalid file fails the rebuild");
    assert!(
        errors
            .iter()
            .any(|d| d.path == Path::new("src/jobs/tick.echo")),
        "{errors:?}"
    );
    // Nothing was installed, so the last good build still serves.
    assert_eq!(installed.generation, app.state.project.load().generation);
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn an_interpreter_change_rebuilds_the_installed_project() {
    let Some(app) = app().await else { return };
    let root = project_dir(&[("src/jobs/tick.echo", "value:none")]);
    let report = build_snapshot(&app.state, &root, BuildOptions::default())
        .await
        .unwrap_or_else(|d| panic!("{d:?}"));
    install(&app.state, report.snapshot).await;
    let before = app.state.project.load().generation;
    let watcher = spawn_rebuild_on_interpreter_change(app.state.clone());

    app.state
        .plugin_registry
        .register(echo_interpreter::plugin_with_extensions("echo", &["echo"]))
        .await;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while app.state.project.load().generation == before {
        assert!(
            std::time::Instant::now() < deadline,
            "no rebuild after the interpreter changed"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    watcher.abort();
    std::fs::remove_dir_all(root).ok();
}

/// Waits for the generation to pass `before`, then for long enough that a
/// second rebuild of the same change would have landed, and answers the
/// generation it settled on.
async fn settled_generation_after(app: &TestApp, before: u64) -> u64 {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while app.state.project.load().generation == before {
        assert!(
            std::time::Instant::now() < deadline,
            "no rebuild after the interpreter changed"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    app.state.project.load().generation
}

/// `boot::start` is what `main` calls: the boot build, then in serve mode
/// the listener that rebuilds once for each interpreter change.
#[tokio::test]
async fn start_builds_and_rebuilds_once_per_interpreter_change() {
    use happyview::project::boot::{Mode, start};
    let Some(app) = app().await else { return };
    let root = project_dir(&[("src/jobs/tick.echo", "value:none")]);
    let installed = start(&app.state, &root, Mode::Serve)
        .await
        .unwrap_or_else(|d| panic!("{d:?}"));
    assert_eq!(installed.generation, app.state.project.load().generation);

    app.state
        .plugin_registry
        .register(echo_interpreter::plugin_with_extensions("echo", &["echo"]))
        .await;

    assert_eq!(
        settled_generation_after(&app, installed.generation).await,
        installed.generation + 1
    );
    std::fs::remove_dir_all(root).ok();
}

/// In dev mode the watch loop is the only listener: an interpreter change
/// is one build, reported with the line a save gets. `start` in dev mode
/// spawning its own listener as well would make this two.
#[tokio::test]
async fn dev_rebuilds_once_per_interpreter_change_and_prints_it() {
    use happyview::dev::watch::{WatchMode, spawn_with};
    use happyview::project::boot::{Mode, start};
    let Some(app) = app().await else { return };
    let root = project_dir(&[("src/jobs/tick.echo", "value:none")]);
    let installed = start(&app.state, &root, Mode::Dev)
        .await
        .unwrap_or_else(|d| panic!("{d:?}"));
    let session = happyview::dev::DevSession {
        root: root.clone(),
        public_url: String::new(),
        secrets: happyview::dev::secrets::load_or_create(&root.join(".happyview")).unwrap(),
        // A poll far longer than the test, so no file event adds a build.
        watch_mode: WatchMode::Poll(std::time::Duration::from_secs(60)),
    };
    let (tx, mut lines) = tokio::sync::mpsc::unbounded_channel::<String>();
    let _watch = spawn_with(app.state.clone(), session, move |line| {
        let _ = tx.send(line);
    })
    .unwrap();

    app.state
        .plugin_registry
        .register(echo_interpreter::plugin_with_extensions("echo", &["echo"]))
        .await;

    // Editor types are reported beside builds; this test counts builds.
    let line = loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(30), lines.recv())
            .await
            .expect("no build line after the interpreter changed")
            .expect("the watch loop ended");
        if !line.starts_with("types: ") {
            break line;
        }
    };
    assert!(line.starts_with("built in"), "{line}");
    assert_eq!(
        settled_generation_after(&app, installed.generation).await,
        installed.generation + 1
    );
    while let Ok(line) = lines.try_recv() {
        assert!(
            line.starts_with("types: "),
            "a second build was reported for one change: {line}"
        );
    }
    std::fs::remove_dir_all(root).ok();
}

async fn next(lines: &mut tokio::sync::mpsc::UnboundedReceiver<String>) -> String {
    tokio::time::timeout(std::time::Duration::from_secs(30), lines.recv())
        .await
        .expect("no line from the dev server")
        .expect("the watch loop ended")
}

/// The dev server keeps `.happyview/types/` current: written once the watch
/// starts, rewritten after a build that changes what a script can see, and
/// left alone, silently, after one that does not.
#[tokio::test]
async fn dev_writes_editor_types_at_start_and_after_a_change() {
    use happyview::dev::watch::{WatchMode, spawn_with};
    use happyview::project::boot::{Mode, start};
    let Some(app) = app().await else { return };
    let root = project_dir(&[("src/jobs/tick.echo", "value:none")]);
    start(&app.state, &root, Mode::Dev)
        .await
        .unwrap_or_else(|d| panic!("{d:?}"));
    let session = happyview::dev::DevSession {
        root: root.clone(),
        public_url: String::new(),
        secrets: happyview::dev::secrets::load_or_create(&root.join(".happyview")).unwrap(),
        watch_mode: WatchMode::Poll(std::time::Duration::from_millis(100)),
    };
    let (tx, mut lines) = tokio::sync::mpsc::unbounded_channel::<String>();
    let _watch = spawn_with(app.state.clone(), session, move |line| {
        let _ = tx.send(line);
    })
    .unwrap();
    assert_eq!(next(&mut lines).await, "types: echo: wrote 1 file");
    let written = root.join(".happyview/types/echo/input.json");
    assert!(written.exists());

    std::fs::create_dir_all(root.join("src/lexicons/com/example")).unwrap();
    std::fs::write(
        root.join("src/lexicons/com/example/thing.json"),
        r#"{"lexicon": 1, "id": "com.example.thing", "defs": {"main": {"type": "record", "key": "tid", "record": {"type": "object", "properties": {}}}}}"#,
    )
    .unwrap();
    let built = next(&mut lines).await;
    assert!(built.starts_with("built in"), "{built}");
    assert_eq!(next(&mut lines).await, "types: echo: wrote 1 file");
    let input = std::fs::read_to_string(&written).unwrap();
    assert!(input.contains("com.example.thing"), "{input}");

    // Another pass over the same snapshot writes and reports nothing.
    let before = std::fs::metadata(&written).unwrap().modified().unwrap();
    let again = happyview::dev::watch::refresh_types(&app.state, &root, false).await;
    assert!(again.is_empty(), "{again:?}");
    assert_eq!(
        std::fs::metadata(&written).unwrap().modified().unwrap(),
        before
    );
    std::fs::remove_dir_all(root).ok();
}
