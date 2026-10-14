//! `happyview test` against a project on disk, with the echo fixture as both
//! the handlers' and the tests' interpreter. A binary of its own: a run turns
//! on forged identities for the whole process.

mod common;

use std::path::Path;
use std::time::Duration;

use happyview::cli::TestArgs;
use tempfile::TempDir;

fn project() -> Option<TempDir> {
    if !common::echo_interpreter::is_built() {
        eprintln!("skipping: {}", common::echo_interpreter::BUILD);
        return None;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let plugin = root.join(".happyview/plugins/interpreter_echo");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::copy(
        "tests/fixtures/interpreter_echo/target/wasm32-unknown-unknown/release/interpreter_echo.wasm",
        plugin.join("interpreter_echo.wasm"),
    )
    .unwrap();
    let mut manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string("tests/fixtures/interpreter_echo/manifest.json").unwrap(),
    )
    .unwrap();
    manifest["wasm_file"] = "interpreter_echo.wasm".into();
    std::fs::write(plugin.join("manifest.json"), manifest.to_string()).unwrap();
    write(
        root,
        "src/lexicons/com/example/getThing.json",
        &serde_json::json!({
            "lexicon": 1, "id": "com.example.getThing",
            "defs": {"main": {"type": "query", "parameters": {"type": "params", "properties": {"id": {"type": "string"}}}}}
        })
        .to_string(),
    );
    write(
        root,
        "src/lexicons/com/example/getThing.echo",
        "returns:{\"thing\":1}",
    );
    Some(dir)
}

fn write(root: &Path, path: &str, body: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn test_file(checks: &str) -> String {
    format!(
        r#"test:[{{"import":"trigger","request":{{"kind":"query","target":"com.example.getThing","input":{{"id":"1"}},"as":"did:plc:alice"}}}},{checks}]"#
    )
}

async fn run(root: &Path, filter: Option<&str>) -> (u8, String) {
    let mut out = Vec::new();
    let code = happyview::testing::runner::run(
        &TestArgs {
            root: root.to_path_buf(),
            filter: filter.map(str::to_string),
            ..TestArgs::default()
        },
        &mut out,
    )
    .await;
    (code, String::from_utf8(out).unwrap())
}

#[tokio::test]
async fn a_passing_and_a_failing_file_report_and_exit_one() {
    let Some(dir) = project() else { return };
    let root = dir.path();
    write(
        root,
        "src/lexicons/com/example/getThing.test.echo",
        &test_file(
            r#"{"check":{"name":"answers 200","answer":0,"pointer":"/ok/status","equals":200,"line":2}}"#,
        ),
    );
    write(
        root,
        "src/lib/broken.test.echo",
        &test_file(
            r#"{"check":{"name":"answers 404","answer":0,"pointer":"/ok/status","equals":404,"line":5}}"#,
        ),
    );
    let (code, out) = run(root, None).await;
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains(" PASS  src/lexicons/com/example/getThing.test.echo (1 tests)"),
        "{out}"
    );
    assert!(
        out.contains(" FAIL  src/lib/broken.test.echo > answers 404"),
        "{out}"
    );
    let (code, out) = run(root, Some("getThing")).await;
    assert_eq!(code, 0, "{out}");
}

#[tokio::test]
async fn a_file_that_reports_no_tests_fails_the_run() {
    let Some(dir) = project() else { return };
    let root = dir.path();
    write(root, "src/lib/empty.test.echo", "test:[]");
    let (code, out) = run(root, None).await;
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("reported no tests"), "{out}");
}

#[tokio::test]
async fn a_filter_matching_nothing_exits_one() {
    let Some(dir) = project() else { return };
    let root = dir.path();
    write(root, "src/lib/a.test.echo", "test:[]");
    let (code, out) = run(root, Some("nonesuch")).await;
    assert_eq!(code, 1);
    assert!(out.contains("no test file matches 'nonesuch'"), "{out}");
}

#[tokio::test]
async fn a_build_error_exits_two_with_its_path() {
    let Some(dir) = project() else { return };
    let root = dir.path();
    write(root, "src/lexicons/com/example/bad.json", "{ not json");
    let (code, out) = run(root, None).await;
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("src/lexicons/com/example/bad.json"), "{out}");
}

/// An interpreter that cannot decode a test file's input is out of date, not
/// broken, and the file's error says which one and what it needs.
#[tokio::test]
async fn an_interpreter_that_predates_test_runs_is_named() {
    let Some(dir) = project() else { return };
    let root = dir.path();
    write(root, "src/lib/old.test.echo", "predates-tests");
    let (code, out) = run(root, None).await;
    assert_eq!(code, 1, "{out}");
    assert!(out.contains(" FAIL  src/lib/old.test.echo"), "{out}");
    assert!(
        out.contains("the 'interpreter_echo' interpreter predates test runs"),
        "{out}"
    );
    assert!(out.contains("happyview-plugin-sdk 0.3.0 or later"), "{out}");
}

/// A project's `.env` holds real secrets for a served instance. Through the
/// real binary, since `main` is what decides whether to load it: here it
/// points `PLUGIN_DIR` at a plugin that cannot load, so a run that read it
/// would exit 2.
#[test]
fn the_test_command_does_not_load_the_projects_env() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let bogus = root.join("bogus/broken");
    std::fs::create_dir_all(&bogus).unwrap();
    std::fs::write(bogus.join("manifest.json"), "{}").unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join(".env"),
        format!("PLUGIN_DIR={}\n", root.join("bogus").display()),
    )
    .unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_happyview"))
        .args(["test", "."])
        .current_dir(root)
        .env_remove("PLUGIN_DIR")
        .env_remove("DATABASE_URL")
        .env_remove("HAPPYVIEW_PROJECT")
        .output()
        .expect("run happyview test");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("no test files"), "{stdout}");
}

/// Each run reads its own project's `.env.test` as `ctx.env`. The runs start
/// together, so their configuration loads would interleave if anything let
/// two runs share the process at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_runs_each_read_their_own_env_test() {
    const RUNS: usize = 8;
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(RUNS));
    let mut handles = Vec::new();
    for n in 0..RUNS {
        let Some(dir) = project() else { return };
        let root = dir.path();
        std::fs::write(root.join(".env.test"), format!("WHO=run-{n}\n")).unwrap();
        write(
            root,
            "src/lexicons/com/example/whoami.json",
            &serde_json::json!({
                "lexicon": 1, "id": "com.example.whoami",
                "defs": {"main": {"type": "query"}}
            })
            .to_string(),
        );
        write(root, "src/lexicons/com/example/whoami.echo", "value:object");
        write(
            root,
            "src/lib/env.test.echo",
            &format!(
                r#"test:[{{"import":"trigger","request":{{"kind":"query","target":"com.example.whoami","as":"did:plc:alice"}}}},{{"check":{{"name":"reads its own env","answer":0,"pointer":"/ok/body/env/WHO","equals":"run-{n}","line":1}}}}]"#
            ),
        );
        let barrier = barrier.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            let result = run(dir.path(), None).await;
            drop(dir);
            result
        }));
    }
    for handle in handles {
        let (code, out) = handle.await.unwrap();
        assert_eq!(code, 0, "{out}");
    }
}

/// A test file that never returns is stopped by its deadline and reported
/// as that file's failure, not left to hang the run. Slow by necessity: the
/// deadline is the test-file budget of a minute.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_file_that_spins_is_stopped_and_reported() {
    let Some(dir) = project() else { return };
    let root = dir.path();
    write(root, "src/lib/spin.test.echo", r#"test:[{"spin":true}]"#);
    write(
        root,
        "src/lexicons/com/example/getThing.test.echo",
        &test_file(
            r#"{"check":{"name":"answers 200","answer":0,"pointer":"/ok/status","equals":200,"line":2}}"#,
        ),
    );
    let (code, out) = tokio::time::timeout(Duration::from_secs(240), run(root, None))
        .await
        .expect("the run finishes");
    assert_eq!(code, 1, "{out}");
    assert!(out.contains(" FAIL  src/lib/spin.test.echo"), "{out}");
    // The other file finishes on its own time rather than waiting out the
    // spinning one's deadline.
    let pass = out
        .lines()
        .find(|l| l.starts_with(" PASS  src/lexicons/com/example/getThing.test.echo"))
        .unwrap_or_else(|| panic!("{out}"));
    let ms: u64 = pass
        .rsplit(' ')
        .next()
        .and_then(|t| t.strip_suffix("ms"))
        .and_then(|t| t.parse().ok())
        .unwrap_or_else(|| panic!("{pass}"));
    assert!(ms < 30_000, "{out}");
}
