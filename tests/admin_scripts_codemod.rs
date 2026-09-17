//! `POST /admin/scripts/{id}/codemod` and the `needs_migration` field on
//! `GET /admin/scripts`, through `TestApp` and guarded by
//! `common::require_db!()` so a normal `cargo test` run exercises them
//! whenever `TEST_DATABASE_URL` is set.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn json_body(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

fn admin_get(
    uri: &str,
    cookie: (axum::http::HeaderName, axum::http::HeaderValue),
) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(cookie.0, cookie.1)
        .body(Body::empty())
        .unwrap()
}

fn admin_post(
    uri: &str,
    cookie: (axum::http::HeaderName, axum::http::HeaderValue),
    body: &Value,
) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(cookie.0, cookie.1)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

/// A POST with no body and no `Content-Type` at all, distinct from
/// `admin_post(..., &json!({}))`'s two-byte `{}` with an explicit
/// `application/json` header. Axum's `Option<Json<T>>` resolves to `None`
/// only when `Content-Type` is absent entirely, so this is the case that
/// actually exercises the preview default.
fn admin_post_no_body(
    uri: &str,
    cookie: (axum::http::HeaderName, axum::http::HeaderValue),
) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(cookie.0, cookie.1)
        .body(Body::empty())
        .unwrap()
}

fn bearer_post(uri: &str, key: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("authorization", format!("Bearer {key}"))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

async fn create_script(app: &TestApp, id: &str, body: &str) -> Value {
    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/scripts",
            app.admin_cookie(),
            &json!({ "id": id, "body": body }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "create '{id}' failed; body: {:?}",
        json_body(resp).await
    );
    let resp = app
        .router
        .clone()
        .oneshot(admin_get(
            &format!("/admin/scripts/{}", urlencoding::encode(id)),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    json_body(resp).await
}

/// Seed a script row with an arbitrary `script_type`, bypassing
/// `POST /admin/scripts` — the only way to get a non-Lua row into the table,
/// since `ScriptLanguage` has no such variant to submit through the API.
async fn seed_script_with_type(app: &TestApp, id: &str, script_type: &str, body: &str) {
    let now = happyview::db::now_rfc3339();
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_scripts (id, script_type, body, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(id)
        .bind(script_type)
        .bind(body)
        .bind(&now)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .expect("seed_script_with_type: insert failed");
}

/// Mints a scoped API key via `POST /admin/api-keys`, the same mechanism
/// `super_user_api_key_is_bounded_by_its_permissions` (tests/e2e_admin.rs)
/// uses — it is the only way to exercise a specific permission set without
/// a second logged-in user.
async fn scoped_api_key(app: &TestApp, name: &str, permissions: &[&str]) -> String {
    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/api-keys",
            app.admin_cookie(),
            &json!({ "name": name, "permissions": permissions }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    json_body(resp).await["key"]
        .as_str()
        .expect("api key returned")
        .to_string()
}

// ---------------------------------------------------------------------------
// Preview / apply
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn codemod_preview_does_not_store() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = "xrpc.query:com.example.list";
    let original = "function handle()\n  return params.q\nend\n";
    create_script(&app, id, original).await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/scripts/{}/codemod", urlencoding::encode(id)),
            app.admin_cookie(),
            &json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let result = json_body(resp).await;
    assert_eq!(result["changed"], true);
    assert!(
        result["source"].as_str().unwrap().contains("input.q"),
        "expected the preview to rewrite params.q to input.q: {result:?}"
    );

    // The stored row must be untouched by a preview.
    let row = app
        .router
        .clone()
        .oneshot(admin_get(
            &format!("/admin/scripts/{}", urlencoding::encode(id)),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    let row = json_body(row).await;
    assert_eq!(row["body"], original);
}

/// A genuinely empty POST (no body, no `Content-Type`) must resolve to a
/// preview, not a 415/422 — the endpoint is usable from a plain `curl -X
/// POST` with nothing else.
#[tokio::test]
#[serial]
async fn codemod_preview_with_empty_body_and_no_content_type() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = "xrpc.query:com.example.list";
    let original = "function handle()\n  return params.q\nend\n";
    create_script(&app, id, original).await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_post_no_body(
            &format!("/admin/scripts/{}/codemod", urlencoding::encode(id)),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let result = json_body(resp).await;
    assert_eq!(result["changed"], true);

    let row = app
        .router
        .clone()
        .oneshot(admin_get(
            &format!("/admin/scripts/{}", urlencoding::encode(id)),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    let row = json_body(row).await;
    assert_eq!(row["body"], original, "an empty body must not apply");
}

#[tokio::test]
#[serial]
async fn codemod_apply_stores_and_next_get_shows_no_needs_migration() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = "xrpc.query:com.example.list";
    create_script(&app, id, "function handle()\n  return params.q\nend\n").await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/scripts/{}/codemod", urlencoding::encode(id)),
            app.admin_cookie(),
            &json!({ "apply": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let result = json_body(resp).await;
    assert_eq!(result["changed"], true);
    let applied_source = result["source"].as_str().unwrap().to_string();

    let row = app
        .router
        .clone()
        .oneshot(admin_get(
            &format!("/admin/scripts/{}", urlencoding::encode(id)),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    let row = json_body(row).await;
    assert_eq!(row["body"], applied_source);

    let list = app
        .router
        .clone()
        .oneshot(admin_get("/admin/scripts", app.admin_cookie()))
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let list = json_body(list).await;
    let entry = list
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .expect("script present in list");
    assert_eq!(
        entry["needs_migration"],
        json!([]),
        "a migrated script must report no remaining migration work: {entry:?}"
    );
}

#[tokio::test]
#[serial]
async fn codemod_rejects_non_lua_script() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = "xrpc.query:com.example.list";
    seed_script_with_type(&app, id, "javascript", "function handle() {}").await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/scripts/{}/codemod", urlencoding::encode(id)),
            app.admin_cookie(),
            &json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let err = json_body(resp).await;
    let msg = err["error"].as_str().unwrap_or("");
    assert!(
        msg.contains("codemod applies to Lua scripts"),
        "expected the Lua-only error, got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Marker guard: apply refuses a rewrite that still has markers unless
// `allow_markers` is set.
// ---------------------------------------------------------------------------

/// A TID conversion, which no rename can carry over because the two sides
/// differ in precision — so this rewrites and still leaves a marker.
const MARKED_SOURCE: &str = "function handle()\n  return TID.toNumber(params.tid)\nend\n";

#[tokio::test]
#[serial]
async fn codemod_apply_refuses_when_markers_remain() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = "xrpc.procedure:com.example.create";
    create_script(&app, id, MARKED_SOURCE).await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/scripts/{}/codemod", urlencoding::encode(id)),
            app.admin_cookie(),
            &json!({ "apply": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let err = json_body(resp).await;
    let msg = err["error"].as_str().unwrap_or("");
    assert!(
        msg.contains('1'),
        "expected the marker count in the message, got: {msg}"
    );

    let row = app
        .router
        .clone()
        .oneshot(admin_get(
            &format!("/admin/scripts/{}", urlencoding::encode(id)),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    let row = json_body(row).await;
    assert_eq!(row["body"], MARKED_SOURCE, "a refused apply must not store");
}

#[tokio::test]
#[serial]
async fn codemod_apply_with_allow_markers_stores_despite_markers() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = "xrpc.procedure:com.example.create";
    create_script(&app, id, MARKED_SOURCE).await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/scripts/{}/codemod", urlencoding::encode(id)),
            app.admin_cookie(),
            &json!({ "apply": true, "allow_markers": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let result = json_body(resp).await;
    assert_eq!(result["changed"], true);
    assert!(!result["notes"].as_array().unwrap().is_empty());

    let row = app
        .router
        .clone()
        .oneshot(admin_get(
            &format!("/admin/scripts/{}", urlencoding::encode(id)),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    let row = json_body(row).await;
    assert!(
        row["body"].as_str().unwrap().contains("-- codemod:"),
        "expected the marker comment to be stored: {row:?}"
    );
}

/// Applying a rewrite that has already landed writes nothing, so the marker
/// guard has nothing to protect and the call succeeds. Refusing it would
/// report a settled script as a new conflict on every retry.
#[tokio::test]
#[serial]
async fn codemod_apply_a_second_time_is_a_no_op_rather_than_a_conflict() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = "xrpc.procedure:com.example.create";
    create_script(&app, id, MARKED_SOURCE).await;

    let apply = |body: Value| {
        let router = app.router.clone();
        let cookie = app.admin_cookie();
        async move {
            router
                .oneshot(admin_post(
                    &format!("/admin/scripts/{}/codemod", urlencoding::encode(id)),
                    cookie,
                    &body,
                ))
                .await
                .unwrap()
        }
    };

    let first = apply(json!({ "apply": true, "allow_markers": true })).await;
    assert_eq!(first.status(), StatusCode::OK);
    let first = json_body(first).await;
    assert_eq!(first["changed"], true);

    let second = apply(json!({ "apply": true })).await;
    assert_eq!(second.status(), StatusCode::OK);
    let second = json_body(second).await;
    assert_eq!(second["changed"], false);
    assert!(
        !second["notes"].as_array().unwrap().is_empty(),
        "the marker is still reported: {second:?}"
    );
    assert_eq!(second["source"], first["source"]);
}

// ---------------------------------------------------------------------------
// needs_migration on GET /admin/scripts
// ---------------------------------------------------------------------------

/// A script whose `needs_migration` list is empty must show that from
/// `GET /admin/scripts` even without ever calling the codemod endpoint on it
/// — a freshly-authored v3 script never needs the trip.
#[tokio::test]
#[serial]
async fn list_reports_needs_migration_for_an_unmigrated_script() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = "xrpc.query:com.example.list";
    create_script(&app, id, "function handle()\n  return params.q\nend\n").await;

    let list = app
        .router
        .clone()
        .oneshot(admin_get("/admin/scripts", app.admin_cookie()))
        .await
        .unwrap();
    let list = json_body(list).await;
    let entry = list
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .expect("script present in list");
    let needs_migration = entry["needs_migration"].as_array().unwrap();
    assert!(
        needs_migration.iter().any(|v| v.as_str() == Some("params")),
        "expected 'params' in needs_migration, got: {entry:?}"
    );
}

#[tokio::test]
#[serial]
async fn list_reports_empty_needs_migration_for_a_non_lua_script() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = "xrpc.query:com.example.list";
    seed_script_with_type(
        &app,
        id,
        "javascript",
        "function handle() { return params.q; }",
    )
    .await;

    let list = app
        .router
        .clone()
        .oneshot(admin_get("/admin/scripts", app.admin_cookie()))
        .await
        .unwrap();
    let list = json_body(list).await;
    let entry = list
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .expect("script present in list");
    assert_eq!(entry["needs_migration"], json!([]));
}

// ---------------------------------------------------------------------------
// Permission gating
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn codemod_preview_needs_only_scripts_read() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = "xrpc.query:com.example.list";
    create_script(&app, id, "function handle()\n  return params.q\nend\n").await;
    let key = scoped_api_key(&app, "scripts-reader", &["scripts:read"]).await;

    let resp = app
        .router
        .clone()
        .oneshot(bearer_post(
            &format!("/admin/scripts/{}/codemod", urlencoding::encode(id)),
            &key,
            &json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
#[serial]
async fn codemod_apply_requires_scripts_manage_even_with_scripts_read() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = "xrpc.query:com.example.list";
    create_script(&app, id, "function handle()\n  return params.q\nend\n").await;
    let key = scoped_api_key(&app, "scripts-reader", &["scripts:read"]).await;

    let resp = app
        .router
        .clone()
        .oneshot(bearer_post(
            &format!("/admin/scripts/{}/codemod", urlencoding::encode(id)),
            &key,
            &json!({ "apply": true }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "apply must require scripts:manage, not just scripts:read"
    );

    let row = app
        .router
        .clone()
        .oneshot(admin_get(
            &format!("/admin/scripts/{}", urlencoding::encode(id)),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    let row = json_body(row).await;
    assert_eq!(row["body"], "function handle()\n  return params.q\nend\n");
}

#[tokio::test]
#[serial]
async fn codemod_without_scripts_read_returns_403() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = "xrpc.query:com.example.list";
    create_script(&app, id, "function handle()\n  return params.q\nend\n").await;
    let key = scoped_api_key(&app, "unrelated", &["stats:read"]).await;

    let resp = app
        .router
        .clone()
        .oneshot(bearer_post(
            &format!("/admin/scripts/{}/codemod", urlencoding::encode(id)),
            &key,
            &json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}
