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
        sent["source"],
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
/// back with its file and line, and a module (not yet validated) is stored.
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
