//! What `/admin/scripts` does about the language a row names: refusing to
//! store a body no installed interpreter could check or run, and reporting
//! whether a stored row can run at all.
//!
//! The read path only asks the registry which languages are installed, so the
//! interpreter it sees claims a language and carries no module. The save path
//! asks the interpreter itself whether the body is one it can run, so the one
//! test that gets as far as storing a row carries the fixture's module.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;
use common::echo_interpreter;

const ID: &str = "xrpc.query:com.example.list";
const BODY: &str = "function handle(input, ctx)\n  return input.q\nend\n";

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

async fn list_scripts(app: &TestApp) -> Vec<Value> {
    let cookie = app.admin_cookie();
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/scripts")
                .header(cookie.0, cookie.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    json_body(resp).await.as_array().cloned().unwrap()
}

/// Insert a row for a language the instance cannot run, which is the state a
/// database arrives in when its interpreter was uninstalled.
async fn seed_row(app: &TestApp, id: &str, script_type: &str) {
    let now = happyview::db::now_rfc3339();
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_scripts (id, script_type, body, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(id)
        .bind(script_type)
        .bind(BODY)
        .bind(&now)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .expect("seed a script row");
}

// ---------------------------------------------------------------------------
// Saving
// ---------------------------------------------------------------------------

/// Nothing could check this body and nothing could run it, so the refusal
/// names the language and where an operator goes to fix it.
#[tokio::test]
#[serial]
async fn a_create_for_a_language_with_no_interpreter_is_refused_and_stores_nothing() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = post_script(
        &app,
        &json!({ "id": ID, "script_type": "typescript", "body": BODY }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let error = json_body(resp).await["error"]
        .as_str()
        .expect("a message")
        .to_string();
    assert!(error.contains("typescript"), "{error}");
    assert!(error.contains("plugins page"), "{error}");

    assert_eq!(get_script(&app, ID).await.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[serial]
async fn a_patch_onto_a_language_with_no_interpreter_is_refused_and_stores_nothing() {
    common::require_db!();
    let app = TestApp::new().await;
    seed_row(&app, ID, "lua").await;

    let resp = patch_script(
        &app,
        ID,
        &json!({ "script_type": "typescript", "body": "function handle() end\n" }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let error = json_body(resp).await["error"]
        .as_str()
        .expect("a message")
        .to_string();
    assert!(error.contains("typescript"), "{error}");
    assert!(error.contains("plugins page"), "{error}");

    let row = json_body(get_script(&app, ID).await).await;
    assert_eq!(row["script_type"], "lua");
    assert_eq!(row["body"], BODY);
}

/// The language is the only thing that was wrong, so installing its
/// interpreter is the whole fix.
#[tokio::test]
#[serial]
async fn installing_the_interpreter_lets_the_same_body_save() {
    common::require_db!();
    if !echo_interpreter::is_built() {
        eprintln!("skipping: {}", echo_interpreter::BUILD);
        return;
    }
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .register(echo_interpreter::plugin("typescript"))
        .await;

    let resp = post_script(
        &app,
        &json!({ "id": ID, "script_type": "typescript", "body": BODY }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let row = json_body(resp).await;
    assert_eq!(row["script_type"], "typescript");
    assert_eq!(row["runnable"], true);
}

// ---------------------------------------------------------------------------
// Reading a row the instance cannot run
// ---------------------------------------------------------------------------

/// A row whose interpreter is absent is inert rather than gone: an operator
/// has to be able to read it to decide whether to install the interpreter or
/// delete the script.
#[tokio::test]
#[serial]
async fn a_row_for_an_uninstalled_language_still_lists_and_still_reads() {
    common::require_db!();
    let app = TestApp::new().await;
    seed_row(&app, ID, "typescript").await;

    let row = json_body(get_script(&app, ID).await).await;
    assert_eq!(row["script_type"], "typescript");
    assert_eq!(row["body"], BODY);
    assert_eq!(row["runnable"], false);

    let listed = list_scripts(&app).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], ID);
    assert_eq!(listed[0]["runnable"], false);
}

#[tokio::test]
#[serial]
async fn runnable_follows_the_installed_interpreters_rather_than_the_row() {
    common::require_db!();
    let app = TestApp::new().await;
    seed_row(&app, ID, "typescript").await;
    seed_row(&app, "xrpc.query:com.example.other", "lua").await;

    app.state
        .plugin_registry
        .register(echo_interpreter::claiming("lua"))
        .await;

    let listed = list_scripts(&app).await;
    let runnable: Vec<(&str, &Value)> = listed
        .iter()
        .map(|row| (row["script_type"].as_str().unwrap(), &row["runnable"]))
        .collect();
    // Ordered by trigger id, which `ID` sorts ahead of.
    assert_eq!(
        runnable,
        [("typescript", &json!(false)), ("lua", &json!(true))]
    );
}
