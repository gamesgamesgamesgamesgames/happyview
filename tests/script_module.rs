//! A real `happyview-script` module, read and run by the host: the sections
//! the linker actually produced, the capabilities its imports actually need,
//! and handlers run through the same execute path an interpreter takes.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use happyview::config::project::ModuleDecl;
use happyview::lexicon::{ParsedLexicon, ProcedureAction};
use happyview::plugin::capabilities::{PluginCapability, analyze_imports, minimal_set};
use happyview::plugin::{
    ExecutionError, ScriptErrorKind, ScriptExecuteContext, ScriptExecuteInput, ScriptExecuteLimits,
    ScriptExecuteOutput, ScriptKind, ScriptLibraryRef, ScriptValueKind, loader,
};
use happyview::project::script_module::sections::{ModuleHandler, read_sections};
use happyview::project::script_module::{ScriptModule, load_modules};
use happyview::project::snapshot::ProjectLexicon;
use happyview::test_support::{migrated_memory_pool, test_state_with_pool};

const MODULE: &str = "tests/fixtures/script_module/target/wasm32-wasip1/release/script_module.wasm";
const SOURCE: &str = "tests/fixtures/script_module/src/lib.rs";

macro_rules! require_fixture {
    ($path:expr, $build:literal) => {
        if !std::path::Path::new($path).exists() {
            happyview::test_support::unavailable(concat!("fixture not built. Run: ", $build));
            return;
        }
    };
}

macro_rules! require_module {
    () => {
        require_fixture!(
            MODULE,
            "cargo build --manifest-path tests/fixtures/script_module/Cargo.toml --target wasm32-wasip1 --release"
        )
    };
}

/// What the module's imports need: the two concurrent-call imports are
/// `library:call`, and std's panic machinery links `fd_write`.
fn capabilities() -> Vec<PluginCapability> {
    vec![PluginCapability::LibraryCall, PluginCapability::WasiStdio]
}

fn lexicon(id: &str, kind: &str) -> (String, ProjectLexicon) {
    let main = match kind {
        "record" => {
            json!({"type": "record", "key": "tid", "record": {"type": "object", "properties": {}}})
        }
        other => json!({"type": other}),
    };
    let parsed = ParsedLexicon::parse(
        json!({"lexicon": 1, "id": id, "defs": {"main": main}}),
        1,
        None,
        ProcedureAction::Upsert,
        None,
    )
    .unwrap();
    (
        id.into(),
        ProjectLexicon {
            parsed,
            path: format!("src/lexicons/{id}.json").into(),
            backfill: false,
            indexed: true,
        },
    )
}

/// The fixture loaded the way a snapshot build loads it.
fn module(state: &happyview::AppState) -> Arc<ScriptModule> {
    let dir = tempfile::tempdir().unwrap();
    let build = dir.path().join(".happyview/build");
    std::fs::create_dir_all(&build).unwrap();
    std::fs::copy(MODULE, build.join("fixture.wasm")).unwrap();
    let decls = BTreeMap::from([(
        "fixture".to_string(),
        ModuleDecl {
            language: "rust".into(),
            path: "tests/fixtures/script_module".into(),
            capabilities: capabilities(),
        },
    )]);
    let lexicons = [
        lexicon("app.test.echo", "query"),
        lexicon("app.test.pair", "query"),
        lexicon("app.test.boom", "query"),
        lexicon("app.test.spin", "query"),
        lexicon("app.test.thing", "record"),
        lexicon("app.test.calibration.shape", "query"),
        lexicon("app.test.calibration.aggregate", "query"),
        lexicon("app.test.calibration.render", "query"),
    ]
    .into_iter()
    .collect();
    let loaded = load_modules(
        &state.wasm_runtime,
        dir.path(),
        &decls,
        &lexicons,
        &BTreeMap::new(),
    )
    .unwrap_or_else(|d| panic!("{d:?}"));
    loaded.modules.into_values().next().unwrap()
}

fn input(trigger: &str, value: Value, instructions: Option<u32>) -> ScriptExecuteInput {
    ScriptExecuteInput {
        source: String::new(),
        kind: ScriptKind::XrpcQuery,
        input: value,
        context: ScriptExecuteContext {
            trigger: trigger.into(),
            caller_did: Some("did:plc:caller".into()),
            ..ScriptExecuteContext::default()
        },
        libraries: vec![ScriptLibraryRef {
            namespace: "http".into(),
            id: "sdk_http".into(),
        }],
        limits: ScriptExecuteLimits {
            instructions,
            memory_bytes: 64 * 1024 * 1024,
        },
        removed_globals: Vec::new(),
        chunk_name: happyview::plugin::DEFAULT_CHUNK_NAME.into(),
        artifact: None,
    }
}

async fn run(
    state: &happyview::AppState,
    module: &ScriptModule,
    input: &ScriptExecuteInput,
) -> Result<ScriptExecuteOutput, ExecutionError> {
    state
        .plugin_executor()
        .execute_compiled_as(module.plugin.clone(), &module.source_path, input, None)
        .await
}

async fn state() -> happyview::AppState {
    test_state_with_pool(migrated_memory_pool().await)
}

#[test]
fn the_linker_concatenates_every_handler_line_into_one_readable_section() {
    require_module!();
    let sections = read_sections(&std::fs::read(MODULE).unwrap()).unwrap();
    let mut handlers = sections.handlers.clone();
    handlers.sort_by(|a, b| a.trigger.cmp(&b.trigger));
    let expected: Vec<ModuleHandler> = [
        ("record.index:app.test.thing", "index_thing"),
        ("xrpc.query:app.test.boom", "boom"),
        (
            "xrpc.query:app.test.calibration.aggregate",
            "calibration_aggregate",
        ),
        (
            "xrpc.query:app.test.calibration.render",
            "calibration_render",
        ),
        ("xrpc.query:app.test.calibration.shape", "calibration_shape"),
        ("xrpc.query:app.test.echo", "echo"),
        ("xrpc.query:app.test.pair", "pair"),
        ("xrpc.query:app.test.spin", "spin"),
    ]
    .into_iter()
    .map(|(trigger, function)| ModuleHandler {
        trigger: trigger.into(),
        function: Some(function.into()),
    })
    .collect();
    assert_eq!(handlers, expected);
    assert_eq!(sections.interface.as_deref(), Some("1"));
    assert!(
        !sections.uses_exceptions,
        "a stable wasm32-wasip1 build must not use exception handling"
    );
}

#[test]
fn its_imports_need_exactly_library_call_and_stdio() {
    require_module!();
    let mut needed = minimal_set(&analyze_imports(&std::fs::read(MODULE).unwrap()).unwrap());
    needed.sort_by_key(|c| c.as_str());
    assert_eq!(needed, capabilities());
}

fn manifest() -> happyview::plugin::PluginManifest {
    serde_json::from_value(json!({
        "id": "module:fixture", "name": "fixture", "version": "0.0.0", "api_version": "2",
        "plugin_type": "interpreter", "language_id": "rust",
        "capabilities": capabilities(),
    }))
    .unwrap()
}

#[test]
fn dropping_a_capability_it_needs_fails_the_build() {
    require_module!();
    let wasm = std::fs::read(MODULE).unwrap();
    // Without this, a manifest refused for some other reason would satisfy
    // every drop below.
    loader::validate_capabilities(&manifest(), &wasm).unwrap();
    for dropped in capabilities() {
        let mut manifest = manifest();
        manifest.capabilities.retain(|c| *c != dropped);
        let err = loader::validate_capabilities(&manifest, &wasm).unwrap_err();
        assert!(
            err.to_string().contains(dropped.as_str()),
            "{dropped:?}: {err}"
        );
    }
}

#[tokio::test]
async fn a_query_runs_through_the_execute_path() {
    require_module!();
    let state = state().await;
    let module = module(&state);
    let output = run(
        &state,
        &module,
        &input(
            "xrpc.query:app.test.echo",
            json!({"q": "hi"}),
            Some(1_000_000),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        output,
        ScriptExecuteOutput::Returned {
            value: json!({"q": "hi", "trigger": "xrpc.query:app.test.echo", "caller": "did:plc:caller"}),
            value_kind: ScriptValueKind::Object,
        }
    );
}

#[tokio::test]
async fn a_record_handler_returning_nothing_reads_as_none() {
    require_module!();
    let state = state().await;
    let module = module(&state);
    let mut event = input("record.index:app.test.thing", json!({}), Some(1_000_000));
    event.kind = ScriptKind::RecordEvent;
    let output = run(&state, &module, &event).await.unwrap();
    assert_eq!(
        output,
        ScriptExecuteOutput::Returned {
            value: Value::Null,
            value_kind: ScriptValueKind::None
        }
    );
}

#[tokio::test]
async fn a_panic_is_a_runtime_error_at_the_authors_line() {
    require_module!();
    let source = std::fs::read_to_string(SOURCE).unwrap();
    let lines: Vec<u32> = source
        .lines()
        .enumerate()
        .filter(|(_, l)| l.trim() == "panic!(\"the fixture panicked on purpose\")")
        .map(|(i, _)| i as u32 + 1)
        .collect();
    let [line] = lines[..] else {
        panic!("expected exactly one panic! line in {SOURCE}, found {lines:?}");
    };
    let state = state().await;
    let module = module(&state);
    let output = run(
        &state,
        &module,
        &input("xrpc.query:app.test.boom", json!({}), Some(1_000_000)),
    )
    .await
    .unwrap();
    let ScriptExecuteOutput::Error {
        kind,
        message,
        line: at,
        chunk,
        ..
    } = output
    else {
        panic!("expected an error, got {output:?}");
    };
    assert_eq!(kind, ScriptErrorKind::Runtime);
    assert_eq!(chunk.as_deref(), Some(SOURCE));
    assert_eq!(at, Some(line));
    // The host prints the location beside the message, so the message
    // carries none of its own.
    assert_eq!(message, "the fixture panicked on purpose");
}

#[tokio::test]
async fn spending_the_instruction_budget_is_a_timeout_not_a_crash() {
    require_module!();
    let state = state().await;
    let module = module(&state);
    let started = Instant::now();
    let err = run(
        &state,
        &module,
        &input("xrpc.query:app.test.spin", json!({}), Some(1_000)),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ExecutionError::Timeout), "{err}");
    // Fuel, not the ten-second wall clock, ended it.
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_trivial_handler_fits_a_tenth_of_the_default_budget_and_not_one_instruction() {
    require_module!();
    let state = state().await;
    let module = module(&state);
    // The echo handler's whole run, decoding its input included, has to fit
    // in a tenth of the default budget.
    let output = run(
        &state,
        &module,
        &input(
            "xrpc.query:app.test.echo",
            json!({"q": "hi"}),
            Some(100_000),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        output,
        ScriptExecuteOutput::Returned {
            value: json!({"q": "hi", "trigger": "xrpc.query:app.test.echo", "caller": "did:plc:caller"}),
            value_kind: ScriptValueKind::Object,
        }
    );
    // Without this, a budget that never reached the guest would pass the
    // check above.
    let err = run(
        &state,
        &module,
        &input("xrpc.query:app.test.echo", json!({"q": "hi"}), Some(1)),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ExecutionError::Timeout), "{err}");
}

#[tokio::test]
async fn an_unknown_trigger_is_a_missing_handler() {
    require_module!();
    let state = state().await;
    let module = module(&state);
    let output = run(
        &state,
        &module,
        &input("xrpc.query:app.test.other", json!({}), Some(1_000_000)),
    )
    .await
    .unwrap();
    let ScriptExecuteOutput::Error {
        kind: ScriptErrorKind::MissingHandle,
        message,
        ..
    } = &output
    else {
        panic!("expected a missing handler, got {output:?}");
    };
    assert!(message.contains("xrpc.query:app.test.other"), "{message}");
}

/// `/slow?ms=N` holds the request for `N` ms and counts how many are in
/// flight at once.
async fn slow_server() -> (String, Arc<AtomicUsize>) {
    use axum::extract::{Query, State};
    use axum::routing::get;

    #[derive(serde::Deserialize)]
    struct Slow {
        ms: u64,
    }
    #[derive(Default)]
    struct Seen {
        now: AtomicUsize,
        peak: Arc<AtomicUsize>,
    }

    async fn slow(State(seen): State<Arc<Seen>>, Query(q): Query<Slow>) -> &'static str {
        let now = seen.now.fetch_add(1, Ordering::SeqCst) + 1;
        seen.peak.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(q.ms)).await;
        seen.now.fetch_sub(1, Ordering::SeqCst);
        "slow"
    }

    let seen = Arc::new(Seen::default());
    let peak = seen.peak.clone();
    let app = axum::Router::new()
        .route("/slow", get(slow))
        .with_state(seen);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, peak)
}

#[tokio::test]
async fn joined_library_calls_overlap() {
    require_module!();
    require_fixture!(
        "tests/fixtures/sdk_http/target/wasm32-unknown-unknown/release/sdk_http.wasm",
        "cargo build --manifest-path tests/fixtures/sdk_http/Cargo.toml --target wasm32-unknown-unknown --release"
    );
    let (base, peak) = slow_server().await;
    let state = state().await;
    let http = loader::load_from_file(std::path::Path::new("tests/fixtures/sdk_http"))
        .await
        .unwrap();
    state.plugin_registry.install(http).await.unwrap();
    let module = module(&state);

    let output = run(
        &state,
        &module,
        &input(
            "xrpc.query:app.test.pair",
            json!({"url": format!("{base}/slow?ms=300")}),
            None,
        ),
    )
    .await
    .unwrap();

    let ScriptExecuteOutput::Returned { value, .. } = output else {
        panic!("{output:?}")
    };
    assert_eq!(value[0]["body"], "slow", "{value}");
    assert_eq!(value[1]["body"], "slow", "{value}");
    // Each request is held for 300 ms, so two in flight at once is overlap.
    assert_eq!(peak.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_query_reaches_the_module_through_the_xrpc_router() {
    require_module!();
    use tower::ServiceExt;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("happyview.toml"),
        "[modules.fixture]\nlanguage = \"rust\"\npath = \"modules/fixture\"\ncapabilities = [\"library:call\", \"wasi:stdio\"]\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src/lexicons/app/test")).unwrap();
    for id in ["echo", "pair", "boom", "spin"] {
        // `q` is declared so parameter validation passes it through.
        let main = json!({"type": "query", "parameters": {"type": "params", "properties": {
            "q": {"type": "string"}, "url": {"type": "string"}
        }}});
        std::fs::write(
            root.join(format!("src/lexicons/app/test/{id}.json")),
            json!({"lexicon": 1, "id": format!("app.test.{id}"), "defs": {"main": main}})
                .to_string(),
        )
        .unwrap();
    }
    std::fs::create_dir_all(root.join("src/lexicons/app/test/calibration")).unwrap();
    for id in ["shape", "aggregate", "render"] {
        std::fs::write(
            root.join(format!("src/lexicons/app/test/calibration/{id}.json")),
            json!({"lexicon": 1, "id": format!("app.test.calibration.{id}"), "defs": {"main": {"type": "query"}}})
                .to_string(),
        )
        .unwrap();
    }
    std::fs::write(
        root.join("src/lexicons/app/test/thing.json"),
        json!({"lexicon": 1, "id": "app.test.thing", "defs": {"main": {"type": "record", "key": "tid", "record": {"type": "object", "properties": {}}}}}).to_string(),
    )
    .unwrap();
    std::fs::create_dir_all(root.join(".happyview/build")).unwrap();
    std::fs::copy(MODULE, root.join(".happyview/build/fixture.wasm")).unwrap();

    let state = state().await;
    let report = happyview::project::build::build_snapshot(
        &state,
        root,
        happyview::project::build::BuildOptions {
            include_tests: false,
        },
    )
    .await
    .unwrap_or_else(|d| panic!("{d:?}"));
    happyview::project::build::install(&state, report.snapshot).await;

    // The router answers only hosts it serves; the bare test state serves none.
    let now = happyview::db::now_rfc3339();
    state
        .domain_cache
        .insert(happyview::domain::Domain {
            id: "test".into(),
            url: "http://127.0.0.1".into(),
            is_primary: true,
            created_at: now.clone(),
            updated_at: now,
        })
        .await;

    let router = happyview::server::router(state.clone());
    let response = router
        .oneshot(
            axum::http::Request::get("/xrpc/app.test.echo?q=hi")
                .header("host", "127.0.0.1")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["q"], "hi");
    assert_eq!(body["trigger"], "xrpc.query:app.test.echo");
}
