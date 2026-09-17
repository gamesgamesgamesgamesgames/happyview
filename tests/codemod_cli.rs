//! Exercises the built `happyview-codemod` binary as a subprocess, so a
//! regression in argument parsing or exit-code plumbing shows up here rather
//! than only in the library's own unit tests.

mod common;

use std::io::Write;
use std::process::{Command, Stdio};

use happyview::db::{adapt_sql, now_rfc3339};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_happyview-codemod")
}

fn run_stdin(args: &[&str], stdin: &str) -> std::process::Output {
    let mut child = Command::new(bin())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn happyview-codemod");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    child
        .wait_with_output()
        .expect("wait for happyview-codemod")
}

#[test]
fn stdin_mode_on_an_already_migrated_script_exits_0_with_no_notes() {
    let source = "local log = require(\"internal.logging\")\n\nfunction handle(input, ctx)\n  log.info(\"hi\")\nend\n";
    let output = run_stdin(&["--stdin", "--kind", "query"], source);

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), source);
    assert!(
        output.stderr.is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn stdin_mode_on_a_script_with_a_marker_exits_2_and_reports_it_on_stderr() {
    let source = "function handle()\n  return TID.toNumber(params.tid)\nend\n";
    let output = run_stdin(&["--stdin", "--kind", "procedure"], source);

    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("-- codemod:"), "stdout: {stdout}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("line 2"), "stderr: {stderr}");
}

#[test]
fn file_mode_reads_the_named_file_and_exits_2_on_a_marker() {
    let dir = std::env::temp_dir().join(format!(
        "happyview_codemod_cli_test_{}_{}",
        std::process::id(),
        "file_mode_marker"
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join("script.lua");
    std::fs::write(
        &path,
        "function handle()\n  return TID.toNumber(params.tid)\nend\n",
    )
    .expect("write fixture script");

    let output = Command::new(bin())
        .args([
            "--file",
            path.to_str().expect("utf8 path"),
            "--kind",
            "procedure",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("run happyview-codemod");

    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("-- codemod:"), "stdout: {stdout}");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn file_mode_on_a_clean_script_exits_0() {
    let dir = std::env::temp_dir().join(format!(
        "happyview_codemod_cli_test_{}_{}",
        std::process::id(),
        "file_mode_clean"
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join("script.lua");
    let source = "local log = require(\"internal.logging\")\n\nfunction handle(input, ctx)\n  log.info(\"hi\")\nend\n";
    std::fs::write(&path, source).expect("write fixture script");

    let output = Command::new(bin())
        .args([
            "--file",
            path.to_str().expect("utf8 path"),
            "--kind",
            "query",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("run happyview-codemod");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), source);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn missing_kind_in_stdin_mode_exits_1_without_reading_stdin() {
    let output = Command::new(bin())
        .args(["--stdin"])
        .stdin(Stdio::null())
        .output()
        .expect("run happyview-codemod");
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn unparseable_source_exits_1() {
    let output = run_stdin(&["--stdin", "--kind", "query"], "function handle( end");
    assert_eq!(
        output.status.code(),
        Some(1),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn database_url_cannot_be_combined_with_file_or_stdin() {
    let output = Command::new(bin())
        .args(["--database-url", "sqlite://ignored.db", "--stdin"])
        .stdin(Stdio::null())
        .output()
        .expect("run happyview-codemod");
    assert_eq!(output.status.code(), Some(1));
}

/// A bad host must be a reported error, not a panic: `db::connect` (used
/// elsewhere in this codebase) `.expect()`s the connection and would abort
/// with a backtrace instead of the documented exit-1 contract.
#[test]
fn unreachable_database_exits_1_without_panicking() {
    let output = Command::new(bin())
        .args([
            "--database-url",
            "postgres://baduser:badpass@127.0.0.1:1/nonexistent",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("run happyview-codemod");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        !stderr.to_lowercase().contains("panicked"),
        "stderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Database mode: marker guard on --apply. Needs a real database, so these
// skip (not fail) without TEST_DATABASE_URL, same as the rest of the suite.
// ---------------------------------------------------------------------------

async fn seed_cli_test_script(
    pool: &sqlx::AnyPool,
    backend: happyview::db::DatabaseBackend,
    id: &str,
    body: &str,
) {
    let now = now_rfc3339();
    let sql = adapt_sql(
        "INSERT INTO happyview_scripts (id, body, script_type, created_at, updated_at) VALUES (?, ?, 'lua', ?, ?)
         ON CONFLICT (id) DO UPDATE SET body = EXCLUDED.body, updated_at = EXCLUDED.updated_at",
        backend,
    );
    happyview::db::query(&sql)
        .bind(id)
        .bind(body)
        .bind(&now)
        .bind(&now)
        .execute(pool)
        .await
        .expect("seed script fixture");
}

async fn fetch_cli_test_body(
    pool: &sqlx::AnyPool,
    backend: happyview::db::DatabaseBackend,
    id: &str,
) -> String {
    let sql = adapt_sql("SELECT body FROM happyview_scripts WHERE id = ?", backend);
    let (body,): (String,) = happyview::db::query_as(&sql)
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("fetch script body");
    body
}

async fn delete_cli_test_script(
    pool: &sqlx::AnyPool,
    backend: happyview::db::DatabaseBackend,
    id: &str,
) {
    let sql = adapt_sql("DELETE FROM happyview_scripts WHERE id = ?", backend);
    happyview::db::query(&sql).bind(id).execute(pool).await.ok();
}

const MARKED_SOURCE: &str = "function handle()\n  return TID.toNumber(params.tid)\nend\n";

#[tokio::test]
async fn db_mode_apply_skips_scripts_with_markers_without_allow_markers() {
    common::require_db!();
    // Held for the test's duration: this file has no `TestApp` to serialize
    // it against every other suite that truncates this same database.
    let _lock = common::db::acquire_test_lock().await;
    let pool = common::db::test_pool().await;
    let backend = common::db::test_backend();
    let id = "xrpc.procedure:com.example.codemod_cli_marker_skip";
    seed_cli_test_script(&pool, backend, id, MARKED_SOURCE).await;

    let url = std::env::var("TEST_DATABASE_URL").expect("checked by require_db!");
    let output = Command::new(bin())
        .args(["--database-url", &url, "--script", id, "--apply"])
        .stdin(Stdio::null())
        .output()
        .expect("run happyview-codemod");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(2), "stdout: {stdout}");
    assert!(stdout.contains("not applying"), "stdout: {stdout}");
    assert_eq!(fetch_cli_test_body(&pool, backend, id).await, MARKED_SOURCE);

    delete_cli_test_script(&pool, backend, id).await;
}

#[tokio::test]
async fn db_mode_apply_with_allow_markers_writes_back_despite_markers() {
    common::require_db!();
    let _lock = common::db::acquire_test_lock().await;
    let pool = common::db::test_pool().await;
    let backend = common::db::test_backend();
    let id = "xrpc.procedure:com.example.codemod_cli_marker_override";
    seed_cli_test_script(&pool, backend, id, MARKED_SOURCE).await;

    let url = std::env::var("TEST_DATABASE_URL").expect("checked by require_db!");
    let output = Command::new(bin())
        .args([
            "--database-url",
            &url,
            "--script",
            id,
            "--apply",
            "--allow-markers",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("run happyview-codemod");

    // Markers are still present in the stored source, so exit stays 2 even
    // though the write succeeded.
    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stored = fetch_cli_test_body(&pool, backend, id).await;
    assert!(
        stored.contains("-- codemod:"),
        "expected the marker comment to be written back: {stored}"
    );

    delete_cli_test_script(&pool, backend, id).await;
}
