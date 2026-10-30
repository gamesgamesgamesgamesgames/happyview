//! `GET /admin/editor/declarations`: the instance's declarations, rendered by
//! the interpreter that claims a language. The echo fixture's
//! `get_declarations` writes its whole input as `input.json`, so these tests
//! read back what the interpreter was given.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;
use common::echo_interpreter;

/// GET `uri` as the admin, or with `header` when it names its own
/// authorization.
async fn get(app: &TestApp, uri: &str, header: Option<(&str, String)>) -> axum::response::Response {
    let mut req = Request::builder().uri(uri);
    if !matches!(header, Some(("authorization", _))) {
        let cookie = app.admin_cookie();
        req = req.header(cookie.0, cookie.1);
    }
    if let Some((name, value)) = header {
        req = req.header(name, value);
    }
    app.router
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn json_body(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

/// The input the interpreter rendered, out of the echo fixture's one file.
fn rendered_input(body: &Value) -> Value {
    let files = body["files"].as_array().expect("files");
    let file = files
        .iter()
        .find(|f| f["path"] == "input.json")
        .unwrap_or_else(|| panic!("no input.json: {body}"));
    serde_json::from_str(file["contents"].as_str().unwrap()).unwrap()
}

async fn put_lexicon(app: &TestApp, id: &str) {
    app.state
        .lexicons
        .put_manual(
            &app.state.db,
            app.state.db_backend,
            happyview::catalog::LexiconWrite {
                lexicon_json: json!({
                    "lexicon": 1,
                    "id": id,
                    "defs": { "main": { "type": "query", "output": { "encoding": "application/json",
                        "schema": { "type": "object", "properties": { "n": { "type": "integer" } } } } } }
                }),
                backfill: false,
                target_collection: None,
                action: happyview::lexicon::ProcedureAction::Upsert,
                token_cost: None,
                authority_did: None,
                indexed: true,
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
#[serial]
async fn renders_the_instances_lexicons_and_revalidates() {
    common::require_db!();
    if !echo_interpreter::is_built() {
        happyview::test_support::unavailable(echo_interpreter::BUILD);
        return;
    }
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .register(echo_interpreter::plugin("echo"))
        .await;
    put_lexicon(&app, "com.example.first").await;

    let resp = get(&app, "/admin/editor/declarations?language=echo", None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let etag = resp.headers()[header::ETAG].to_str().unwrap().to_string();
    let body = json_body(resp).await;
    assert_eq!(body["language_id"], "echo");
    let input = rendered_input(&body);
    let nsids: Vec<&str> = input["lexicons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["nsid"].as_str().unwrap())
        .collect();
    assert_eq!(nsids, ["com.example.first"], "{input}");
    assert!(input["ctx"]["query"].is_object(), "{input}");

    // Nothing changed, so the dashboard's copy is still good.
    let again = get(
        &app,
        "/admin/editor/declarations?language=echo",
        Some(("if-none-match", etag.clone())),
    )
    .await;
    assert_eq!(again.status(), StatusCode::NOT_MODIFIED);

    // A new lexicon is a new input, and the next request renders it.
    put_lexicon(&app, "com.example.second").await;
    let changed = get(
        &app,
        "/admin/editor/declarations?language=echo",
        Some(("if-none-match", etag.clone())),
    )
    .await;
    assert_eq!(changed.status(), StatusCode::OK);
    assert_ne!(changed.headers()[header::ETAG].to_str().unwrap(), etag);
    let input = rendered_input(&json_body(changed).await);
    assert_eq!(input["lexicons"].as_array().unwrap().len(), 2, "{input}");
}

#[tokio::test]
#[serial]
async fn a_language_nothing_claims_is_not_found() {
    common::require_db!();
    let app = TestApp::new().await;
    let resp = get(&app, "/admin/editor/declarations?language=cobol", None).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[serial]
async fn reading_declarations_needs_scripts_read() {
    common::require_db!();
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .register(echo_interpreter::claiming("echo"))
        .await;
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
                    json!({ "name": "stats", "permissions": ["stats:read"] }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let key = json_body(resp).await["key"].as_str().unwrap().to_string();

    let resp = get(
        &app,
        "/admin/editor/declarations?language=echo",
        Some(("authorization", format!("Bearer {key}"))),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}
