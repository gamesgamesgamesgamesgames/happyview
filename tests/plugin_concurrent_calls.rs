//! `host_call_library_start` and `host_call_library_wait_any` through real
//! modules: the `interpreter_echo` fixture's `host:calls` directive drives the
//! two imports, and the SDK-built `http` fixture is the library it calls, at a
//! local server that is slow on purpose and counts what it sees.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use happyview::plugin::library::MAX_CONCURRENT_LIBRARY_CALLS;
use happyview::plugin::{
    ExecutionError, LoadedPlugin, PluginManifest, PluginSource, ScriptExecuteContext,
    ScriptExecuteInput, ScriptExecuteLimits, ScriptExecuteOutput, ScriptKind, loader,
};
use happyview::test_support::{migrated_memory_pool, test_state_with_pool};

const ECHO: &str =
    "tests/fixtures/interpreter_echo/target/wasm32-unknown-unknown/release/interpreter_echo.wasm";

/// Skips the caller when a fixture's module is absent, naming the build that
/// produces it.
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

/// What the server has seen: requests in flight now, the most ever in flight
/// at once, requests that ran to the end, and hits on `/mark`.
#[derive(Default)]
struct Seen {
    in_flight: AtomicUsize,
    peak: AtomicUsize,
    completed: AtomicUsize,
    marks: AtomicUsize,
}

/// `/slow?ms=N` holds the request for `N` ms; `/mark` answers at once. Both
/// are counted.
async fn server() -> (String, Arc<Seen>) {
    use axum::extract::{Query, State};
    use axum::routing::get;

    #[derive(serde::Deserialize)]
    struct Slow {
        ms: u64,
    }

    async fn slow(State(seen): State<Arc<Seen>>, Query(q): Query<Slow>) -> &'static str {
        let now = seen.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        seen.peak.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(q.ms)).await;
        seen.in_flight.fetch_sub(1, Ordering::SeqCst);
        seen.completed.fetch_add(1, Ordering::SeqCst);
        "slow"
    }

    async fn mark(State(seen): State<Arc<Seen>>) -> &'static str {
        seen.marks.fetch_add(1, Ordering::SeqCst);
        "mark"
    }

    let seen = Arc::new(Seen::default());
    let app = axum::Router::new()
        .route("/slow", get(slow))
        .route("/mark", get(mark))
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, seen)
}

fn echo() -> LoadedPlugin {
    let manifest: PluginManifest = serde_json::from_value(json!({
        "id": "echo", "name": "echo", "version": "1.0.0", "api_version": "2",
        "plugin_type": "interpreter", "language_id": "echo",
        "capabilities": ["library:call", "script:host"],
    }))
    .unwrap();
    LoadedPlugin {
        info: manifest.clone().into(),
        source: PluginSource::File {
            path: "tests/fixtures/interpreter_echo".into(),
        },
        wasm_bytes: std::fs::read(ECHO).expect("the echo module should read"),
        manifest: Some(manifest),
    }
}

async fn state() -> happyview::AppState {
    let state = test_state_with_pool(migrated_memory_pool().await);
    state.plugin_registry.register(echo()).await;
    let http = loader::load_from_file(std::path::Path::new("tests/fixtures/sdk_http"))
        .await
        .expect("the http module should load");
    state.plugin_registry.install(http).await.unwrap();
    state
}

fn input(steps: Value) -> ScriptExecuteInput {
    ScriptExecuteInput {
        source: "host:calls".into(),
        kind: ScriptKind::XrpcQuery,
        input: json!({"steps": steps}),
        context: ScriptExecuteContext {
            trigger: "xrpc.query:app.test.q".into(),
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

async fn run(state: &happyview::AppState, steps: Value) -> Result<Vec<Value>, ExecutionError> {
    let out = tokio::time::timeout(
        Duration::from_secs(20),
        state
            .plugin_executor()
            .execute_script("echo", &input(steps)),
    )
    .await
    .expect("the run hung")?;
    match out {
        ScriptExecuteOutput::Returned { value, .. } => Ok(value.as_array().unwrap().clone()),
        other => panic!("expected a returned value, got {other:?}"),
    }
}

fn get(url: String) -> Value {
    json!({"start": {"library": "sdk_http", "function": "get", "args": [url]}})
}

/// Compile both modules before anything is timed.
async fn warm(state: &happyview::AppState, base: &str) {
    run(
        state,
        json!([get(format!("{base}/slow?ms=0")), {"wait_all": true}]),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn two_slow_calls_overlap() {
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    require_fixture!("sdk_http", "wasm32-unknown-unknown");
    let (base, seen) = server().await;
    let state = state().await;
    warm(&state, &base).await;

    let started = Instant::now();
    let answers = run(
        &state,
        json!([
            get(format!("{base}/slow?ms=300")),
            get(format!("{base}/slow?ms=300")),
            {"wait_all": true},
        ]),
    )
    .await
    .unwrap();
    let elapsed = started.elapsed();

    assert_eq!(answers[0], json!({"handle": 1}));
    assert_eq!(answers[1], json!({"handle": 2}));
    let settled = answers[2].as_array().unwrap();
    assert_eq!(settled.len(), 2, "{answers:?}");
    let mut handles: Vec<i64> = settled
        .iter()
        .map(|s| {
            assert_eq!(s["ok"]["status"], 200, "{s}");
            assert_eq!(s["ok"]["body"], "slow", "{s}");
            s["handle"].as_i64().unwrap()
        })
        .collect();
    handles.sort();
    assert_eq!(handles, [1, 2]);
    // One after the other is at least 600 ms.
    assert!(elapsed < Duration::from_millis(550), "{elapsed:?}");
    assert_eq!(seen.peak.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn no_more_than_eight_run_at_once() {
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    require_fixture!("sdk_http", "wasm32-unknown-unknown");
    let (base, seen) = server().await;
    let state = state().await;
    warm(&state, &base).await;

    let mut steps: Vec<Value> = (0..12)
        .map(|_| get(format!("{base}/slow?ms=400")))
        .collect();
    steps.push(json!({"wait_all": true}));
    let answers = run(&state, Value::Array(steps)).await.unwrap();

    // Every handle came back at once, the queued four included.
    let handles: Vec<i64> = answers[..12]
        .iter()
        .map(|a| a["handle"].as_i64().unwrap())
        .collect();
    assert_eq!(handles, (1..=12).collect::<Vec<_>>());
    let settled = answers[12].as_array().unwrap();
    assert_eq!(settled.len(), 12);
    assert!(
        settled.iter().all(|s| s["ok"]["status"] == 200),
        "{settled:?}"
    );
    assert_eq!(
        seen.peak.load(Ordering::SeqCst),
        MAX_CONCURRENT_LIBRARY_CALLS
    );
    assert_eq!(seen.completed.load(Ordering::SeqCst), 13);
}

/// A failure comes back through the handle as exactly the envelope
/// `host_call_library` answers, and `start` itself still hands out a handle.
#[tokio::test]
async fn a_failure_comes_back_through_its_handle() {
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    require_fixture!("sdk_http", "wasm32-unknown-unknown");
    let state = state().await;

    let answers = run(
        &state,
        json!([
            // The callee's own error: `get` needs a url.
            {"start": {"library": "sdk_http", "function": "get", "args": []}},
            {"wait": [1]},
            // The host's refusals, before any library runs.
            {"start": {"library": "sdk_http", "function": "get", "args_raw": "{}"}},
            {"wait": [2]},
            {"start": {"library": "nope", "function": "get", "args": []}},
            {"wait": [3]},
        ]),
    )
    .await
    .unwrap();

    assert_eq!(answers[0], json!({"handle": 1}));
    assert_eq!(answers[1]["handle"], 1);
    assert_eq!(answers[1]["error"]["code"], "BAD_INPUT", "{}", answers[1]);
    assert_eq!(answers[2], json!({"handle": 2}));
    assert_eq!(answers[3]["handle"], 2);
    assert_eq!(answers[3]["error"]["code"], "BAD_INPUT", "{}", answers[3]);
    assert!(
        answers[3]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("JSON array"),
        "{}",
        answers[3]
    );
    assert_eq!(answers[4], json!({"handle": 3}));
    assert_eq!(answers[5]["handle"], 3);
    assert_eq!(
        answers[5]["error"]["code"], "LIBRARY_ERROR",
        "{}",
        answers[5]
    );

    // The same call through `host_call_library` answers the same envelope.
    let out = state
        .plugin_executor()
        .execute_script(
            "echo",
            &ScriptExecuteInput {
                source: "host:library".into(),
                input: json!({"library": "sdk_http", "function": "get", "args": []}),
                ..input(json!([]))
            },
        )
        .await
        .unwrap();
    let ScriptExecuteOutput::Returned { value, .. } = out else {
        panic!("expected a returned value, got {out:?}");
    };
    let mut through_handle = answers[1].clone();
    through_handle.as_object_mut().unwrap().remove("handle");
    assert_eq!(value["call"], through_handle);
}

/// A wait that could never return answers `BAD_INPUT` at once, naming no
/// handle.
#[tokio::test]
async fn an_unanswerable_wait_is_refused_at_once() {
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    require_fixture!("sdk_http", "wasm32-unknown-unknown");
    let (base, _) = server().await;
    let state = state().await;

    let answers = run(
        &state,
        json!([
            {"wait": []},
            {"wait": [42]},
            {"wait_raw": "not json"},
            {"wait_raw": "{\"handle\": 1}"},
            get(format!("{base}/mark")),
            {"wait": [1]},
            {"wait": [1]},
        ]),
    )
    .await
    .unwrap();

    for refused in [
        &answers[0],
        &answers[1],
        &answers[2],
        &answers[3],
        &answers[6],
    ] {
        assert_eq!(refused["error"]["code"], "BAD_INPUT", "{refused}");
        assert!(refused.get("handle").is_none(), "{refused}");
    }
    assert!(
        answers[1]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("42")
    );
    assert_eq!(answers[5]["handle"], 1);
    assert_eq!(answers[5]["ok"]["body"], "mark");
}

/// A run that returns without waiting still lets its calls land, and the run
/// does not answer until they have.
#[tokio::test]
async fn a_returned_run_lets_its_calls_land() {
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    require_fixture!("sdk_http", "wasm32-unknown-unknown");
    let (base, seen) = server().await;
    let state = state().await;
    warm(&state, &base).await;
    let before = seen.completed.load(Ordering::SeqCst);

    let answers = run(&state, json!([get(format!("{base}/slow?ms=300"))]))
        .await
        .unwrap();
    assert_eq!(answers, [json!({"handle": 1})]);
    assert_eq!(seen.completed.load(Ordering::SeqCst), before + 1);
}

/// A run that traps takes its calls down with it: with eight calls holding
/// every slot, a ninth is still queued when the run ends, and never reaches
/// the server.
#[tokio::test]
async fn a_trapped_run_aborts_its_calls() {
    require_fixture!("interpreter_echo", "wasm32-unknown-unknown");
    require_fixture!("sdk_http", "wasm32-unknown-unknown");
    let (base, seen) = server().await;
    let state = state().await;
    warm(&state, &base).await;

    let mut steps: Vec<Value> = (0..MAX_CONCURRENT_LIBRARY_CALLS)
        .map(|_| get(format!("{base}/slow?ms=400")))
        .collect();
    steps.push(get(format!("{base}/mark")));
    steps.push(json!({"trap": true}));
    let err = run(&state, Value::Array(steps)).await.unwrap_err();
    assert!(matches!(err, ExecutionError::Trap(_)), "{err:?}");

    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(seen.marks.load(Ordering::SeqCst), 0);
}
