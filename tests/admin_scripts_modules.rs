//! `GET /admin/scripts/modules`: the interpreter modules of the project the
//! instance serves, with the source their files hold, so a client can rebuild
//! the project's `src/` from the instance.

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;
use happyview::plugin::PreparedArtifact;
use happyview::project::{ProjectModule, ProjectSnapshot};

async fn json_body(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

/// GET the route as the admin, or with `bearer` when it is given.
async fn list_modules(app: &TestApp, bearer: Option<&str>) -> axum::response::Response {
    let mut req = Request::builder().uri("/admin/scripts/modules");
    match bearer {
        Some(key) => req = req.header("authorization", format!("Bearer {key}")),
        None => {
            let cookie = app.admin_cookie();
            req = req.header(cookie.0, cookie.1);
        }
    }
    app.router
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

fn module(id: &str, language_id: &str, source: &str) -> ProjectModule {
    ProjectModule {
        id: id.to_string(),
        path: PathBuf::from(id),
        language_id: language_id.to_string(),
        source: source.into(),
        // What an interpreter prepared is not the project's to hand back, so
        // one is set here to show it stays out of the response.
        artifact: Some(PreparedArtifact {
            interpreter: language_id.to_string(),
            version: "1.0.0".into(),
            engine: "test".into(),
            format: "test-bytecode".into(),
            bytes: vec![1, 2, 3],
            source_map: None,
        }),
    }
}

/// Serve a project holding `modules`, as a deploy would.
fn serve_modules(app: &TestApp, modules: Vec<ProjectModule>) {
    let modules: BTreeMap<String, ProjectModule> =
        modules.into_iter().map(|m| (m.id.clone(), m)).collect();
    let snapshot = ProjectSnapshot {
        root: Some(PathBuf::from("/project")),
        generation: 1,
        modules,
        ..ProjectSnapshot::empty()
    };
    app.state.project.store(Arc::new(snapshot));
}

#[tokio::test]
#[serial]
async fn lists_the_projects_modules_with_their_source_sorted_by_id() {
    common::require_db!();
    let app = TestApp::new().await;
    serve_modules(
        &app,
        vec![
            module(
                "src/lib/text/format.ts",
                "typescript",
                "export const f = 1;\n",
            ),
            module("src/lib/format.lua", "lua", "return {}\n"),
        ],
    );

    let resp = list_modules(&app, None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        json_body(resp).await,
        json!([
            {
                "id": "src/lib/format.lua",
                "path": "src/lib/format.lua",
                "language": "lua",
                "source": "return {}\n",
            },
            {
                "id": "src/lib/text/format.ts",
                "path": "src/lib/text/format.ts",
                "language": "typescript",
                "source": "export const f = 1;\n",
            },
        ])
    );
}

/// The response carries the project's files and nothing an interpreter made
/// from them.
#[tokio::test]
#[serial]
async fn a_module_answers_without_its_prepared_artifact() {
    common::require_db!();
    let app = TestApp::new().await;
    serve_modules(
        &app,
        vec![module("src/lib/format.lua", "lua", "return {}\n")],
    );

    let body = json_body(list_modules(&app, None).await).await;
    let entry = body[0].as_object().expect("one module");
    let mut keys: Vec<&str> = entry.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["id", "language", "path", "source"]);
}

/// With no project deployed there is nothing to list, and that is not an
/// error.
#[tokio::test]
#[serial]
async fn is_empty_with_no_project_deployed() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = list_modules(&app, None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json_body(resp).await, json!([]));
}

/// Module source is script source, so it takes what reading a script's body
/// takes.
#[tokio::test]
#[serial]
async fn listing_modules_needs_scripts_read() {
    common::require_db!();
    let app = TestApp::new().await;
    serve_modules(
        &app,
        vec![module("src/lib/format.lua", "lua", "return {}\n")],
    );

    let key = app_key(&app, &["stats:read"]).await;
    let resp = list_modules(&app, Some(&key)).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let key = app_key(&app, &["scripts:read"]).await;
    let resp = list_modules(&app, Some(&key)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json_body(resp).await.as_array().unwrap().len(), 1);
}

/// An API key holding exactly `permissions`.
async fn app_key(app: &TestApp, permissions: &[&str]) -> String {
    let cookie = app.admin_cookie();
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/api-keys")
                .header(cookie.0, cookie.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "name": permissions.join(","), "permissions": permissions })
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    json_body(resp).await["key"].as_str().unwrap().to_string()
}
