//! The test runner's imports from a real wasm guest: refused outside a test
//! run, refused without `script:test`, and acting on the file's harness
//! inside one.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use happyview::catalog::{LexiconWrite, ScriptWrite};
use happyview::lexicon::ProcedureAction;
use happyview::plugin::{
    ExecutionError, PluginRegistry, ScriptExecuteInput, ScriptExecuteOutput, TestStatus,
    WasmRuntime,
};
use happyview::testing::env::TestEnv;

use common::echo_interpreter;

async fn env() -> Option<TestEnv> {
    if !echo_interpreter::is_built() {
        eprintln!("skipping: {}", echo_interpreter::BUILD);
        return None;
    }
    let registry = Arc::new(PluginRegistry::new());
    registry
        .install(echo_interpreter::plugin("echo"))
        .await
        .unwrap();
    Some(
        TestEnv::new(registry, Arc::new(WasmRuntime::new().unwrap()), None)
            .await
            .unwrap(),
    )
}

/// Built from JSON so a field a later plan adds with a serde default does not
/// break this file.
fn input(kind: &str, steps: Value) -> ScriptExecuteInput {
    raw_input(kind, &format!("test:{steps}"))
}

fn raw_input(kind: &str, source: &str) -> ScriptExecuteInput {
    serde_json::from_value(json!({
        "source": source,
        "kind": kind,
        "input": null,
        "context": {"trigger": "test:src/lib/a.test.echo"},
        "limits": {"instructions": null, "memory_bytes": 67_108_864},
    }))
    .unwrap()
}

async fn answers(state: &happyview::AppState, input: &ScriptExecuteInput) -> Vec<Value> {
    match state
        .plugin_executor()
        .execute_script("interpreter_echo", input)
        .await
        .unwrap()
    {
        ScriptExecuteOutput::Returned { value, .. } => value["answers"].as_array().unwrap().clone(),
        other => panic!("expected a value, got {other:?}"),
    }
}

fn report_step(name: &str) -> Value {
    json!({"import": "report", "request": {"path": "", "name": name, "status": "pass", "duration_ms": 1}})
}

#[tokio::test]
async fn the_test_imports_refuse_a_run_that_is_not_a_test_file() {
    let Some(env) = env().await else { return };
    let file = env.file_state("src/lib/a.test.echo").await.unwrap();
    let got = answers(&file.state, &input("xrpc_query", json!([report_step("a")]))).await;
    assert_eq!(got[0]["error"]["code"], "UNSUPPORTED", "{got:?}");
    assert!(file.harness.reports().is_empty());
    file.finish().await;
}

#[tokio::test]
async fn the_test_imports_refuse_a_state_with_no_harness() {
    let Some(env) = env().await else { return };
    let got = answers(&env.base, &input("test", json!([report_step("a")]))).await;
    assert_eq!(got[0]["error"]["code"], "UNSUPPORTED", "{got:?}");
}

/// The gate as a handler meets it: a query script served on a test file's
/// own state, reached through the router, is a run whose kind is not `test`,
/// so the harness is there and still out of reach.
#[tokio::test]
async fn a_query_served_on_a_test_state_cannot_reach_the_harness() {
    let Some(env) = env().await else { return };
    let file = env.file_state("src/lib/a.test.echo").await.unwrap();
    let state = &file.state;
    state
        .lexicons
        .put_manual(
            &state.db,
            state.db_backend,
            LexiconWrite {
                lexicon_json: common::fixtures::list_games_query_lexicon(),
                backfill: false,
                target_collection: Some("games.gamesgamesgamesgames.game".into()),
                action: ProcedureAction::Upsert,
                token_cost: None,
                authority_did: None,
            },
        )
        .await
        .unwrap();
    state
        .scripts
        .put_manual(
            &state.db,
            state.db_backend,
            ScriptWrite {
                id: "xrpc.query:games.gamesgamesgamesgames.listGames".into(),
                script_type: "echo".into(),
                body: format!("test:{}", json!([report_step("a")])),
                description: None,
            },
        )
        .await
        .unwrap();

    let response = happyview::server::router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/xrpc/games.gamesgamesgamesgames.listGames")
                .header("host", happyview::testing::TEST_HOST)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    assert!(status.is_success(), "{status}: {body}");
    assert_eq!(body["answers"][0]["error"]["code"], "UNSUPPORTED", "{body}");
    assert!(file.harness.reports().is_empty());
    file.finish().await;
}

#[tokio::test]
async fn a_report_lands_on_the_files_harness() {
    let Some(env) = env().await else { return };
    let file = env.file_state("src/lib/a.test.echo").await.unwrap();
    let got = answers(
        &file.state,
        &input(
            "test",
            json!([
                report_step("first"),
                {"check": {"name": "second", "answer": 0, "pointer": "/ok", "equals": null, "line": 9}},
                {"import": "calls", "request": {"namespace": "http"}},
                {"import": "pds_writes", "request": {}},
            ]),
        ),
    )
    .await;
    assert_eq!(got[2], json!({"ok": []}));
    assert_eq!(got[3], json!({"ok": []}));
    let reports = file.harness.reports();
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].path, "src/lib/a.test.echo");
    assert_eq!(reports[1].name, "second");
    assert_eq!(reports[1].status, TestStatus::Pass);
    assert_eq!(reports[1].line, Some(9));
    file.finish().await;
}

#[tokio::test]
async fn the_test_imports_refuse_an_interpreter_without_script_test() {
    if !echo_interpreter::is_built() {
        eprintln!("skipping: {}", echo_interpreter::BUILD);
        return;
    }
    let mut plugin = echo_interpreter::plugin("echo");
    let manifest = plugin.manifest.as_mut().unwrap();
    manifest
        .capabilities
        .retain(|c| c.as_str() != "script:test");
    plugin.info = manifest.clone().into();
    let registry = Arc::new(PluginRegistry::new());
    registry.install(plugin).await.unwrap();
    let env = TestEnv::new(registry, Arc::new(WasmRuntime::new().unwrap()), None)
        .await
        .unwrap();
    let file = env.file_state("src/lib/a.test.echo").await.unwrap();
    let got = answers(&file.state, &input("test", json!([report_step("a")]))).await;
    assert_eq!(got[0]["error"]["code"], "FORBIDDEN", "{got:?}");
    file.finish().await;
}

/// A test file's own code is bounded, by its own budget rather than the
/// operator's wall clock. The wall clock is cut to a second so that arm would
/// stop the spin long before the file's budget runs out, and the outer
/// timeout turns a lifted deadline, the job arm, into a failure rather than a
/// hang.
#[tokio::test]
async fn a_spinning_test_file_times_out_at_its_own_budget() {
    let Some(env) = env().await else { return };
    let file = env.file_state("src/lib/a.test.echo").await.unwrap();
    file.state.script_limits.set_wall_clock_seconds(1);
    let budget = Duration::from_secs(happyview::plugin::executor::TEST_FILE_GUEST_SECONDS);

    let started = Instant::now();
    let err = tokio::time::timeout(
        budget + Duration::from_secs(30),
        file.state
            .plugin_executor()
            .execute_script("interpreter_echo", &raw_input("test", "spin")),
    )
    .await
    .expect("a spinning test file was never stopped")
    .expect_err("a spinning test file returned");
    let elapsed = started.elapsed();
    assert!(matches!(err, ExecutionError::Timeout), "{err}");
    assert!(
        elapsed >= budget && elapsed < budget + Duration::from_secs(5),
        "{elapsed:?}"
    );
    file.finish().await;
}

/// A library with no module: a mocked call never instantiates it, and any
/// call that does fails, which is what tells the two apart.
async fn install_library(registry: &PluginRegistry, id: &str, namespace: &str) {
    let manifest: happyview::plugin::PluginManifest = serde_json::from_value(json!({
        "id": id, "name": id, "version": "1.0.0", "api_version": "2",
        "plugin_type": "library", "namespace": namespace, "capabilities": [],
    }))
    .unwrap();
    registry
        .install(happyview::plugin::LoadedPlugin {
            info: manifest.clone().into(),
            source: happyview::plugin::PluginSource::File { path: id.into() },
            wasm_bytes: vec![],
            manifest: Some(manifest),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_mocked_namespace_answers_from_data_and_records_the_call() {
    let Some(env) = env().await else { return };
    install_library(&env.base.plugin_registry, "fake-http", "http").await;
    let file = env.file_state("src/lib/a.test.echo").await.unwrap();
    let got = answers(
        &file.state,
        &input(
            "test",
            json!([
                {"import": "mock", "request": {"namespace": "http", "responses": {"get": {"status": 200}}}},
                {"import": "mock", "request": {"namespace": "nope", "responses": {}}},
            ]),
        ),
    )
    .await;
    assert_eq!(got[0], json!({"ok": null}));
    assert_eq!(got[1]["error"]["code"], "BAD_INPUT", "{got:?}");

    let answer = file
        .state
        .plugin_executor()
        .call_library(
            "fake-http",
            "get",
            &[json!("https://x")],
            &Default::default(),
            0,
        )
        .await
        .unwrap();
    assert_eq!(answer, json!({"status": 200}));
    assert_eq!(file.harness.calls("http")[0].args, json!(["https://x"]));
    file.finish().await;
}

/// The mock lives on the file's harness, so a state without one dispatches
/// to the real library even while a test file has the same namespace mocked.
#[tokio::test]
async fn a_production_state_runs_the_real_library_for_a_mocked_namespace() {
    let Some(env) = env().await else { return };
    install_library(&env.base.plugin_registry, "fake-http", "http").await;
    let file = env.file_state("src/lib/a.test.echo").await.unwrap();
    let got = answers(
        &file.state,
        &input(
            "test",
            json!([
                {"import": "mock", "request": {"namespace": "http", "responses": {"get": {"status": 200}}}},
            ]),
        ),
    )
    .await;
    assert_eq!(got[0], json!({"ok": null}));

    assert!(env.base.test.is_none());
    let err = env
        .base
        .plugin_executor()
        .call_library(
            "fake-http",
            "get",
            &[json!("https://x")],
            &Default::default(),
            0,
        )
        .await
        .expect_err("a moduleless library cannot answer once it is instantiated");
    assert!(
        !matches!(err, ExecutionError::PluginError { .. }),
        "expected an instantiation failure, got {err:?}"
    );
    assert!(file.harness.calls("http").is_empty());
    file.finish().await;
}
