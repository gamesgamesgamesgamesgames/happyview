//! The job worker's script branch, driven through the real worker.
//!
//! The `interpreter_echo` fixture claims `lua`, which is what a job script
//! row's `script_type` holds, so a claimed job resolves its row and reaches the
//! fixture exactly as it would reach a deployed interpreter. The fixture
//! interprets nothing — its `source` is a directive — so what this pins is the
//! worker's half: what reaches an interpreter, what becomes of the job row, and
//! which of the job's own controls the run can act on.
//!
//! Language semantics belong to the interpreter and are pinned where it is
//! loaded, in `lua_interpreter_plugin.rs` and `lua_differential.rs`.

mod common;

use std::time::Duration;

use serde_json::{Value, json};
use serial_test::serial;
use uuid::Uuid;

use common::app::TestApp;
use common::echo_interpreter;

const TYPE: &str = "test.probe";

/// Skips the caller when a fixture's module is absent, naming the build that
/// produces it. Every fixture's `target/` is gitignored, so that is a step
/// nobody ran; panicking on it would report it as a failure of the worker.
macro_rules! require_echo {
    () => {
        if !echo_interpreter::is_built() {
            eprintln!("skipping: {}", echo_interpreter::BUILD);
            return;
        }
    };
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// An app with the echo fixture installed as the interpreter for `lua`, the
/// language a seeded script row names.
async fn app() -> TestApp {
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .register(echo_interpreter::plugin("lua"))
        .await;
    app
}

/// Seed a job script row directly. `POST /admin/scripts` validates what it
/// stores, and a directive is not a script in the language the row names.
async fn seed_script(app: &TestApp, job_type: &str, body: &str) {
    let now = happyview::db::now_rfc3339();
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_scripts (id, body, script_type, created_at, updated_at) \
         VALUES (?, ?, 'lua', ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(format!("job.run:{job_type}"))
        .bind(body)
        .bind(&now)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .expect("seed a script row");
}

async fn seed_job(app: &TestApp, job_type: &str, input: &Value) -> String {
    seed_job_as(app, job_type, input, false, None, None).await
}

async fn seed_job_as(
    app: &TestApp,
    job_type: &str,
    input: &Value,
    inherit_auth: bool,
    api_client_id: Option<&str>,
    dpop_key_id: Option<&str>,
) -> String {
    let id = Uuid::new_v4().to_string();
    let now = happyview::db::now_rfc3339();
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_jobs \
           (id, job_type, status, input, progress, created_by, created_at, inherit_auth, \
            api_client_id, dpop_key_id) \
         VALUES (?, ?, 'pending', ?, '{}', ?, ?, ?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(&id)
        .bind(job_type)
        .bind(input.to_string())
        .bind(&app.admin_did)
        .bind(&now)
        .bind(inherit_auth)
        .bind(api_client_id)
        .bind(dpop_key_id)
        .execute(&app.state.db)
        .await
        .expect("enqueue a job");
    id
}

struct JobRow {
    status: String,
    result: Option<String>,
    error: Option<String>,
    progress: String,
}

async fn job_row(app: &TestApp, id: &str) -> JobRow {
    let sql = happyview::db::adapt_sql(
        "SELECT status, result, error, progress FROM happyview_jobs WHERE id = ?",
        app.state.db_backend,
    );
    let (status, result, error, progress) =
        happyview::db::query_as::<(String, Option<String>, Option<String>, String)>(&sql)
            .bind(id)
            .fetch_one(&app.state.db)
            .await
            .expect("read the job row");
    JobRow {
        status,
        result,
        error,
        progress,
    }
}

async fn set_status(app: &TestApp, id: &str, status: &str) {
    let sql = happyview::db::adapt_sql(
        "UPDATE happyview_jobs SET status = ? WHERE id = ?",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(status)
        .bind(id)
        .execute(&app.state.db)
        .await
        .expect("set the job status");
}

/// Poll until the job reaches a state nothing will move it out of. The
/// returned row is whatever was there at the end either way, so an assertion
/// reports the state it found rather than a timeout.
///
/// `cancelling` and `pausing` are states a run is still in: the worker reads
/// them once the body returns and settles the row, so waiting only for
/// `pending`/`running` to clear would read the request rather than the outcome.
async fn await_terminal(app: &TestApp, id: &str) -> JobRow {
    for _ in 0..600 {
        let row = job_row(app, id).await;
        if !matches!(
            row.status.as_str(),
            "pending" | "running" | "cancelling" | "pausing"
        ) {
            return row;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    job_row(app, id).await
}

/// Poll until the worker has claimed the job, which is the window in which a
/// pause or cancel reaches a run rather than the queue.
async fn await_running(app: &TestApp, id: &str) {
    for _ in 0..200 {
        if job_row(app, id).await.status == "running" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the worker never claimed the job");
}

/// Run the worker until `id` reaches a terminal state, then stop it.
async fn run_to_completion(app: &TestApp, id: &str) -> JobRow {
    let worker = tokio::spawn(happyview::jobs::worker::run_worker(app.state.clone()));
    let row = await_terminal(app, id).await;
    worker.abort();
    row
}

fn result_of(row: &JobRow) -> Value {
    serde_json::from_str(row.result.as_deref().unwrap_or_else(|| {
        panic!(
            "no result persisted; status {}, error {:?}",
            row.status, row.error
        )
    }))
    .expect("the result column holds JSON")
}

// ---------------------------------------------------------------------------
// What reaches the interpreter
// ---------------------------------------------------------------------------

/// The whole input a job run sends, echoed back as the job's result: the kind,
/// the body, the enqueued input, the context the worker fills and the budget a
/// job does not get.
#[tokio::test]
#[serial]
async fn a_job_hands_the_interpreter_its_input_its_job_and_no_instruction_budget() {
    common::require_db!();
    require_echo!();
    let app = app().await;
    // created_at has no default on Postgres — bind it explicitly.
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_script_variables (key, value, created_at) VALUES ('API_KEY', 'k', ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(happyview::db::now_rfc3339())
        .execute(&app.state.db)
        .await
        .expect("seed a script variable");
    seed_script(&app, TYPE, "probe").await;
    let input = json!({ "since": "2026-01-01", "pages": 3 });
    let id = seed_job(&app, TYPE, &input).await;

    let row = run_to_completion(&app, &id).await;
    assert_eq!(row.status, "completed", "job error: {:?}", row.error);
    let sent = result_of(&row);

    assert_eq!(sent["kind"], "job");
    assert_eq!(sent["source"], "probe");
    assert_eq!(sent["input"], input);
    assert_eq!(sent["context"]["trigger"], format!("job.run:{TYPE}"));
    assert_eq!(sent["context"]["caller_did"], app.admin_did);
    assert_eq!(sent["context"]["job"]["id"], id);
    assert_eq!(sent["context"]["has_pds_auth"], false);
    assert_eq!(sent["context"]["env"]["API_KEY"], "k");
    assert!(
        sent["limits"]["instructions"].is_null(),
        "a job has no instruction budget: {sent}"
    );
    for absent in ["method", "collection", "params", "delegate_did", "space"] {
        assert!(
            sent["context"].get(absent).is_none(),
            "a job run has no {absent}: {sent}"
        );
    }
}

// ---------------------------------------------------------------------------
// What becomes of the row
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn a_returned_value_becomes_the_job_result() {
    common::require_db!();
    require_echo!();
    let app = app().await;
    seed_script(&app, TYPE, r#"returns:{"indexed":42}"#).await;
    let id = seed_job(&app, TYPE, &json!({})).await;

    let row = run_to_completion(&app, &id).await;
    assert_eq!(row.status, "completed", "job error: {:?}", row.error);
    assert_eq!(result_of(&row), json!({ "indexed": 42 }));
    assert!(row.error.is_none());
}

/// A script that returned nothing completes with a null result rather than
/// failing: a job is run for its effects, and "returned nothing" is an
/// outcome only a record event reads as a decision.
#[tokio::test]
#[serial]
async fn a_job_that_returned_nothing_completes_with_a_null_result() {
    common::require_db!();
    require_echo!();
    let app = app().await;
    seed_script(&app, TYPE, "value:none").await;
    let id = seed_job(&app, TYPE, &json!({})).await;

    let row = run_to_completion(&app, &id).await;
    assert_eq!(row.status, "completed", "job error: {:?}", row.error);
    assert_eq!(result_of(&row), Value::Null);
}

/// The `error` column is the only place an operator reads why a job failed, so
/// it carries the failure's category as well as the interpreter's text: a spent
/// budget and an exhausted heap describe themselves in no prose, and without
/// the category they read as any other runtime failure.
#[tokio::test]
#[serial]
async fn a_failed_run_fails_the_job_keeping_its_category_and_its_line() {
    common::require_db!();
    require_echo!();
    let app = app().await;
    let mut categories = Vec::new();
    for kind in ["runtime", "timeout", "memory"] {
        let job_type = format!("test.{kind}");
        seed_script(&app, &job_type, &format!("error:{kind}")).await;
        let id = seed_job(&app, &job_type, &json!({})).await;

        let row = run_to_completion(&app, &id).await;
        assert_eq!(row.status, "failed", "{kind}");
        assert!(row.result.is_none(), "{kind}: a failed job has no result");
        let error = row.error.expect("no error persisted");
        assert!(
            error.contains("[string \"script\"]:7:"),
            "{kind}: the line that points at the script is gone: {error}"
        );
        categories.push(
            error
                .split_once(": ")
                .map(|(category, _)| category.to_string())
                .unwrap_or(error),
        );
    }
    assert_eq!(categories, ["runtime", "timeout", "memory"]);
}

/// A language nothing installed claims fails the job naming it, rather than
/// leaving the row running with nobody to run it.
#[tokio::test]
#[serial]
async fn a_job_whose_language_has_no_interpreter_fails_naming_it() {
    common::require_db!();
    let app = TestApp::new().await;
    seed_script(&app, TYPE, "probe").await;
    let id = seed_job(&app, TYPE, &json!({})).await;

    let row = run_to_completion(&app, &id).await;
    assert_eq!(row.status, "failed");
    let error = row.error.expect("no error persisted");
    assert!(error.contains("lua"), "{error}");
}

// ---------------------------------------------------------------------------
// The job's own controls
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn progress_from_a_run_lands_on_the_row_and_in_a_read() {
    common::require_db!();
    require_echo!();
    let app = app().await;
    seed_script(&app, TYPE, "host:job_progress").await;
    let id = seed_job(&app, TYPE, &json!({ "done": 3, "total": 10 })).await;

    let row = run_to_completion(&app, &id).await;
    assert_eq!(row.status, "completed", "job error: {:?}", row.error);
    assert_eq!(
        serde_json::from_str::<Value>(&row.progress).expect("the progress column holds JSON"),
        json!({ "done": 3, "total": 10 })
    );

    let read = app.get_json(&format!("/admin/jobs/{id}")).await;
    assert_eq!(read["progress"], json!({ "done": 3, "total": 10 }));
}

/// The stop flag the run reads is its own job's. A job nobody has asked to
/// stop answers `false`, which is what lets a script loop at all.
#[tokio::test]
#[serial]
async fn a_running_job_is_not_asked_to_stop() {
    common::require_db!();
    require_echo!();
    let app = app().await;
    seed_script(&app, TYPE, "host:job_should_stop").await;
    let id = seed_job(&app, TYPE, &json!({})).await;

    let row = run_to_completion(&app, &id).await;
    assert_eq!(row.status, "completed", "job error: {:?}", row.error);
    assert_eq!(result_of(&row), json!({ "ok": false }));
}

/// A cooperative exit is not a failure: the job lands in the state that was
/// asked for and records neither a result nor an error.
#[tokio::test]
#[serial]
async fn a_stop_asked_for_mid_run_lands_the_job_in_the_state_it_named() {
    common::require_db!();
    require_echo!();
    let app = app().await;
    for (asked, landed) in [("cancelling", "cancelled"), ("pausing", "paused")] {
        let job_type = format!("test.{asked}");
        // A host wait holds the run open long enough for the status to change
        // under it, which is the window a dashboard button acts in.
        seed_script(&app, &job_type, "host:job_wait").await;
        let id = seed_job(&app, &job_type, &json!({ "seconds": 1.0 })).await;

        let worker = tokio::spawn(happyview::jobs::worker::run_worker(app.state.clone()));
        await_running(&app, &id).await;
        set_status(&app, &id, asked).await;
        let row = await_terminal(&app, &id).await;
        worker.abort();

        assert_eq!(row.status, landed, "{asked}");
        assert!(row.result.is_none(), "{asked}: {:?}", row.result);
        assert!(row.error.is_none(), "{asked}: {:?}", row.error);
    }
}

/// A wait is host time charged to the instance's own counter, and one asked
/// for in negative seconds costs nothing rather than refusing.
#[tokio::test]
#[serial]
async fn a_wait_adds_its_time_to_the_counter_and_a_negative_one_costs_nothing() {
    common::require_db!();
    require_echo!();
    let app = app().await;
    let counter = &app.state.telemetry_counters.job_wait_ms;
    seed_script(&app, TYPE, "host:job_wait").await;

    let id = seed_job(&app, TYPE, &json!({ "seconds": 0.4 })).await;
    let started = std::time::Instant::now();
    let row = run_to_completion(&app, &id).await;
    assert_eq!(row.status, "completed", "job error: {:?}", row.error);
    assert!(started.elapsed() >= Duration::from_millis(400));
    let waited = counter.load(std::sync::atomic::Ordering::Relaxed);
    assert!(waited >= 400, "{waited}ms recorded for a 0.4s wait");

    let id = seed_job(&app, TYPE, &json!({ "seconds": -5.0 })).await;
    let started = std::time::Instant::now();
    let row = run_to_completion(&app, &id).await;
    assert_eq!(row.status, "completed", "job error: {:?}", row.error);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(
        counter.load(std::sync::atomic::Ordering::Relaxed),
        waited,
        "a negative wait should cost nothing"
    );
}

// ---------------------------------------------------------------------------
// Whose credentials the run carries
// ---------------------------------------------------------------------------

/// Load and install the `sdk_caller` fixture, built at
/// `cargo build --manifest-path tests/fixtures/sdk_caller/Cargo.toml --target wasm32-unknown-unknown --release`.
///
/// It is the cheapest library that tells a run with a session from one
/// without: every caller-acting import refuses outright when there is nothing
/// to act as.
async fn install_caller_library(app: &TestApp) -> bool {
    let Ok(plugin) = happyview::plugin::loader::load_from_file(std::path::Path::new(
        "tests/fixtures/sdk_caller",
    ))
    .await
    else {
        eprintln!(
            "skipping: sdk_caller fixture not built. Run: cargo build --manifest-path \
             tests/fixtures/sdk_caller/Cargo.toml --target wasm32-unknown-unknown --release"
        );
        return false;
    };
    app.state
        .plugin_registry
        .install(plugin)
        .await
        .expect("install the caller fixture");
    true
}

/// A library call reached through `require`'s contract: the value on success,
/// a raise on an error envelope.
fn create_record_call() -> Value {
    json!({
        "library": "sdk_caller",
        "function": "create_record",
        "args": [{
            "collection": "com.example.post",
            "record": { "text": "hi" },
            "validate": false,
        }],
    })
}

#[tokio::test]
#[serial]
async fn a_job_without_inherited_auth_lends_a_library_no_session() {
    common::require_db!();
    require_echo!();
    let app = app().await;
    if !install_caller_library(&app).await {
        return;
    }
    seed_script(&app, TYPE, "host:require").await;
    let id = seed_job(&app, TYPE, &create_record_call()).await;

    let row = run_to_completion(&app, &id).await;
    assert_eq!(row.status, "failed");
    let error = row.error.expect("no error persisted");
    assert!(error.contains("NO_SESSION"), "{error}");
}

/// With `inherit_auth` the run acts as the creator: the library reaches the
/// creator's own DPoP session, named by the columns the job carries. There is
/// no such session here, so what this reads is which session was looked for.
#[tokio::test]
#[serial]
async fn a_job_with_inherited_auth_acts_as_its_creator() {
    common::require_db!();
    require_echo!();
    let app = app().await;
    if !install_caller_library(&app).await {
        return;
    }
    seed_script(&app, TYPE, "host:require").await;
    let id = seed_job_as(
        &app,
        TYPE,
        &create_record_call(),
        true,
        Some("api-client-1"),
        Some("dpop-key-1"),
    )
    .await;

    let row = run_to_completion(&app, &id).await;
    assert_eq!(row.status, "failed");
    let error = row.error.expect("no error persisted");
    assert!(
        !error.contains("NO_SESSION"),
        "the run carried no session at all: {error}"
    );
    assert!(error.contains("DPoP session"), "{error}");
}
