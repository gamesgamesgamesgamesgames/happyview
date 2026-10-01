//! The XRPC query and procedure runners, driven through the real routes.
//!
//! The `interpreter_echo` fixture claims `lua`, which is what a script row's
//! `script_type` holds, so a request resolves a script and reaches the fixture
//! exactly as it would reach a deployed interpreter. The fixture interprets
//! nothing — its `source` is a directive, and anything it does not recognise
//! echoes the whole `execute` input back — so what this pins is the runners'
//! half: what reaches an interpreter, what a caller is told about what came
//! back, and the rows an operator reads afterwards.
//!
//! Language semantics belong to the interpreter and are pinned where it is
//! loaded, in `lua_interpreter_plugin.rs` and `lua_differential.rs`.

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::atomic::Ordering;
use tower::ServiceExt;

use common::app::TestApp;
use common::echo_interpreter;

const QUERY: &str = "games.gamesgamesgamesgames.listGames";
const PROCEDURE: &str = "games.gamesgamesgamesgames.createGame";

/// Skips the caller when the fixture's module is absent, naming the build that
/// produces it. Every fixture's `target/` is gitignored, so that is a step
/// nobody ran; panicking on it would report it as a failure of the runners.
macro_rules! require_fixture {
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

/// The echo fixture installed as the interpreter for `lua`, the language a
/// seeded script row names.
async fn interpreter(app: &TestApp) {
    app.state
        .plugin_registry
        .register(echo_interpreter::plugin("lua"))
        .await;
}

async fn json_body(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

async fn upload_lexicon(app: &TestApp, lexicon: Value) {
    let cookie = app.admin_cookie();
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/lexicons")
                .header(cookie.0, cookie.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "lexicon_json": lexicon,
                        "target_collection": "games.gamesgamesgamesgames.game",
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(resp.status().is_success(), "{}", resp.status());
}

async fn seed_script(app: &TestApp, trigger_id: &str, body: &str) {
    let now = happyview::db::now_rfc3339();
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_scripts (id, body, script_type, created_at, updated_at) \
         VALUES (?, ?, 'lua', ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(trigger_id)
        .bind(body)
        .bind(&now)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .expect("seed a script row");
}

/// A query endpoint bound to `source`, ready to call.
async fn query_app(source: &str) -> TestApp {
    let app = TestApp::new().await;
    interpreter(&app).await;
    upload_lexicon(&app, common::fixtures::list_games_query_lexicon()).await;
    seed_script(&app, &format!("xrpc.query:{QUERY}"), source).await;
    app
}

/// A procedure endpoint bound to `source`, ready to call.
async fn procedure_app(source: &str) -> TestApp {
    let app = TestApp::new().await;
    interpreter(&app).await;
    upload_lexicon(&app, common::fixtures::create_game_procedure_lexicon()).await;
    seed_script(&app, &format!("xrpc.procedure:{PROCEDURE}"), source).await;
    app
}

async fn call_query(app: &TestApp, query_string: &str) -> axum::response::Response {
    app.router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/xrpc/{QUERY}{query_string}"))
                .header("x-client-key", "hvc_test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn call_procedure(app: &TestApp, body: &Value) -> axum::response::Response {
    let cookie = app.admin_cookie();
    app.router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/xrpc/{PROCEDURE}"))
                .header(cookie.0, cookie.1)
                .header("x-client-key", "hvc_test")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// The `detail` of the one row of `event_type`, so a test reads what an
/// operator reads rather than what the runner meant to write.
async fn one_event(app: &TestApp, event_type: &str) -> (Option<String>, Option<String>, Value) {
    let rows: Vec<(Option<String>, Option<String>, String)> =
        happyview::db::query_as(&happyview::db::adapt_sql(
            "SELECT actor_did, subject, detail FROM happyview_event_logs WHERE event_type = ?",
            app.state.db_backend,
        ))
        .bind(event_type)
        .fetch_all(&app.state.db)
        .await
        .expect("read the event log");
    assert_eq!(rows.len(), 1, "{event_type}: {rows:?}");
    let detail = serde_json::from_str(&rows[0].2).expect("the detail should be JSON");
    (rows[0].0.clone(), rows[0].1.clone(), detail)
}

fn detail_keys(detail: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = detail
        .as_object()
        .expect("a detail object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    keys
}

// ---------------------------------------------------------------------------
// What reaches the interpreter
// ---------------------------------------------------------------------------

/// The whole input, echoed back as the response body: the first argument of
/// `handle` and every context field a query run fills.
#[tokio::test]
async fn a_query_hands_the_interpreter_its_params_and_its_context() {
    common::require_db!();
    require_fixture!();
    let app = query_app("echo").await;
    happyview::db::query(&happyview::db::adapt_sql(
        "INSERT INTO happyview_script_variables (key, value, created_at) VALUES (?, ?, ?)",
        app.state.db_backend,
    ))
    .bind("API_KEY")
    .bind("k")
    .bind(happyview::db::now_rfc3339())
    .execute(&app.state.db)
    .await
    .expect("seed a script variable");

    let resp = call_query(&app, "?limit=5").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let sent = json_body(resp).await;

    assert_eq!(sent["source"], "echo");
    assert_eq!(sent["kind"], "xrpc_query");
    assert_eq!(sent["input"]["limit"], 5);
    assert_eq!(sent["context"]["trigger"], format!("xrpc.query:{QUERY}"));
    assert_eq!(sent["context"]["method"], QUERY);
    assert_eq!(
        sent["context"]["collection"],
        "games.gamesgamesgamesgames.game"
    );
    assert_eq!(sent["context"]["has_pds_auth"], false);
    assert_eq!(sent["context"]["env"]["API_KEY"], "k");
    // The parameters are the first argument, so carrying them twice would
    // leave a script author guessing which one a runner fills.
    assert!(sent["context"].get("params").is_none(), "{sent}");
    assert!(sent["context"].get("caller_did").is_none(), "{sent}");
    assert!(sent["context"].get("job").is_none(), "{sent}");
}

#[tokio::test]
async fn a_procedure_hands_the_interpreter_its_input_and_its_caller() {
    common::require_db!();
    require_fixture!();
    let app = procedure_app("echo").await;

    let resp = call_procedure(&app, &json!({ "title": "hello" })).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let sent = json_body(resp).await;

    assert_eq!(sent["kind"], "xrpc_procedure");
    assert_eq!(sent["input"]["title"], "hello");
    assert_eq!(
        sent["context"]["trigger"],
        format!("xrpc.procedure:{PROCEDURE}")
    );
    assert_eq!(sent["context"]["caller_did"], app.admin_did);
    assert_eq!(sent["context"]["method"], PROCEDURE);
    assert!(
        sent["context"]["params"].is_object(),
        "a procedure's parameters are its own field: {sent}"
    );
}

/// The budget an interpreter is given is the instance's cached one, so a
/// settings change reaches the next run without a restart.
#[tokio::test]
async fn the_cached_instruction_budget_is_what_reaches_the_interpreter() {
    common::require_db!();
    require_fixture!();
    let app = query_app("echo").await;

    let sent = json_body(call_query(&app, "").await).await;
    assert_eq!(
        sent["limits"]["instructions"],
        app.state.script_limits.instruction_limit()
    );

    app.state.script_limits.set_instruction_limit(1_000);
    let sent = json_body(call_query(&app, "").await).await;
    assert_eq!(sent["limits"]["instructions"], 1_000);
}

// ---------------------------------------------------------------------------
// What a caller is told
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_returned_value_is_the_response_body() {
    common::require_db!();
    require_fixture!();
    let app = query_app("value:other").await;

    let resp = call_query(&app, "?limit=5").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json_body(resp).await, json!({ "limit": 5 }));
}

/// `nil` is a body of `null`, not an empty body and not an error: a query that
/// found nothing still answered.
#[tokio::test]
async fn a_run_that_returned_nothing_answers_null() {
    common::require_db!();
    require_fixture!();
    let app = query_app("value:none").await;

    let resp = call_query(&app, "").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json_body(resp).await, Value::Null);
}

#[tokio::test]
async fn a_failure_carries_the_interpreters_kind_message_and_line() {
    common::require_db!();
    require_fixture!();
    for (kind, status) in [
        ("syntax", StatusCode::INTERNAL_SERVER_ERROR),
        ("runtime", StatusCode::INTERNAL_SERVER_ERROR),
        ("memory", StatusCode::INTERNAL_SERVER_ERROR),
        ("missing_handle", StatusCode::INTERNAL_SERVER_ERROR),
    ] {
        let app = query_app(&format!("error:{kind}")).await;
        let resp = call_query(&app, "").await;
        assert_eq!(resp.status(), status, "{kind}");
        let body = json_body(resp).await;
        assert_eq!(body["error"], "script_error", "{kind}");
        assert_eq!(body["errorType"], kind, "{kind}");
        assert_eq!(body["message"], format!("echo: {kind}"), "{kind}");
        assert_eq!(body["method"], QUERY, "{kind}");
        assert_eq!(body["line"], 7, "{kind}");
    }
}

/// A spent budget is the one failure whose message is the host's: the limit is
/// the host's, and the status says to retry rather than to fix the script.
#[tokio::test]
async fn a_timeout_is_a_408_with_the_execution_limit_sentence() {
    common::require_db!();
    require_fixture!();
    let app = query_app("error:timeout").await;

    let resp = call_query(&app, "").await;
    assert_eq!(resp.status(), StatusCode::REQUEST_TIMEOUT);
    let body = json_body(resp).await;
    assert_eq!(body["errorType"], "timeout");
    assert_eq!(body["message"], "script exceeded execution time limit");
}

/// A guest that never yields is stopped by the host's own deadline rather than
/// by anything the script could catch, and reaches a caller the same way a
/// spent budget does.
#[tokio::test]
async fn a_script_that_never_returns_is_a_408() {
    common::require_db!();
    require_fixture!();
    let app = query_app("spin").await;
    app.state.script_limits.set_wall_clock_seconds(1);

    let started = std::time::Instant::now();
    let resp = call_query(&app, "").await;
    assert_eq!(resp.status(), StatusCode::REQUEST_TIMEOUT);
    assert_eq!(
        json_body(resp).await["message"],
        "script exceeded execution time limit"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "the run outlived the one-second clock: {:?}",
        started.elapsed()
    );
}

// ---------------------------------------------------------------------------
// What an operator reads afterwards
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_successful_query_writes_the_executed_row() {
    common::require_db!();
    require_fixture!();
    let app = query_app("value:other").await;

    assert_eq!(call_query(&app, "?limit=5").await.status(), StatusCode::OK);

    let (actor, subject, detail) = one_event(&app, "script.executed").await;
    assert_eq!(actor, None, "a query run acts as nobody");
    assert_eq!(subject.as_deref(), Some(QUERY));
    assert_eq!(
        detail_keys(&detail),
        [
            "duration_ms",
            "method",
            "params",
            "response",
            "response_size"
        ]
    );
    assert_eq!(detail["method"], QUERY);
    assert_eq!(detail["params"]["limit"], 5);
    assert_eq!(detail["response"], json!({ "limit": 5 }));
    assert_eq!(detail["response_size"], r#"{"limit":5}"#.len());
}

/// The event log keeps the interpreter's unparsed text, which is the only
/// place the line, the chunk name and any traceback survive.
#[tokio::test]
async fn a_failed_query_writes_the_error_row_with_the_unparsed_text() {
    common::require_db!();
    require_fixture!();
    let app = query_app("error:runtime").await;

    assert_eq!(
        call_query(&app, "").await.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );

    let (actor, subject, detail) = one_event(&app, "script.error").await;
    assert_eq!(actor, None);
    assert_eq!(subject.as_deref(), Some(QUERY));
    assert_eq!(
        detail_keys(&detail),
        ["duration_ms", "error", "method", "script_source"]
    );
    assert_eq!(detail["error"], r#"[string "script"]:7: echo: runtime"#);
    assert_eq!(detail["script_source"], "error:runtime");
}

#[tokio::test]
async fn a_successful_procedure_writes_the_executed_row() {
    common::require_db!();
    require_fixture!();
    let app = procedure_app("value:other").await;

    assert_eq!(
        call_procedure(&app, &json!({ "title": "hello" }))
            .await
            .status(),
        StatusCode::OK
    );

    let (actor, subject, detail) = one_event(&app, "script.executed").await;
    assert_eq!(actor.as_deref(), Some(app.admin_did.as_str()));
    assert_eq!(subject.as_deref(), Some(PROCEDURE));
    assert_eq!(
        detail_keys(&detail),
        [
            "caller_did",
            "duration_ms",
            "input",
            "method",
            "response",
            "response_size"
        ]
    );
    assert_eq!(detail["caller_did"], app.admin_did);
    assert_eq!(detail["input"]["title"], "hello");
    assert_eq!(detail["response"]["title"], "hello");
}

#[tokio::test]
async fn a_failed_procedure_writes_the_error_row() {
    common::require_db!();
    require_fixture!();
    let app = procedure_app("error:runtime").await;

    assert_eq!(
        call_procedure(&app, &json!({ "title": "hello" }))
            .await
            .status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );

    let (actor, subject, detail) = one_event(&app, "script.error").await;
    assert_eq!(actor.as_deref(), Some(app.admin_did.as_str()));
    assert_eq!(subject.as_deref(), Some(PROCEDURE));
    assert_eq!(
        detail_keys(&detail),
        [
            "caller_did",
            "duration_ms",
            "error",
            "input",
            "method",
            "script_source"
        ]
    );
    assert_eq!(detail["error"], r#"[string "script"]:7: echo: runtime"#);
    assert_eq!(detail["input"]["title"], "hello");
}

/// Every run is counted, whatever became of it: a counter lost to an early
/// return is a runtime graph that disagrees with the event log.
#[tokio::test]
async fn every_run_moves_the_script_counters() {
    common::require_db!();
    require_fixture!();
    let app = TestApp::new().await;
    interpreter(&app).await;
    upload_lexicon(&app, common::fixtures::list_games_query_lexicon()).await;
    let counters = app.state.telemetry_counters.clone();

    for (run, source) in ["value:other", "error:missing_handle", "error:runtime"]
        .into_iter()
        .enumerate()
    {
        happyview::db::query(&happyview::db::adapt_sql(
            "DELETE FROM happyview_scripts WHERE id = ?",
            app.state.db_backend,
        ))
        .bind(format!("xrpc.query:{QUERY}"))
        .execute(&app.state.db)
        .await
        .unwrap();
        seed_script(&app, &format!("xrpc.query:{QUERY}"), source).await;

        call_query(&app, "").await;
        assert_eq!(
            counters.script_executions.load(Ordering::Relaxed),
            run as u64 + 1,
            "{source}"
        );
    }
    assert!(counters.script_runtime_us.load(Ordering::Relaxed) > 0);
}
