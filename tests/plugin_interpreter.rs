//! The interpreter ABI end to end: `execute_script`, `validate_script`, and
//! the four `script:host` imports against the same job row and event log the
//! native runner writes to. The `interpreter_echo` fixture interprets nothing
//! — its `source` is a directive — so what this pins is the host's half of the
//! contract, not any language.

mod common;

use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

use common::app::TestApp;

use happyview::error::ScriptErrorType;
use happyview::plugin::{
    ExecutionError, LoadedPlugin, PluginInfo, PluginManifest, PluginSource, ScriptErrorKind,
    ScriptExecuteContext, ScriptExecuteInput, ScriptExecuteLimits, ScriptExecuteOutput, ScriptJob,
    ScriptKind, ScriptLibraryRef, ScriptValueKind,
};

const FIXTURE: &str =
    "tests/fixtures/interpreter_echo/target/wasm32-unknown-unknown/release/interpreter_echo.wasm";

const LIBRARY_FIXTURE: &str =
    "tests/fixtures/test_library/target/wasm32-unknown-unknown/release/test_library.wasm";

/// Skips the caller when a fixture's module is absent, naming the build that
/// produces it. Every fixture's `target/` is gitignored, so that is a step
/// nobody ran; panicking on it would report it as a failure of the ABI under
/// test.
macro_rules! require_fixture {
    ($name:literal, $target:literal) => {
        if !std::path::Path::new(concat!(
            "tests/fixtures/",
            $name,
            "/target/",
            $target,
            "/release/",
            $name,
            ".wasm"
        ))
        .exists()
        {
            eprintln!(concat!(
                "skipping: ",
                $name,
                " fixture not built. Run: cargo build --manifest-path tests/fixtures/",
                $name,
                "/Cargo.toml --target ",
                $target,
                " --release"
            ));
            return;
        }
    };
}

/// The echo module, guarded by `require_fixture!` at every call site.
fn fixture_bytes() -> Vec<u8> {
    std::fs::read(FIXTURE).expect("the echo module should read")
}

fn plugin(id: &str, plugin_type: &str, capabilities: &[&str]) -> LoadedPlugin {
    let mut manifest = json!({
        "id": id, "name": id, "version": "1.0.0", "api_version": "2",
        "plugin_type": plugin_type, "capabilities": capabilities,
    });
    if plugin_type == "interpreter" {
        manifest["language_id"] = json!("echo");
    } else {
        manifest["namespace"] = json!(id);
    }
    let manifest: PluginManifest = serde_json::from_value(manifest).unwrap();
    LoadedPlugin {
        info: PluginInfo {
            id: id.into(),
            name: id.into(),
            version: "1.0.0".into(),
            api_version: "2".into(),
            icon_url: None,
            required_secrets: vec![],
            auth_type: "oauth2".into(),
            config_schema: None,
        },
        source: PluginSource::File {
            path: "tests/fixtures/interpreter_echo".into(),
        },
        wasm_bytes: fixture_bytes(),
        manifest: Some(manifest),
    }
}

/// The raw-ABI library fixture, so `host_call_library` from an interpreter
/// reaches a real library rather than a stub.
fn library_plugin(id: &str) -> LoadedPlugin {
    let manifest: PluginManifest = serde_json::from_value(json!({
        "id": id, "name": id, "version": "1.0.0", "api_version": "2",
        "plugin_type": "library", "namespace": "testlib",
        "capabilities": ["library:call"],
    }))
    .unwrap();
    LoadedPlugin {
        info: PluginInfo {
            id: id.into(),
            name: id.into(),
            version: "1.0.0".into(),
            api_version: "2".into(),
            icon_url: None,
            required_secrets: vec![],
            auth_type: "oauth2".into(),
            config_schema: None,
        },
        source: PluginSource::File {
            path: "tests/fixtures/test_library".into(),
        },
        wasm_bytes: std::fs::read(LIBRARY_FIXTURE).expect("the library module should read"),
        manifest: Some(manifest),
    }
}

async fn interpreter(app: &TestApp) -> &'static str {
    app.state
        .plugin_registry
        .register(plugin(
            "interpreter_echo",
            "interpreter",
            &["library:call", "script:host"],
        ))
        .await;
    "interpreter_echo"
}

fn input(source: &str, kind: ScriptKind) -> ScriptExecuteInput {
    ScriptExecuteInput {
        source: source.to_string(),
        kind,
        input: Value::Null,
        context: ScriptExecuteContext {
            trigger: "xrpc.query:app.test.q".into(),
            caller_did: Some("did:plc:runner".into()),
            has_pds_auth: false,
            env: Default::default(),
            method: Some("app.test.q".into()),
            ..ScriptExecuteContext::default()
        },
        libraries: Vec::new(),
        limits: ScriptExecuteLimits {
            instructions: Some(1_000_000),
            memory_bytes: 16 * 1024 * 1024,
        },
        removed_globals: Vec::new(),
    }
}

fn returned(output: ScriptExecuteOutput) -> (Value, ScriptValueKind) {
    match output {
        ScriptExecuteOutput::Returned { value, value_kind } => (value, value_kind),
        other => panic!("expected a returned value, got {other:?}"),
    }
}

/// `jobs::db` is crate-private, so the row goes in directly — the same way
/// `tests/e2e_jobs.rs` seeds one.
async fn seeded_job(app: &TestApp) -> String {
    let id = Uuid::new_v4().to_string();
    happyview::db::query(&happyview::db::adapt_sql(
        "INSERT INTO happyview_jobs (id, job_type, status, input, progress, created_by, created_at, inherit_auth)
         VALUES (?, 'test.interpreter', 'running', '{}', '{}', ?, ?, ?)",
        app.state.db_backend,
    ))
    .bind(&id)
    .bind("did:plc:runner")
    .bind(happyview::db::now_rfc3339())
    .bind(false)
    .execute(&app.state.db)
    .await
    .expect("seed a job row");
    id
}

async fn set_job_status(app: &TestApp, job: &str, status: &str) {
    happyview::db::query(&happyview::db::adapt_sql(
        "UPDATE happyview_jobs SET status = ? WHERE id = ?",
        app.state.db_backend,
    ))
    .bind(status)
    .bind(job)
    .execute(&app.state.db)
    .await
    .unwrap();
}

#[tokio::test]
async fn execute_round_trips_the_whole_input_back_as_the_returned_value() {
    common::require_db!();
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    let app = TestApp::new().await;
    let id = interpreter(&app).await;

    let mut sent = input("echo me", ScriptKind::XrpcProcedure);
    sent.input = json!({"title": "hello"});
    sent.libraries = vec![ScriptLibraryRef {
        namespace: "testlib".into(),
        id: "testlib".into(),
    }];
    sent.context.collection = Some("app.test.rec".into());

    let (value, kind) = returned(
        app.state
            .plugin_executor()
            .execute_script(id, &sent)
            .await
            .expect("execute should answer"),
    );
    assert_eq!(kind, ScriptValueKind::Object);
    assert_eq!(value["source"], "echo me");
    assert_eq!(value["kind"], "xrpc_procedure");
    assert_eq!(value["input"]["title"], "hello");
    assert_eq!(value["context"]["trigger"], "xrpc.query:app.test.q");
    assert_eq!(value["context"]["collection"], "app.test.rec");
    assert_eq!(value["libraries"][0]["namespace"], "testlib");
    assert_eq!(value["limits"]["instructions"], 1_000_000);
    // Absent is not null: a field that does not apply to this kind is missing
    // from what the guest received rather than present and null.
    assert!(value["context"].get("space").is_none(), "{value}");
    assert!(value["context"].get("job").is_none(), "{value}");
}

#[tokio::test]
async fn each_value_kind_comes_back_as_itself() {
    common::require_db!();
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    let app = TestApp::new().await;
    let id = interpreter(&app).await;
    let executor = app.state.plugin_executor();

    let (value, kind) = returned(
        executor
            .execute_script(id, &input("value:none", ScriptKind::RecordEvent))
            .await
            .unwrap(),
    );
    assert_eq!(kind, ScriptValueKind::None);
    assert_eq!(value, Value::Null);

    let (value, kind) = returned(
        executor
            .execute_script(id, &input("value:object", ScriptKind::RecordEvent))
            .await
            .unwrap(),
    );
    assert_eq!(kind, ScriptValueKind::Object);
    assert_eq!(value["trigger"], "xrpc.query:app.test.q");

    // `other` covers an explicit null, which is why `none` exists at all.
    let mut sent = input("value:other", ScriptKind::RecordEvent);
    sent.input = Value::Null;
    let (value, kind) = returned(executor.execute_script(id, &sent).await.unwrap());
    assert_eq!(kind, ScriptValueKind::Other);
    assert_eq!(value, Value::Null);
}

#[tokio::test]
async fn every_error_kind_maps_onto_its_script_error_type_and_only_timeout_is_408() {
    common::require_db!();
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    let app = TestApp::new().await;
    let id = interpreter(&app).await;

    for kind in ScriptErrorKind::ALL {
        let source = format!("error:{}", kind.as_str());
        let output = app
            .state
            .plugin_executor()
            .execute_script(id, &input(&source, ScriptKind::XrpcQuery))
            .await
            .expect("execute should answer");
        let ScriptExecuteOutput::Error {
            kind: reported,
            message,
            line,
            raw,
        } = output
        else {
            panic!("expected an error output for {kind:?}");
        };
        assert_eq!(reported, kind);
        assert_eq!(line, Some(7));
        assert!(message.contains(kind.as_str()), "{message}");
        assert!(raw.contains("[string \"script\"]"), "{raw}");

        let error_type = ScriptErrorType::from(reported);
        assert_eq!(error_type.to_string(), kind.as_str());
        let status =
            axum::response::IntoResponse::into_response(happyview::error::AppError::ScriptError {
                error_type,
                message,
                method: "app.test.q".into(),
                line,
            })
            .status();
        let expected = if kind == ScriptErrorKind::Timeout {
            axum::http::StatusCode::REQUEST_TIMEOUT
        } else {
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        };
        assert_eq!(status, expected, "{kind:?}");
    }
}

/// The deadline `execute_script` arms is the operator's, and no Lua-level
/// budget or runner timeout is involved in stopping the guest.
#[tokio::test]
async fn a_spinning_script_is_interrupted_at_the_wall_clock_and_classifies_as_timeout() {
    common::require_db!();
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    let app = TestApp::new().await;
    let id = interpreter(&app).await;
    app.state.script_limits.set_wall_clock_seconds(1);

    let started = std::time::Instant::now();
    let err = app
        .state
        .plugin_executor()
        .execute_script(id, &input("spin", ScriptKind::XrpcQuery))
        .await
        .expect_err("a spinning guest was allowed to run");
    let elapsed = started.elapsed();
    assert!(matches!(err, ExecutionError::Timeout), "{err}");
    assert!(
        matches!(err.script_error_type(), ScriptErrorType::Timeout),
        "{err}"
    );
    assert!(
        elapsed >= std::time::Duration::from_secs(1) && elapsed < std::time::Duration::from_secs(4),
        "{elapsed:?}"
    );
}

/// The job exemption, both arms, which nothing else pins. `host:job_wait`
/// cannot: the import wrapper stops the guest's clock for the whole sleep, so
/// its seconds are host time under either arm and the guest's own execution is
/// microseconds. Only guest CPU that outlasts the budget *and then returns*
/// tells the two apart — swap the arms, or arm unconditionally, and one half
/// of this fails.
///
/// The burn is sized to this machine, since no one count outlasts the budget
/// on every machine without costing seconds on some. The non-job arms prove
/// it outlasted the budget whatever it was sized to — a burn too short comes
/// back from them returned rather than timed out — and the floor below is
/// checked first only so a burn the guest never spent is reported as that
/// rather than as a deadline that stopped working.
#[tokio::test]
async fn only_a_job_run_outlives_the_wall_clock() {
    common::require_db!();
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    let app = TestApp::new().await;
    let id = interpreter(&app).await;
    app.state.script_limits.set_wall_clock_seconds(1);
    let burn = common::burn::calibrate(&app.state, id).await;
    let source = burn.source();

    let mut job_run = input(&source, ScriptKind::Job);
    job_run.context.job = Some(ScriptJob {
        id: seeded_job(&app).await,
    });
    let started = std::time::Instant::now();
    let (value, _) = returned(
        app.state
            .plugin_executor()
            .execute_script(id, &job_run)
            .await
            .expect("a job run must not be bounded by the wall clock"),
    );
    let burned = started.elapsed();
    assert_eq!(value["source"], source);
    burn.assert_outlasted(burned);

    for kind in [ScriptKind::XrpcQuery, ScriptKind::RecordEvent] {
        let err = app
            .state
            .plugin_executor()
            .execute_script(id, &input(&source, kind))
            .await
            .expect_err("a run that is not a job must be bounded by the wall clock");
        assert!(matches!(err, ExecutionError::Timeout), "{kind:?}: {err}");
    }
}

/// Running long is what a job is for, so the wall clock is lifted for one.
#[tokio::test]
async fn a_job_run_is_not_interrupted_by_the_wall_clock() {
    common::require_db!();
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    let app = TestApp::new().await;
    let id = interpreter(&app).await;
    let job = seeded_job(&app).await;
    app.state.script_limits.set_wall_clock_seconds(1);

    let mut sent = input("host:job_wait", ScriptKind::Job);
    sent.context.job = Some(ScriptJob { id: job });
    sent.input = json!({"seconds": 2.0});

    let started = std::time::Instant::now();
    let (value, _) = returned(
        app.state
            .plugin_executor()
            .execute_script(id, &sent)
            .await
            .expect("a job run should not be interrupted"),
    );
    assert!(value.get("error").is_none(), "{value}");
    assert!(started.elapsed() >= std::time::Duration::from_secs(2));
}

/// The Lua-level ceiling from the input becomes the store's, so a guest that
/// grows past it is a memory refusal rather than an anonymous trap.
#[tokio::test]
async fn a_memory_refusal_classifies_as_memory() {
    common::require_db!();
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    let app = TestApp::new().await;
    let id = interpreter(&app).await;

    let mut sent = input("grow:2000", ScriptKind::XrpcQuery);
    sent.limits.memory_bytes = 8 * 1024 * 1024;
    let err = app
        .state
        .plugin_executor()
        .execute_script(id, &sent)
        .await
        .expect_err("growing past the ceiling should fail");
    assert!(matches!(err, ExecutionError::MemoryLimit { .. }), "{err}");
    assert!(
        matches!(err.script_error_type(), ScriptErrorType::Memory),
        "{err}"
    );
}

#[tokio::test]
async fn script_log_writes_one_event_with_the_runs_trigger_and_caller() {
    common::require_db!();
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    let app = TestApp::new().await;
    let id = interpreter(&app).await;

    let mut sent = input("host:script_log", ScriptKind::XrpcQuery);
    sent.input = json!({"level": "warn", "message": "from the script", "fields": {"n": 1}});
    let (value, _) = returned(
        app.state
            .plugin_executor()
            .execute_script(id, &sent)
            .await
            .expect("execute should answer"),
    );
    assert!(value.get("error").is_none(), "{value}");

    let rows: Vec<(String, Option<String>, String)> = happyview::db::query_as(
        "SELECT subject, actor_did, detail FROM happyview_event_logs WHERE event_type = 'script.log'",
    )
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].0, "xrpc.query:app.test.q");
    assert_eq!(rows[0].1.as_deref(), Some("did:plc:runner"));
    let detail: Value = serde_json::from_str(&rows[0].2).unwrap();
    assert_eq!(detail["message"], "from the script");
    assert_eq!(detail["level"], "warn");
    assert_eq!(detail["fields"]["n"], 1);
    assert_eq!(detail["trigger"], "xrpc.query:app.test.q");
}

#[tokio::test]
async fn a_job_runs_log_line_also_lands_in_the_jobs_own_log() {
    common::require_db!();
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    let app = TestApp::new().await;
    let id = interpreter(&app).await;
    let job = seeded_job(&app).await;

    let mut sent = input("host:script_log", ScriptKind::Job);
    sent.context.job = Some(ScriptJob { id: job.clone() });
    sent.input = json!({"level": "info", "message": "halfway"});
    app.state
        .plugin_executor()
        .execute_script(id, &sent)
        .await
        .expect("execute should answer");

    let logs: Vec<(String, String)> = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT level, message FROM happyview_job_logs WHERE job_id = ?",
        app.state.db_backend,
    ))
    .bind(&job)
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    assert_eq!(logs, vec![("info".to_string(), "halfway".to_string())]);

    let (events,): (i64,) = happyview::db::query_as(
        "SELECT COUNT(*) FROM happyview_event_logs WHERE event_type = 'script.log'",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(events, 1);
}

#[tokio::test]
async fn job_progress_updates_the_row_and_should_stop_reads_both_stop_states() {
    common::require_db!();
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    let app = TestApp::new().await;
    let id = interpreter(&app).await;
    let job = seeded_job(&app).await;
    let executor = app.state.plugin_executor();

    let mut sent = input("host:job_progress", ScriptKind::Job);
    sent.context.job = Some(ScriptJob { id: job.clone() });
    sent.input = json!({"processed": 42});
    let (value, _) = returned(executor.execute_script(id, &sent).await.unwrap());
    assert!(value.get("error").is_none(), "{value}");

    let (stored,): (String,) = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT progress FROM happyview_jobs WHERE id = ?",
        app.state.db_backend,
    ))
    .bind(&job)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&stored).unwrap(),
        json!({"processed": 42})
    );

    let mut sent = input("host:job_should_stop", ScriptKind::Job);
    sent.context.job = Some(ScriptJob { id: job.clone() });
    let (value, _) = returned(executor.execute_script(id, &sent).await.unwrap());
    assert_eq!(value["ok"], false, "{value}");

    for status in ["cancelling", "pausing"] {
        set_job_status(&app, &job, status).await;
        let (value, _) = returned(executor.execute_script(id, &sent).await.unwrap());
        assert_eq!(value["ok"], true, "{status}: {value}");
    }
}

/// A wait outside the range is clamped rather than refused, and its latency
/// is host time: the deadline armed for the run is still intact afterwards.
#[tokio::test]
async fn job_wait_clamps_its_range_and_is_not_charged_to_the_deadline() {
    common::require_db!();
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    let app = TestApp::new().await;
    let id = interpreter(&app).await;
    let job = seeded_job(&app).await;
    let executor = app.state.plugin_executor();

    let mut sent = input("host:job_wait", ScriptKind::XrpcQuery);
    sent.context.job = Some(ScriptJob { id: job.clone() });
    sent.input = json!({"seconds": -5.0});
    let started = std::time::Instant::now();
    let (value, _) = returned(executor.execute_script(id, &sent).await.unwrap());
    assert!(value.get("error").is_none(), "{value}");
    assert!(started.elapsed() < std::time::Duration::from_secs(1));

    // One second of wall clock, two seconds of waiting, and the run still
    // returns: the wait is the host's time, not the guest's.
    app.state.script_limits.set_wall_clock_seconds(1);
    sent.input = json!({"seconds": 2.0});
    let started = std::time::Instant::now();
    let (value, _) = returned(executor.execute_script(id, &sent).await.unwrap());
    assert!(value.get("error").is_none(), "{value}");
    assert!(started.elapsed() >= std::time::Duration::from_secs(2));
}

/// The three job controls act on the run's job and nothing else, so a run
/// without one is refused rather than given a job to guess at.
#[tokio::test]
async fn a_job_control_outside_a_job_run_is_refused() {
    common::require_db!();
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    let app = TestApp::new().await;
    let id = interpreter(&app).await;

    for source in ["host:job_progress", "host:job_should_stop", "host:job_wait"] {
        let (value, _) = returned(
            app.state
                .plugin_executor()
                .execute_script(id, &input(source, ScriptKind::XrpcQuery))
                .await
                .unwrap(),
        );
        assert_eq!(value["error"]["code"], "BAD_INPUT", "{source}: {value}");
    }
}

/// An interpreter reaches a library the same way a library reaches one: the
/// depth starts at zero and the call context carries the run's identity.
#[tokio::test]
async fn an_interpreter_reaches_a_library_through_the_same_two_imports() {
    common::require_db!();
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    require_fixture!("test_library", "wasm32-unknown-unknown");
    let app = TestApp::new().await;
    let id = interpreter(&app).await;
    app.state
        .plugin_registry
        .register(library_plugin("testlib"))
        .await;

    let mut sent = input("host:library", ScriptKind::XrpcQuery);
    sent.input = json!({"library": "testlib", "function": "whoami", "args": []});
    let (value, _) = returned(
        app.state
            .plugin_executor()
            .execute_script(id, &sent)
            .await
            .expect("execute should answer"),
    );
    assert_eq!(value["surface"]["ok"]["namespace"], "testlib", "{value}");
    assert_eq!(value["call"]["ok"], "did:plc:runner", "{value}");
}

#[tokio::test]
async fn a_library_plugin_passed_to_execute_script_is_refused_by_type() {
    common::require_db!();
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .register(plugin("echo_as_library", "library", &["script:host"]))
        .await;

    let err = app
        .state
        .plugin_executor()
        .execute_script("echo_as_library", &input("echo", ScriptKind::XrpcQuery))
        .await
        .expect_err("a library should not be executable as an interpreter");
    assert!(
        matches!(err, ExecutionError::NotAnInterpreter(ref id) if id == "echo_as_library"),
        "{err}"
    );

    let err = app
        .state
        .plugin_executor()
        .validate_script("echo_as_library", "anything")
        .await
        .expect_err("a library should not be validatable as an interpreter");
    assert!(matches!(err, ExecutionError::NotAnInterpreter(_)), "{err}");
}

#[tokio::test]
async fn validate_answers_both_shapes() {
    common::require_db!();
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    let app = TestApp::new().await;
    let id = interpreter(&app).await;
    let executor = app.state.plugin_executor();

    let valid = executor.validate_script(id, "return 1").await.unwrap();
    assert!(valid.valid);
    assert!(valid.errors.is_empty());

    let invalid = executor
        .validate_script(id, "invalid syntax")
        .await
        .unwrap();
    assert!(!invalid.valid);
    assert_eq!(invalid.errors.len(), 1);
    assert_eq!(invalid.errors[0].kind, ScriptErrorKind::Syntax);
    assert_eq!(invalid.errors[0].line, Some(1));

    let handleless = executor
        .validate_script(id, "no-handle here")
        .await
        .unwrap();
    assert!(!handleless.valid);
    assert_eq!(handleless.errors[0].kind, ScriptErrorKind::MissingHandle);
}

/// Validation reads no run, so the four `script:host` imports refuse inside
/// it and nothing it does reaches the event log. The fixture reports what the
/// import answered, which is what makes an input threaded through from the
/// validate path visible here rather than silent.
#[tokio::test]
async fn validate_reaches_no_script_run_and_writes_no_log_line() {
    common::require_db!();
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    let app = TestApp::new().await;
    let id = interpreter(&app).await;

    let out = app
        .state
        .plugin_executor()
        .validate_script(id, "probe-log")
        .await
        .expect("validate should answer");
    assert!(!out.valid);
    assert_eq!(out.errors[0].message, "UNSUPPORTED", "{:?}", out.errors);

    let (events,): (i64,) = happyview::db::query_as(
        "SELECT COUNT(*) FROM happyview_event_logs WHERE event_type = 'script.log'",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(events, 0);
}

/// A run is handed the host's guard list, not the caller's, so a runner that
/// sent a short one or none at all cannot weaken the guard. The guest echoes
/// what it received, so this fails if the replacement is dropped.
#[tokio::test]
async fn a_run_is_handed_the_hosts_removed_globals_whatever_the_caller_sent() {
    common::require_db!();
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    let app = TestApp::new().await;
    let id = interpreter(&app).await;
    let expected: Vec<&str> = happyview::codemod::REMOVED_GLOBALS.to_vec();

    let mut sent = input("echo", ScriptKind::XrpcQuery);
    sent.removed_globals = vec!["not-a-global".into()];
    let (value, _) = returned(
        app.state
            .plugin_executor()
            .execute_script(id, &sent)
            .await
            .expect("execute should answer"),
    );
    let delivered: Vec<&str> = value["removed_globals"]
        .as_array()
        .expect("removed_globals should be an array")
        .iter()
        .map(|name| name.as_str().expect("a name"))
        .collect();
    assert_eq!(delivered, expected, "the caller's list was honoured");

    // An absent list is filled the same way, which is the case a new runner
    // gets wrong by omission rather than by substitution.
    let mut empty = input("echo", ScriptKind::Label);
    empty.removed_globals = Vec::new();
    let (value, _) = returned(
        app.state
            .plugin_executor()
            .execute_script(id, &empty)
            .await
            .expect("execute should answer"),
    );
    assert_eq!(
        value["removed_globals"].as_array().unwrap().len(),
        expected.len()
    );
}

/// Validation loads the chunk under the same guard a run does, so it is
/// handed the host's removed-globals list. Delete the field from
/// `validate_script` and the fixture reports an empty guard, which this
/// fails on.
#[tokio::test]
async fn validate_is_handed_the_hosts_removed_globals() {
    common::require_db!();
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    let app = TestApp::new().await;
    let id = interpreter(&app).await;

    let out = app
        .state
        .plugin_executor()
        .validate_script(id, "probe-globals")
        .await
        .expect("validate should answer");
    let delivered: Vec<&str> = out.errors[0].message.split(',').collect();
    assert_eq!(
        delivered,
        happyview::codemod::REMOVED_GLOBALS.to_vec(),
        "validate saw a different guard list than the codemod's"
    );
}

#[tokio::test]
async fn an_unknown_interpreter_is_not_found() {
    common::require_db!();
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    let app = TestApp::new().await;
    let err = app
        .state
        .plugin_executor()
        .execute_script(
            &format!("absent-{}", Uuid::new_v4().simple()),
            &input("echo", ScriptKind::XrpcQuery),
        )
        .await
        .expect_err("an unregistered id should not execute");
    assert!(matches!(err, ExecutionError::PluginNotFound(_)), "{err}");
}

/// Nothing here needs a session, but the field exists so a procedure run can
/// lend one; a run without one still reaches the library imports.
#[tokio::test]
async fn execute_script_as_accepts_an_absent_session() {
    common::require_db!();
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    let app = TestApp::new().await;
    let id = interpreter(&app).await;
    let caller: Option<Arc<happyview::plugin::caller::CallerSession>> = None;
    let output = app
        .state
        .plugin_executor()
        .execute_script_as(id, &input("echo", ScriptKind::Label), caller)
        .await
        .expect("execute should answer");
    let (value, _) = returned(output);
    assert_eq!(value["kind"], "label");
}
