//! A project built from files and served through the real router.
//!
//! The `interpreter_echo` fixture claims `.echo` files and echoes its whole
//! `execute` input back, so a response shows which file answered and what
//! the host handed it. The real Lua plugin, when `HAPPYVIEW_LUA_PLUGIN`
//! names it, shows that a real interpreter's verdicts reach the build.

mod common;

use std::path::Path;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

use common::app::TestApp;
use common::echo_interpreter;
use happyview::project::{BuildOptions, build_snapshot, install};

const BASIC: &str = "tests/fixtures/projects/basic";

macro_rules! require_fixture {
    () => {
        if !echo_interpreter::is_built() {
            eprintln!("skipping: {}", echo_interpreter::BUILD);
            return;
        }
    };
}

async fn json_body(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn a_project_query_is_served_by_its_file_with_its_overrides() {
    common::require_db!();
    require_fixture!();
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .register(echo_interpreter::plugin_with_extensions("echo", &["echo"]))
        .await;

    let report = build_snapshot(&app.state, Path::new(BASIC), BuildOptions::default())
        .await
        .unwrap_or_else(|d| {
            panic!(
                "{}",
                d.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        });
    install(&app.state, report.snapshot).await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/xrpc/com.example.project.getThing")
                .header("x-client-key", "hvc_test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let sent = json_body(resp).await;
    assert_eq!(
        echo_interpreter::script_text(&sent),
        std::fs::read_to_string(format!(
            "{BASIC}/src/lexicons/com/example/project/getThing.echo"
        ))
        .unwrap()
    );
    assert_eq!(
        sent["context"]["trigger"],
        "xrpc.query:com.example.project.getThing"
    );
    // From happyview.toml, not the lexicon file.
    assert_eq!(sent["context"]["collection"], "com.example.project.thing");

    assert!(
        app.state
            .lexicons
            .get_space_declaration("com.example.project.space")
            .await
            .is_some()
    );
}

/// The real interpreter's verdicts: a handler builds, a syntax error comes
/// back with its file and line, and a module with no `handle` is stored.
#[tokio::test]
async fn a_lua_project_builds_through_the_real_interpreter() {
    let Some(dir) = common::lua_plugin::plugin_dir() else {
        return;
    };
    let state =
        happyview::test_support::test_state_with_pool(happyview::test_support::memory_pool().await);
    state
        .plugin_registry
        .install(
            happyview::plugin::loader::load_from_file(&dir)
                .await
                .unwrap(),
        )
        .await
        .unwrap();

    let root = tempfile::tempdir().unwrap();
    let write = |rel: &str, contents: &str| {
        let path = root.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    };
    write(
        "src/lexicons/com/example/getThing.json",
        r#"{"lexicon":1,"id":"com.example.getThing","defs":{"main":{"type":"query"}}}"#,
    );
    write(
        "src/lexicons/com/example/getThing.lua",
        "-- Answers a thing.\nfunction handle(input, ctx)\n  return { ok = true }\nend\n",
    );
    write("src/lib/format.lua", "local M = {}\nreturn M\n");

    let report = build_snapshot(&state, root.path(), BuildOptions::default())
        .await
        .unwrap();
    assert_eq!(
        report.snapshot.scripts["xrpc.query:com.example.getThing"]
            .description
            .as_deref(),
        Some("Answers a thing.")
    );
    assert!(report.snapshot.modules.contains_key("src/lib/format.lua"));

    write(
        "src/lexicons/com/example/getThing.lua",
        "function handle(input, ctx)\n  return {\nend\n",
    );
    let errors = build_snapshot(&state, root.path(), BuildOptions::default())
        .await
        .err()
        .unwrap();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(
        errors[0].path,
        Path::new("src/lexicons/com/example/getThing.lua")
    );
    assert!(
        errors[0].line.is_some(),
        "the interpreter names a line: {}",
        errors[0]
    );

    write(
        "src/lexicons/com/example/getThing.lua",
        "function handle()\n  return db.get(params.uri)\nend\n",
    );
    let errors = build_snapshot(&state, root.path(), BuildOptions::default())
        .await
        .err()
        .unwrap();
    assert!(
        errors[0].message.contains("removed globals: db, params"),
        "{}",
        errors[0]
    );
}

async fn post_admin(app: &TestApp, uri: &str, body: &Value) -> axum::response::Response {
    let cookie = app.admin_cookie();
    app.router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(cookie.0, cookie.1)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// A dashboard lexicon and a dashboard script the project overrides are
/// reported, and kept.
#[tokio::test]
async fn boot_reports_what_the_project_shadows() {
    common::require_db!();
    require_fixture!();
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .register(echo_interpreter::plugin_with_extensions("lua", &["lua"]))
        .await;

    let lexicon = serde_json::json!({
        "lexicon": 1,
        "id": "com.example.project.thing",
        "defs": {"main": {"type": "record", "key": "tid", "record": {"type": "object", "properties": {}}}}
    });
    let resp = post_admin(
        &app,
        "/admin/lexicons",
        &serde_json::json!({"lexicon_json": lexicon}),
    )
    .await;
    assert!(resp.status().is_success(), "{}", resp.status());

    let body = "function handle(input, ctx)\n  return input\nend\n";
    let resp = post_admin(
        &app,
        "/admin/scripts",
        &serde_json::json!({
            "id": "record.index:com.example.project.thing",
            "script_type": "lua",
            "body": body,
        }),
    )
    .await;
    let status = resp.status();
    assert!(status.is_success(), "{status}: {}", json_body(resp).await);

    let root = tempfile::tempdir().unwrap();
    let write = |rel: &str, contents: &str| {
        let path = root.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    };
    write(
        "src/lexicons/com/example/project/thing.json",
        &lexicon.to_string(),
    );
    write("src/lexicons/com/example/project/thing.index.lua", body);

    let installed = happyview::project::boot::build_and_install(&app.state, root.path())
        .await
        .unwrap_or_else(|d| {
            panic!(
                "{}",
                d.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        });
    let shown: Vec<String> = installed.warnings.iter().map(ToString::to_string).collect();
    assert_eq!(
        shown,
        [
            "src/lexicons/com/example/project/thing.index.lua: warning: overrides the dashboard \
             script record.index:com.example.project.thing, which answers again if this file is \
             removed",
            "src/lexicons/com/example/project/thing.json: warning: overrides the dashboard lexicon \
             com.example.project.thing, which answers again if this file is removed",
        ]
    );
}

#[tokio::test]
async fn a_declared_backfill_starts_once() {
    common::require_db!();
    let app = TestApp::new().await;
    let root = tempfile::tempdir().unwrap();
    let write = |rel: &str, contents: &str| {
        let path = root.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    };
    write(
        "happyview.toml",
        "[lexicons.\"com.example.project.thing\"]\nbackfill = true\n",
    );
    write(
        "src/lexicons/com/example/project/thing.json",
        r#"{"lexicon":1,"id":"com.example.project.thing","defs":{"main":{"type":"record","key":"tid","record":{"type":"object","properties":{}}}}}"#,
    );
    write(
        "src/lexicons/com/example/project/other.json",
        r#"{"lexicon":1,"id":"com.example.project.other","defs":{"main":{"type":"record","key":"tid","record":{"type":"object","properties":{}}}}}"#,
    );
    happyview::project::boot::build_and_install(&app.state, root.path())
        .await
        .unwrap();

    let started = happyview::project::boot::start_declared_backfills(&app.state).await;
    assert_eq!(started, ["com.example.project.thing"]);
    let (jobs,): (i64,) = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT COUNT(*) FROM happyview_backfill_jobs WHERE collection = ?",
        app.state.db_backend,
    ))
    .bind("com.example.project.thing")
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(jobs, 1);

    // The next boot finds the job and starts nothing.
    assert!(
        happyview::project::boot::start_declared_backfills(&app.state)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn a_backfill_of_named_accounts_does_not_stand_in_for_the_declared_one() {
    common::require_db!();
    let app = TestApp::new().await;
    let root = tempfile::tempdir().unwrap();
    let write = |rel: &str, contents: &str| {
        let path = root.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    };
    write(
        "happyview.toml",
        "[lexicons.\"com.example.project.named\"]\nbackfill = true\n",
    );
    write(
        "src/lexicons/com/example/project/named.json",
        r#"{"lexicon":1,"id":"com.example.project.named","defs":{"main":{"type":"record","key":"tid","record":{"type":"object","properties":{}}}}}"#,
    );
    happyview::project::boot::build_and_install(&app.state, root.path())
        .await
        .unwrap();
    happyview::db::query(&happyview::db::adapt_sql(
        "INSERT INTO happyview_backfill_jobs (id, collection, did, scope, status, created_at) \
         VALUES (?, ?, ?, 'dids', 'completed', ?)",
        app.state.db_backend,
    ))
    .bind("named-accounts-job")
    .bind("com.example.project.named")
    .bind("did:plc:someone")
    .bind(happyview::db::now_rfc3339())
    .execute(&app.state.db)
    .await
    .unwrap();

    let started = happyview::project::boot::start_declared_backfills(&app.state).await;
    assert_eq!(started, ["com.example.project.named"]);
}
