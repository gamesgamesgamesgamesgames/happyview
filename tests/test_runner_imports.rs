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
                indexed: true,
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
        matches!(err, ExecutionError::Instantiation(_)),
        "expected an instantiation failure, got {err:?}"
    );
    assert!(file.harness.calls("http").is_empty());
    file.finish().await;
}

// ---------------------------------------------------------------------------
// Firing triggers
// ---------------------------------------------------------------------------

async fn seed_lexicon(state: &happyview::AppState, doc: Value) {
    happyview::testing::seed::lexicon(state, doc).await;
}

async fn seed_script(state: &happyview::AppState, trigger: &str, body: &str) {
    happyview::testing::seed::script(state, trigger, "echo", body).await;
}

const QUERY: &str = "com.example.getThing";
const COLLECTION: &str = "com.example.thing";
const CREATE: &str = "com.example.createThing";

fn trigger(kind: &str, target: &str, extra: Value) -> Value {
    let mut request = json!({"kind": kind, "target": target});
    for (k, v) in extra.as_object().unwrap() {
        request[k] = v.clone();
    }
    json!({"import": "trigger", "request": request})
}

async fn file_with_handlers(env: &TestEnv) -> happyview::testing::env::FileState {
    let file = env.file_state("src/lib/a.test.echo").await.unwrap();
    seed_lexicon(&file.state, json!({"lexicon": 1, "id": QUERY, "defs": {"main": {
        "type": "query", "parameters": {"type": "params", "properties": {"id": {"type": "string"}}}
    }}})).await;
    seed_lexicon(
        &file.state,
        json!({"lexicon": 1, "id": COLLECTION, "defs": {"main": {
            "type": "record", "key": "tid", "record": {"type": "object", "properties": {}}
        }}}),
    )
    .await;
    seed_script(&file.state, &format!("xrpc.query:{QUERY}"), "anything").await;
    seed_script(
        &file.state,
        &format!("record.index:{COLLECTION}"),
        "value:other",
    )
    .await;
    file
}

#[tokio::test]
async fn a_query_runs_through_the_router_as_the_forged_caller() {
    let Some(env) = env().await else { return };
    let file = file_with_handlers(&env).await;
    let got = answers(
        &file.state,
        &input(
            "test",
            json!([
                trigger(
                    "query",
                    QUERY,
                    json!({"input": {"id": "1"}, "as": "did:plc:alice"})
                ),
                trigger("query", QUERY, json!({"input": {"id": "1"}})),
            ]),
        ),
    )
    .await;
    let forged = &got[0]["ok"];
    assert_eq!(forged["status"], 200, "{got:?}");
    // The echo handler returns its whole input, context included.
    assert_eq!(forged["body"]["context"]["caller_did"], "did:plc:alice");
    assert_eq!(forged["body"]["input"]["id"], "1");
    assert!(got[1]["ok"]["body"]["context"].get("caller_did").is_none());
    file.finish().await;
}

#[tokio::test]
async fn a_procedure_without_a_caller_is_refused_with_401() {
    let Some(env) = env().await else { return };
    let file = file_with_handlers(&env).await;
    let got = answers(
        &file.state,
        &input(
            "test",
            json!([trigger(
                "procedure",
                "com.example.doThing",
                json!({"input": {}})
            ),]),
        ),
    )
    .await;
    assert_eq!(got[0]["ok"]["status"], 401, "{got:?}");
    file.finish().await;
}

#[tokio::test]
async fn a_record_create_runs_the_index_script_and_indexes_with_a_real_cid() {
    let Some(env) = env().await else { return };
    let file = file_with_handlers(&env).await;
    let got = answers(
        &file.state,
        &input(
            "test",
            json!([trigger(
                "record_create",
                COLLECTION,
                json!({"input": {"n": 1}, "did": "did:plc:alice", "rkey": "1"})
            ),]),
        ),
    )
    .await;
    let result = &got[0]["ok"];
    assert_eq!(result["status"], 200, "{got:?}");
    assert_eq!(result["body"]["outcome"], "matched");
    let (cid,): (String,) =
        happyview::db::query_as("SELECT cid FROM happyview_records WHERE uri = ?")
            .bind(format!("at://did:plc:alice/{COLLECTION}/1"))
            .fetch_one(&file.state.db)
            .await
            .unwrap();
    assert_eq!(result["body"]["cid"], cid);
    file.finish().await;
}

#[tokio::test]
async fn a_failing_record_script_is_a_500_after_one_attempt() {
    let Some(env) = env().await else { return };
    let file = file_with_handlers(&env).await;
    seed_script(
        &file.state,
        &format!("record.create:{COLLECTION}"),
        "error:runtime",
    )
    .await;
    let started = std::time::Instant::now();
    let got = answers(
        &file.state,
        &input(
            "test",
            json!([trigger("record_create", COLLECTION, json!({"input": {}})),]),
        ),
    )
    .await;
    assert_eq!(got[0]["ok"]["status"], 500, "{got:?}");
    assert!(
        got[0]["ok"]["body"]["errors"][0]
            .as_str()
            .unwrap()
            .contains("echo: runtime")
    );
    // Production retries with 1+2+4 seconds of backoff; a test run does not.
    // The bound leaves room for a loaded machine, and the dead letter's own
    // count is the exact check.
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    let (attempts,): (i64,) =
        happyview::db::query_as("SELECT attempts FROM happyview_dead_letter_scripts")
            .fetch_one(&file.state.db)
            .await
            .unwrap();
    assert_eq!(attempts, 1);
    file.finish().await;
}

#[tokio::test]
async fn a_record_trigger_for_an_untracked_collection_names_it() {
    let Some(env) = env().await else { return };
    let file = file_with_handlers(&env).await;
    let got = answers(
        &file.state,
        &input(
            "test",
            json!([
                trigger("record_create", "com.example.unknown", json!({"input": {}})),
                trigger("record_update", COLLECTION, json!({"input": {}})),
            ]),
        ),
    )
    .await;
    assert_eq!(got[0]["error"]["code"], "BAD_INPUT");
    assert!(
        got[0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("com.example.unknown")
    );
    assert!(
        got[1]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("rkey")
    );
    file.finish().await;
}

#[tokio::test]
async fn a_job_runs_once_and_reports_its_row() {
    let Some(env) = env().await else { return };
    let file = env.file_state("src/lib/a.test.echo").await.unwrap();
    seed_script(&file.state, "job.run:export", "returns:{\"done\":true}").await;
    let got = answers(
        &file.state,
        &input(
            "test",
            json!([
                trigger(
                    "job",
                    "export",
                    json!({"input": {"n": 1}, "as": "did:plc:alice"})
                ),
                trigger("job", "missing", json!({})),
            ]),
        ),
    )
    .await;
    assert_eq!(got[0]["ok"]["status"], 200, "{got:?}");
    assert_eq!(got[0]["ok"]["body"]["result"], json!({"done": true}));
    assert_eq!(got[1]["ok"]["status"], 500);
    file.finish().await;
}

/// Runs `test` on a runtime of its own and fails once `limit` passes. A guest
/// whose deadline was lifted never yields, so the timeout has to fire on
/// another worker, and the spinning one is abandoned rather than awaited.
fn within<F>(limit: Duration, test: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let handle = runtime.spawn(test);
    let finished = runtime.block_on(async { tokio::time::timeout(limit, handle).await });
    runtime.shutdown_background();
    match finished {
        Err(_) => panic!("the test was still running after {limit:?}"),
        Ok(Err(join)) => std::panic::resume_unwind(join.into_panic()),
        Ok(Ok(())) => {}
    }
}

/// A job fired from a test file has a test file's budget, where a serving job
/// has none: nothing would ever ask it to stop, and the run awaits it.
#[test]
fn a_spinning_job_fired_from_a_test_file_fails_at_the_files_budget() {
    let budget = Duration::from_secs(happyview::plugin::executor::TEST_FILE_GUEST_SECONDS);
    within(budget + Duration::from_secs(60), async move {
        let Some(env) = env().await else { return };
        let file = env.file_state("src/lib/a.test.echo").await.unwrap();
        seed_script(&file.state, "job.run:spin", "spin").await;

        let started = Instant::now();
        let got = answers(
            &file.state,
            &input("test", json!([trigger("job", "spin", json!({}))])),
        )
        .await;
        let elapsed = started.elapsed();
        assert_eq!(got[0]["ok"]["status"], 500, "{got:?}");
        assert_eq!(got[0]["ok"]["body"]["status"], "failed", "{got:?}");
        assert!(
            got[0]["ok"]["body"]["error"]
                .as_str()
                .unwrap()
                .contains("execution limit"),
            "{got:?}"
        );
        assert!(
            elapsed >= budget && elapsed < budget + Duration::from_secs(10),
            "{elapsed:?}"
        );
        file.finish().await;
    });
}

/// An hour's wait in a job fired from a test file is cut to the file's
/// budget, and the job then completes.
#[tokio::test]
async fn an_hours_wait_in_a_job_fired_from_a_test_file_returns_within_its_budget() {
    let Some(env) = env().await else { return };
    let file = env.file_state("src/lib/a.test.echo").await.unwrap();
    seed_script(&file.state, "job.run:sleep", "host:job_wait").await;
    let budget = Duration::from_secs(happyview::plugin::executor::TEST_FILE_GUEST_SECONDS);

    let started = Instant::now();
    let got = tokio::time::timeout(
        budget + Duration::from_secs(30),
        answers(
            &file.state,
            &input(
                "test",
                json!([trigger("job", "sleep", json!({"input": {"seconds": 3600}}))]),
            ),
        ),
    )
    .await
    .expect("an hour's wait in a test run was not cut to the file's budget");
    assert_eq!(got[0]["ok"]["status"], 200, "{got:?}");
    assert!(started.elapsed() < budget + Duration::from_secs(10));
    file.finish().await;
}

#[tokio::test]
async fn a_label_runs_its_script_and_answers_what_was_stored() {
    let Some(env) = env().await else { return };
    let file = env.file_state("src/lib/a.test.echo").await.unwrap();
    seed_script(
        &file.state,
        &format!("labeler.apply:{COLLECTION}"),
        "value:none",
    )
    .await;
    let subject = format!("at://did:plc:alice/{COLLECTION}/1");
    let got = answers(
        &file.state,
        &input(
            "test",
            json!([
                trigger("label", &subject, json!({"input": {"val": "spam"}})),
                trigger("label", "did:plc:alice", json!({"input": {"val": "spam"}})),
            ]),
        ),
    )
    .await;
    assert_eq!(
        got[0]["ok"]["body"]["applied"],
        json!(null),
        "skipped by the script: {got:?}"
    );
    assert_eq!(got[1]["ok"]["body"]["applied"]["val"], "spam");
    file.finish().await;
}

/// A label the database refused is not reported as applied: the trigger
/// fails and names the write.
#[tokio::test]
async fn a_label_that_could_not_be_stored_is_a_500_and_not_applied() {
    let Some(env) = env().await else { return };
    let file = env.file_state("src/lib/a.test.echo").await.unwrap();
    happyview::db::query("DROP TABLE happyview_labels")
        .execute(&file.state.db)
        .await
        .unwrap();
    let got = answers(
        &file.state,
        &input(
            "test",
            json!([trigger(
                "label",
                "did:plc:alice",
                json!({"input": {"val": "spam"}})
            )]),
        ),
    )
    .await;
    let result = &got[0]["ok"];
    assert_eq!(result["status"], 500, "{got:?}");
    assert_eq!(result["body"]["applied"], json!(null), "{got:?}");
    assert!(
        result["body"]["errors"][0]
            .as_str()
            .unwrap()
            .contains("failed to upsert label"),
        "{got:?}"
    );
    file.finish().await;
}

#[tokio::test]
async fn a_forged_caller_is_ignored_without_a_harness() {
    let Some(env) = env().await else { return };
    let mut request = axum::http::Request::builder()
        .method("POST")
        .uri("/xrpc/com.example.doThing")
        .header("host", happyview::testing::TEST_HOST)
        .header("content-type", "application/json")
        // The key a fired request carries, so the refusal below is the
        // anonymous one rather than the missing-client-key one, which a
        // forged identity would also meet.
        .header("x-client-key", happyview::testing::TEST_CLIENT_KEY)
        .body(axum::body::Body::from("{}"))
        .unwrap();
    request
        .extensions_mut()
        .insert(happyview::testing::ForgedCaller::new("did:plc:alice"));
    let response = happyview::server::router(env.base.clone())
        .oneshot(request)
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    assert_eq!(status, 401, "{body}");
    assert_eq!(
        body["error"], "XRPC procedures require DPoP authentication",
        "{body}"
    );
}

/// A query or procedure trigger is spliced into `/xrpc/{target}`, so a target
/// that is not an NSID could reach any other route, `/auth/*` included.
#[tokio::test]
async fn an_xrpc_trigger_reaches_only_an_xrpc_method() {
    let Some(env) = env().await else { return };
    let file = file_with_handlers(&env).await;
    let got = answers(
        &file.state,
        &input(
            "test",
            json!([
                trigger("query", "../auth/login", json!({})),
                trigger(
                    "procedure",
                    "com.example.doThing/../../auth/logout",
                    json!({"as": "did:plc:alice"})
                ),
                trigger("query", "com.example.getThing?id=1", json!({})),
            ]),
        ),
    )
    .await;
    for answer in &got {
        assert_eq!(answer["error"]["code"], "BAD_INPUT", "{got:?}");
        assert!(
            answer["error"]["message"]
                .as_str()
                .unwrap()
                .contains("NSID"),
            "{got:?}"
        );
    }
    file.finish().await;
}

/// A script stored on one file's state answers that file's triggers, and
/// neither the template nor the next file sees it.
#[tokio::test]
async fn a_script_seeded_on_a_file_answers_only_that_files_trigger() {
    let Some(env) = env().await else { return };
    let query = json!({"lexicon": 1, "id": QUERY, "defs": {"main": {"type": "query"}}});
    let first = env.file_state("src/lib/a.test.echo").await.unwrap();
    seed_lexicon(&first.state, query.clone()).await;
    seed_script(
        &first.state,
        &format!("xrpc.query:{QUERY}"),
        "returns:{\"from\":\"a\"}",
    )
    .await;
    let second = env.file_state("src/lib/b.test.echo").await.unwrap();
    seed_lexicon(&second.state, query).await;

    let fire = json!([trigger("query", QUERY, json!({}))]);
    let from_first = answers(&first.state, &input("test", fire.clone())).await;
    assert_eq!(from_first[0]["ok"]["status"], 200, "{from_first:?}");
    assert_eq!(from_first[0]["ok"]["body"], json!({"from": "a"}));

    let from_second = answers(&second.state, &input("test", fire)).await;
    assert_ne!(from_second[0]["ok"]["status"], 200, "{from_second:?}");
    assert_ne!(from_second[0]["ok"]["body"], json!({"from": "a"}));
    assert!(
        env.base
            .scripts
            .get_manual(&format!("xrpc.query:{QUERY}"))
            .await
            .unwrap()
            .is_none()
    );
    first.finish().await;
    second.finish().await;
}

/// Iterations of the echo fixture's `burn` step that take about `target` of
/// guest time on this machine, measured rather than assumed.
async fn calibrate_burn(state: &happyview::AppState, target: Duration) -> u64 {
    let mut iterations: u64 = 10_000_000;
    loop {
        let started = Instant::now();
        answers(state, &input("test", json!([{"burn": iterations}]))).await;
        let took = started.elapsed();
        if took >= Duration::from_millis(500) {
            return (iterations as f64 * target.as_secs_f64() / took.as_secs_f64()) as u64;
        }
        iterations *= 4;
    }
}

/// A fired handler runs on an instance of its own, under its own deadline:
/// its spin is stopped by the handler's wall clock and the file carries on.
/// The file's budget is neither cut to the handler's, nor charged for the
/// handler's time, nor reset by the trigger's return: the file burns guest
/// time before it fires, so a fresh budget on return would end the run that
/// much later than one budget plus the handler's wall clock.
#[tokio::test]
async fn a_fired_trigger_runs_on_its_own_instance_and_budget() {
    let Some(env) = env().await else { return };
    let file = file_with_handlers(&env).await;
    seed_script(&file.state, &format!("xrpc.query:{QUERY}"), "spin").await;
    const HANDLER_SECONDS: u32 = 5;
    file.state
        .script_limits
        .set_wall_clock_seconds(HANDLER_SECONDS);
    let handler = Duration::from_secs(HANDLER_SECONDS.into());
    let budget = Duration::from_secs(happyview::plugin::executor::TEST_FILE_GUEST_SECONDS);
    // Twice the upper bound's slack, so a reset lands well past it.
    let slack = Duration::from_secs(8);
    let burn = calibrate_burn(&file.state, slack * 2).await;

    let steps = json!([
        {"burn": burn},
        trigger("query", QUERY, json!({})),
        {"check": {"name": "the handler timed out", "answer": 1, "pointer": "/ok/status", "equals": 408}},
        {"spin": true},
    ]);
    let started = Instant::now();
    let err = tokio::time::timeout(
        budget * 2 + handler,
        file.state
            .plugin_executor()
            .execute_script("interpreter_echo", &input("test", steps)),
    )
    .await
    .expect("a spinning test file was never stopped")
    .expect_err("a spinning test file returned");
    let elapsed = started.elapsed();
    assert!(matches!(err, ExecutionError::Timeout), "{err}");
    let reports = file.harness.reports();
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(reports[0].status, TestStatus::Pass, "{reports:?}");
    assert!(
        elapsed >= budget + handler && elapsed < budget + handler + slack,
        "{elapsed:?}"
    );
    file.finish().await;
}

fn procedure_lexicon() -> LexiconWrite {
    LexiconWrite {
        lexicon_json: json!({"lexicon": 1, "id": CREATE, "defs": {"main": {
            "type": "procedure",
            "input": {"encoding": "application/json", "schema": {"type": "object", "properties": {}}},
        }}}),
        backfill: false,
        target_collection: Some(COLLECTION.into()),
        action: ProcedureAction::Create,
        token_cost: None,
        authority_did: None,
        indexed: true,
    }
}

/// A forged caller has no OAuth session, so a scriptless procedure fired as
/// one writes only because a test run answers it from the in-process PDS.
#[tokio::test]
async fn a_scriptless_procedure_fired_as_a_caller_writes_through_the_test_pds() {
    let Some(env) = env().await else { return };
    let file = file_with_handlers(&env).await;
    let state = &file.state;
    state
        .lexicons
        .put_manual(&state.db, state.db_backend, procedure_lexicon())
        .await
        .unwrap();
    let got = answers(
        state,
        &input(
            "test",
            json!([trigger(
                "procedure",
                CREATE,
                json!({"input": {"n": 1}, "as": "did:plc:alice"})
            ),]),
        ),
    )
    .await;
    let result = &got[0]["ok"];
    assert_eq!(result["status"], 200, "{got:?}");

    let writes = file.harness.pds().writes();
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert_eq!(writes[0].repo, "did:plc:alice");
    assert_eq!(writes[0].collection, COLLECTION);
    let uri = result["body"]["uri"]
        .as_str()
        .expect("the PDS's uri is answered");
    let (cid,): (String,) =
        happyview::db::query_as("SELECT cid FROM happyview_records WHERE uri = ?")
            .bind(uri)
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!(result["body"]["cid"], cid);
    file.finish().await;
}

/// A forged caller has no API client to delegate through, so `delegateDid`
/// is refused by name rather than written into the record.
#[tokio::test]
async fn a_procedure_fired_with_delegate_did_is_refused_by_name() {
    let Some(env) = env().await else { return };
    let file = file_with_handlers(&env).await;
    let state = &file.state;
    state
        .lexicons
        .put_manual(&state.db, state.db_backend, procedure_lexicon())
        .await
        .unwrap();
    let got = answers(
        state,
        &input(
            "test",
            json!([trigger(
                "procedure",
                CREATE,
                json!({
                    "input": {"n": 1, "delegateDid": "did:plc:bob"}, "as": "did:plc:alice",
                })
            ),]),
        ),
    )
    .await;
    assert_eq!(got[0]["ok"]["status"], 400, "{got:?}");
    assert!(
        got[0]["ok"]["body"]["error"]
            .as_str()
            .unwrap()
            .contains("delegateDid"),
        "{got:?}"
    );
    assert!(file.harness.pds().writes().is_empty());
    file.finish().await;
}

async fn env_with_fixtures(fixtures: &[&str]) -> Option<TestEnv> {
    if !echo_interpreter::is_built() {
        eprintln!("skipping: {}", echo_interpreter::BUILD);
        return None;
    }
    let registry = Arc::new(PluginRegistry::new());
    registry
        .install(echo_interpreter::plugin("echo"))
        .await
        .unwrap();
    for fixture in fixtures {
        let plugin = happyview::plugin::loader::load_from_file(std::path::Path::new(&format!(
            "tests/fixtures/{fixture}"
        )))
        .await
        .unwrap_or_else(|e| {
            panic!(
                "{fixture} fixture not built ({e}). Run: cargo build --manifest-path \
                 tests/fixtures/{fixture}/Cargo.toml --target wasm32-unknown-unknown --release"
            )
        });
        registry.install(plugin).await.unwrap();
    }
    Some(
        TestEnv::new(registry, Arc::new(WasmRuntime::new().unwrap()), None)
            .await
            .unwrap(),
    )
}

/// `host:require`'s request: the echo fixture reads the call from its input.
fn require(library: &str, function: &str, arg: Value) -> Value {
    json!({"library": library, "function": function, "args": [arg]})
}

/// A job fired `as` a caller inherits that caller's auth, which in a test run
/// is the in-process PDS: its writes land there, and a job it enqueues with
/// `auth = true` carries the same.
#[tokio::test]
async fn a_job_fired_as_a_caller_acts_through_the_test_pds() {
    let Some(env) = env_with_fixtures(&["sdk_caller", "sdk_linked_repos"]).await else {
        return;
    };
    let file = file_with_handlers(&env).await;
    seed_script(&file.state, "job.run:write", "host:require").await;
    seed_script(&file.state, "job.run:spawn", "host:require").await;
    let got = answers(&file.state, &input("test", json!([
        trigger("job", "write", json!({"as": "did:plc:alice", "input": require(
            "sdk_caller",
            "create_record",
            json!({"collection": COLLECTION, "rkey": "j1", "record": {"n": 1}, "validate": false}),
        )})),
        trigger("job", "spawn", json!({"as": "did:plc:alice", "input": require(
            "sdk_linked_repos",
            "jobs_create",
            json!({"job_type": "export", "input": {}, "auth": true}),
        )})),
    ]))).await;

    assert_eq!(got[0]["ok"]["status"], 200, "{got:?}");
    let writes = file.harness.pds().writes();
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert_eq!(writes[0].repo, "did:plc:alice");
    assert_eq!(writes[0].rkey, "j1");

    assert_eq!(got[1]["ok"]["status"], 200, "{got:?}");
    let spawned = got[1]["ok"]["body"]["result"]
        .as_str()
        .expect("the enqueued job's id");
    let (inherit,): (i64,) = happyview::db::query_as(
        "SELECT CAST(inherit_auth AS INTEGER) FROM happyview_jobs WHERE id = ?",
    )
    .bind(spawned)
    .fetch_one(&file.state.db)
    .await
    .unwrap();
    assert_eq!(inherit, 1);
    file.finish().await;
}

/// The mock answers a call a fired handler makes through `host_call_library`,
/// not only one the host makes directly.
#[tokio::test]
async fn a_mocked_namespace_answers_a_fired_handlers_require() {
    let Some(env) = env().await else { return };
    install_library(&env.base.plugin_registry, "fake-http", "http").await;
    let file = env.file_state("src/lib/a.test.echo").await.unwrap();
    seed_script(&file.state, "job.run:fetch", "host:require").await;
    let got = answers(&file.state, &input("test", json!([
        {"import": "mock", "request": {"namespace": "http", "responses": {"get": {"status": 201}}}},
        trigger("job", "fetch", json!({"input": require("fake-http", "get", json!("https://x"))})),
        {"import": "calls", "request": {"namespace": "http"}},
    ]))).await;
    assert_eq!(got[1]["ok"]["status"], 200, "{got:?}");
    assert_eq!(got[1]["ok"]["body"]["result"], json!({"status": 201}));
    assert_eq!(got[2]["ok"][0]["method"], "get", "{got:?}");
    assert_eq!(got[2]["ok"][0]["args"], json!(["https://x"]));
    file.finish().await;
}
