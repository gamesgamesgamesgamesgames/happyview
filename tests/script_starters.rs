//! The JavaScript and TypeScript bodies the new-script form prefills, saved
//! through `/admin/scripts` and then run by the real interpreter plugins, so a
//! starter is one an operator can save as it stands and that does what its
//! comments say. The Lua starters are held the same way in
//! `admin_scripts_validate.rs`.
//!
//! ```sh
//! HAPPYVIEW_JAVASCRIPT_PLUGIN=<dir> HAPPYVIEW_TYPESCRIPT_PLUGIN=<dir> \
//!   TEST_DATABASE_URL=<url> cargo test --test script_starters
//! ```

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;
use common::quickjs_plugins::{JAVASCRIPT_DIR, TYPESCRIPT_DIR, plugin_dir};
use common::workers::Workers;
use happyview::plugin::{
    ScriptExecuteContext, ScriptExecuteInput, ScriptExecuteLimits, ScriptExecuteOutput, ScriptKind,
    ScriptValueKind, loader,
};

const JOB_TYPE: &str = "test.starter";

/// The three starters each language has, by the stem the form imports.
const STARTERS: [&str; 3] = ["job", "record-event", "trigger"];

fn starter(language: &str, ext: &str, stem: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("web/src/lib/script-templates")
        .join(language)
        .join(format!("{stem}.{ext}.txt"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Every starter on disk is one this file runs: a new one nobody ran here is
/// one nobody checked.
#[test]
fn every_starter_on_disk_is_covered() {
    for (language, ext) in [("javascript", "js"), ("typescript", "ts")] {
        let dir: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("web/src/lib/script-templates")
            .join(language);
        let mut found: Vec<String> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        found.sort();
        let expected: Vec<String> = STARTERS
            .iter()
            .map(|stem| format!("{stem}.{ext}.txt"))
            .collect();
        assert_eq!(found, expected, "{language}");
    }
}

async fn json_body(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

/// Save `body` as the form would, asserting the save is accepted.
async fn save(app: &TestApp, id: &str, language: &str, body: &str) {
    let cookie = app.admin_cookie();
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/scripts")
                .header(cookie.0, cookie.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "id": id,
                        "script_type": language,
                        "body": body,
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{language} {id}: {}",
        json_body(resp).await
    );
}

async fn register(app: &TestApp, var: &str) -> Option<String> {
    let dir = plugin_dir(var)?;
    let plugin = loader::load_from_file(&dir)
        .await
        .expect("the plugin should load through the loader");
    let id = plugin.info.id.clone();
    app.state.plugin_registry.register(plugin).await;
    Some(id)
}

async fn run(
    app: &TestApp,
    plugin: &str,
    source: &str,
    kind: ScriptKind,
    trigger: &str,
    input: Value,
) -> ScriptExecuteOutput {
    let sent = ScriptExecuteInput {
        source: source.to_string(),
        // What a stored script runs under, rather than the trigger id, which
        // QuickJS would read as an import specifier.
        chunk_name: happyview::catalog::DASHBOARD_CHUNK_NAME.to_string(),
        artifact: None,
        kind,
        input,
        context: ScriptExecuteContext {
            trigger: trigger.to_string(),
            collection: Some("com.example.thing".into()),
            ..ScriptExecuteContext::default()
        },
        libraries: Vec::new(),
        limits: ScriptExecuteLimits {
            instructions: Some(1_000_000),
            memory_bytes: 64 * 1024 * 1024,
        },
        removed_globals: Vec::new(),
    };
    app.state
        .plugin_executor()
        .execute_script(plugin, &sent)
        .await
        .expect("the interpreter should answer")
}

/// A query answers with what the trigger starter returns, which is its input.
async fn trigger_returns_its_input(app: &TestApp, plugin: &str, language: &str, ext: &str) {
    let source = starter(language, ext, "trigger");
    let id = "xrpc.query:com.example.starter";
    save(app, id, language, &source).await;
    assert_eq!(
        run(
            app,
            plugin,
            &source,
            ScriptKind::XrpcQuery,
            id,
            json!({ "q": "hi" })
        )
        .await,
        ScriptExecuteOutput::Returned {
            value: json!({ "q": "hi" }),
            value_kind: ScriptValueKind::Object,
        },
        "{language}"
    );
}

/// A create indexes the record as it arrived, and a delete, which carries no
/// record, still proceeds rather than being skipped.
async fn record_event_proceeds(app: &TestApp, plugin: &str, language: &str, ext: &str) {
    let source = starter(language, ext, "record-event");
    let id = "record.index:com.example.thing";
    save(app, id, language, &source).await;
    let event = |action: &str, record: Option<Value>| {
        let mut e = json!({
            "action": action,
            "uri": "at://did:plc:abc/com.example.thing/3k",
            "did": "did:plc:abc",
            "collection": "com.example.thing",
            "rkey": "3k",
        });
        if let Some(record) = record {
            e["record"] = record;
        }
        e
    };

    let record = json!({ "$type": "com.example.thing", "text": "hello" });
    assert_eq!(
        run(
            app,
            plugin,
            &source,
            ScriptKind::RecordEvent,
            id,
            event("create", Some(record.clone()))
        )
        .await,
        ScriptExecuteOutput::Returned {
            value: record,
            value_kind: ScriptValueKind::Object,
        },
        "{language} create"
    );
    assert_eq!(
        run(
            app,
            plugin,
            &source,
            ScriptKind::RecordEvent,
            id,
            event("delete", None)
        )
        .await,
        ScriptExecuteOutput::Returned {
            value: json!(true),
            value_kind: ScriptValueKind::Other,
        },
        "{language} delete"
    );
}

/// The job starter run by the real worker: it reports progress, and what it
/// returns becomes the job's result.
async fn job_completes(app: &TestApp, language: &str, ext: &str) {
    let source = starter(language, ext, "job");
    save(app, &format!("job.run:{JOB_TYPE}"), language, &source).await;

    let job = Uuid::new_v4().to_string();
    let now = happyview::db::now_rfc3339();
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_jobs (id, job_type, status, input, progress, created_by, created_at) \
         VALUES (?, ?, 'pending', '{}', '{}', ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(&job)
        .bind(JOB_TYPE)
        .bind(&app.admin_did)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .expect("enqueue a job");

    let workers = Workers::one(&app.state);
    let sql = happyview::db::adapt_sql(
        "SELECT status, result, error, progress FROM happyview_jobs WHERE id = ?",
        app.state.db_backend,
    );
    let mut row = (String::new(), None, None, String::new());
    for _ in 0..600 {
        row = happyview::db::query_as::<(String, Option<String>, Option<String>, String)>(&sql)
            .bind(&job)
            .fetch_one(&app.state.db)
            .await
            .expect("read the job row");
        if !matches!(row.0.as_str(), "pending" | "running") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    workers.stop().await;

    let (status, result, error, progress) = row;
    assert_eq!(status, "completed", "{language}: {error:?}");
    let result: Value = serde_json::from_str(result.as_deref().expect("a result")).unwrap();
    assert_eq!(result, json!({ "done": true }), "{language}");
    let progress: Value = serde_json::from_str(&progress).unwrap();
    assert_eq!(progress, json!({ "status": "working" }), "{language}");
}

async fn starters_save_and_run(var: &str, language: &str, ext: &str) {
    common::require_db!();
    let app = TestApp::new().await;
    let Some(plugin) = register(&app, var).await else {
        return;
    };
    trigger_returns_its_input(&app, &plugin, language, ext).await;
    record_event_proceeds(&app, &plugin, language, ext).await;
    job_completes(&app, language, ext).await;
}

#[tokio::test]
#[serial]
async fn the_javascript_starters_save_and_run() {
    starters_save_and_run(JAVASCRIPT_DIR, "javascript", "js").await;
}

#[tokio::test]
#[serial]
async fn the_typescript_starters_save_and_run() {
    starters_save_and_run(TYPESCRIPT_DIR, "typescript", "ts").await;
}
