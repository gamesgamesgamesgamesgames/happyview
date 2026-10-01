//! What `/admin/scripts` asks an interpreter about a body before storing it,
//! and what it does with the answer.
//!
//! The three refusals an operator meets — a missing `handle`, a body that
//! will not parse, and a file-scope read of a removed global — are asserted
//! against the real Lua plugin, because the wording is the thing under test
//! and a fixture would be asserting the fixture. The two probes that say what
//! the save path *sends* and what it reaches use `interpreter_echo`, whose
//! `source` is a directive: it reports the guard list it was handed, and it
//! reports what `host_script_log` answered.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;
use common::{echo_interpreter, lua_plugin};

const LUA_ID: &str = "xrpc.query:com.example.list";
const ECHO_ID: &str = "xrpc.query:com.example.echo";
const VALID: &str = "function handle(input, ctx)\n  return input\nend\n";

async fn json_body(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

async fn post_script(app: &TestApp, body: &Value) -> axum::response::Response {
    let cookie = app.admin_cookie();
    app.router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/scripts")
                .header(cookie.0, cookie.1)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn patch_script(app: &TestApp, id: &str, body: &Value) -> axum::response::Response {
    let cookie = app.admin_cookie();
    app.router
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/admin/scripts/{}", urlencoding::encode(id)))
                .header(cookie.0, cookie.1)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn get_script(app: &TestApp, id: &str) -> axum::response::Response {
    let cookie = app.admin_cookie();
    app.router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/admin/scripts/{}", urlencoding::encode(id)))
                .header(cookie.0, cookie.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

/// The message a refusal carried, with its status checked first so a test
/// that got a 201 says so rather than failing on a missing field.
async fn refusal(resp: axum::response::Response) -> String {
    let status = resp.status();
    let body = json_body(resp).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    body["error"].as_str().expect("a message").to_string()
}

/// The real Lua plugin, registered as the interpreter for `lua`. `None` when
/// the runner did not name a built plugin.
async fn lua_interpreter(app: &TestApp) -> Option<()> {
    let dir = lua_plugin::plugin_dir()?;
    println!("module: {}", lua_plugin::identify(&dir));
    let plugin = happyview::plugin::loader::load_from_file(&dir)
        .await
        .expect("the plugin should load through the loader");
    app.state.plugin_registry.register(plugin).await;
    Some(())
}

async fn script_log_rows(app: &TestApp) -> i64 {
    let sql = happyview::db::adapt_sql(
        "SELECT COUNT(*) FROM happyview_event_logs WHERE event_type = 'script.log'",
        app.state.db_backend,
    );
    let (count,): (i64,) = happyview::db::query_as(&sql)
        .fetch_one(&app.state.db)
        .await
        .expect("count script.log rows");
    count
}

// ---------------------------------------------------------------------------
// The three refusals, against the interpreter that owns their wording
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn a_body_with_no_handle_is_refused_by_the_sentence_that_names_it() {
    common::require_db!();
    let app = TestApp::new().await;
    if lua_interpreter(&app).await.is_none() {
        return;
    }

    let error = refusal(
        post_script(
            &app,
            &json!({ "id": LUA_ID, "body": "function other() return {} end\n" }),
        )
        .await,
    )
    .await;
    assert_eq!(error, "script must define a handle() function");
    assert_eq!(
        get_script(&app, LUA_ID).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
#[serial]
async fn a_body_that_will_not_parse_is_refused_at_its_line() {
    common::require_db!();
    let app = TestApp::new().await;
    if lua_interpreter(&app).await.is_none() {
        return;
    }

    let resp = post_script(&app, &json!({ "id": LUA_ID, "body": "function handle(" })).await;
    let status = resp.status();
    let body = json_body(resp).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let error = body["error"].as_str().expect("a message");
    assert!(error.starts_with("script compilation failed"), "{error}");
    assert!(error.contains("line 1"), "{error}");
    // The field the dashboard keys its Migrate offer on, and nothing here is
    // a migration.
    assert!(body.get("removed_globals").is_none(), "{body}");
    assert_eq!(
        get_script(&app, LUA_ID).await.status(),
        StatusCode::NOT_FOUND
    );
}

/// `params` is set for a query, so the codemod's scanner reports it and the
/// refusal an operator meets is the codemod's. `input` is not, so this body
/// passes the scanner and the interpreter's own guard is what refuses it —
/// which is the only way to reach that guard through a save.
#[tokio::test]
#[serial]
async fn a_file_scope_read_of_a_removed_global_the_scanner_passes_is_refused_by_name() {
    common::require_db!();
    let app = TestApp::new().await;
    if lua_interpreter(&app).await.is_none() {
        return;
    }

    let error = refusal(
        post_script(
            &app,
            &json!({
                "id": LUA_ID,
                "body": "local carried = input\nfunction handle() return carried end\n",
            }),
        )
        .await,
    )
    .await;
    assert!(
        error.contains("the 'input' global was removed in v3"),
        "{error}"
    );
    assert_eq!(
        get_script(&app, LUA_ID).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
#[serial]
async fn a_v3_body_saves() {
    common::require_db!();
    let app = TestApp::new().await;
    if lua_interpreter(&app).await.is_none() {
        return;
    }

    let resp = post_script(&app, &json!({ "id": LUA_ID, "body": VALID })).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert_eq!(json_body(resp).await["runnable"], true);
}

/// Each prefill the editor offers has to survive the form that offers it, so
/// every template is saved under a trigger of its own kind. An unmapped stem
/// fails rather than being skipped: a new template nobody saved here is a
/// template nobody checked.
#[tokio::test]
#[serial]
async fn every_editor_template_saves() {
    common::require_db!();
    let app = TestApp::new().await;
    if lua_interpreter(&app).await.is_none() {
        return;
    }

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web/src/lib/lua-templates");
    let mut templates: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "lua"))
        .collect();
    templates.sort();
    assert_eq!(templates.len(), 5, "{templates:?}");

    for path in templates {
        let stem = path.file_stem().unwrap().to_str().unwrap().to_string();
        let trigger = match stem.as_str() {
            "job" => "job.run:test.templates".to_string(),
            "procedure" => "xrpc.procedure:com.example.doThing".to_string(),
            "query" => "xrpc.query:com.example.listThings".to_string(),
            "record-event" => "record.create:com.example.thing".to_string(),
            "trigger" => "xrpc.query:com.example.generic".to_string(),
            other => panic!("no trigger kind mapped for the {other} template"),
        };
        let source = std::fs::read_to_string(&path).unwrap();
        let resp = post_script(&app, &json!({ "id": trigger, "body": source })).await;
        let status = resp.status();
        assert_eq!(
            status,
            StatusCode::CREATED,
            "{}: {}",
            path.display(),
            json_body(resp).await
        );
    }
}

/// The scanner names every removed global at once and offers the codemod,
/// which a refusal from an interpreter cannot do, so it answers first —
/// before the language is even resolved to an interpreter.
#[tokio::test]
#[serial]
async fn the_codemod_scanner_answers_ahead_of_any_interpreter() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = post_script(
        &app,
        &json!({
            "id": "xrpc.procedure:com.example.do",
            "body": "function handle()\n  return { db, json, input }\nend\n",
        }),
    )
    .await;
    let status = resp.status();
    let body = json_body(resp).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let named: Vec<&str> = body["removed_globals"]
        .as_array()
        .expect("the names")
        .iter()
        .map(|name| name.as_str().unwrap())
        .collect();
    assert_eq!(named, ["db", "json", "input"]);
}

// ---------------------------------------------------------------------------
// What the save path sends, and what it reaches
// ---------------------------------------------------------------------------

/// The guard list is the host's, so a name a run refuses cannot be saved as
/// valid even by an interpreter that was sent nothing.
#[tokio::test]
#[serial]
async fn the_hosts_guard_list_reaches_validate() {
    common::require_db!();
    if !echo_interpreter::is_built() {
        eprintln!("skipping: {}", echo_interpreter::BUILD);
        return;
    }
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .register(echo_interpreter::plugin("echo"))
        .await;

    let error = refusal(
        post_script(
            &app,
            &json!({ "id": ECHO_ID, "script_type": "echo", "body": "probe-globals" }),
        )
        .await,
    )
    .await;
    for name in happyview::codemod::REMOVED_GLOBALS {
        assert!(error.contains(name), "{name} missing from {error}");
    }
}

/// Checking a body is not running it, so the imports that act on a run have
/// no run to act on and nothing is attributed to one.
#[tokio::test]
#[serial]
async fn validate_reaches_no_run_and_logs_nothing() {
    common::require_db!();
    if !echo_interpreter::is_built() {
        eprintln!("skipping: {}", echo_interpreter::BUILD);
        return;
    }
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .register(echo_interpreter::plugin("echo"))
        .await;
    let before = script_log_rows(&app).await;

    let error = refusal(
        post_script(
            &app,
            &json!({ "id": ECHO_ID, "script_type": "echo", "body": "probe-log" }),
        )
        .await,
    )
    .await;
    assert!(error.contains("UNSUPPORTED"), "{error}");
    assert_eq!(script_log_rows(&app).await, before);
}

// ---------------------------------------------------------------------------
// The language, and where it comes from
// ---------------------------------------------------------------------------

/// Every language is refused on the same terms, the reference language
/// included: whether an interpreter claims it is the only question a save
/// asks.
#[tokio::test]
#[serial]
async fn a_save_is_refused_for_every_language_no_interpreter_claims() {
    common::require_db!();
    let app = TestApp::new().await;

    for language in ["lua", "typescript"] {
        let error = refusal(
            post_script(
                &app,
                &json!({ "id": LUA_ID, "script_type": language, "body": VALID }),
            )
            .await,
        )
        .await;
        assert!(error.contains(language), "{error}");
        assert!(error.contains("plugins page"), "{error}");
        assert_eq!(
            get_script(&app, LUA_ID).await.status(),
            StatusCode::NOT_FOUND
        );
    }
}

/// A body-only patch is checked by the interpreter the stored row names.
/// Reading the language off the default instead would hand another
/// interpreter's body to Lua, which refuses it for being Lua that it is not.
#[tokio::test]
#[serial]
async fn a_body_only_patch_is_checked_against_the_rows_own_language() {
    common::require_db!();
    if !echo_interpreter::is_built() {
        eprintln!("skipping: {}", echo_interpreter::BUILD);
        return;
    }
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .register(echo_interpreter::plugin("echo"))
        .await;

    let created = post_script(
        &app,
        &json!({ "id": ECHO_ID, "script_type": "echo", "body": "echo one" }),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);

    // Valid to the echo interpreter and not Lua at all, so the two answers
    // differ and the test can tell which one was asked.
    let resp = patch_script(&app, ECHO_ID, &json!({ "body": "echo two {{{" })).await;
    let status = resp.status();
    let body = json_body(resp).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["body"], "echo two {{{");
    assert_eq!(body["script_type"], "echo");
}
