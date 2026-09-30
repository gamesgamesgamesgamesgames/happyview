//! End-to-end tests for the trigger-keyed scripts subsystem.
//!
//! Covers:
//! - Admin CRUD on `/admin/scripts` with trigger-id validation.
//! - Dispatcher cascade for record events
//!   (`record.<action>:<nsid>` → `record.index:<nsid>`).
//! - Label scripts: URI-routed dispatch + local index mutation through
//!   `happyview.record` (`delete_local`, `save_local`).
//! - The no-session boundary: a label script that calls `record.put`
//!   gets dead-lettered fail-open with the original record untouched.
//!
//! The five tests marked `#[ignore]` are the ones whose scripts `require` a
//! library plugin. Those libraries ship from the plugins repository rather
//! than from this build, and this harness installs none, so the `require`
//! fails before the body under test runs. They are gated individually rather
//! than as a file so that the rest of the suite runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::db::{adapt_sql, now_rfc3339};
use happyview::lua::{LabelAppliedEvent, LabelHookOutcome, run_label_applied_script};
use happyview::record_handler::{RecordEvent, handle_record_event};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;
use common::fixtures;

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

fn admin_patch(
    uri: &str,
    cookie: (axum::http::HeaderName, axum::http::HeaderValue),
    body: &Value,
) -> Request<Body> {
    Request::builder()
        .method("PATCH")
        .uri(uri)
        .header(cookie.0, cookie.1)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

fn admin_delete(
    uri: &str,
    cookie: (axum::http::HeaderName, axum::http::HeaderValue),
) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .header(cookie.0, cookie.1)
        .body(Body::empty())
        .unwrap()
}

/// Seed a record-type lexicon (no scripts bound — scripts live in their
/// own table now, addressed by trigger id).
async fn seed_lexicon(app: &TestApp, lexicon: Value) {
    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/lexicons",
            app.admin_cookie(),
            &json!({ "lexicon_json": lexicon }),
        ))
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "seeding lexicon failed: {:?}",
        resp.status()
    );
}

/// Create a script via the admin API. Returns the created row.
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

async fn seed_record_row(
    app: &TestApp,
    uri: &str,
    did: &str,
    collection: &str,
    rkey: &str,
    body: Value,
) {
    let sql = adapt_sql(
        "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(uri)
        .bind(did)
        .bind(collection)
        .bind(rkey)
        .bind(serde_json::to_string(&body).unwrap_or_default())
        .bind("bafyseed")
        .bind(now_rfc3339())
        .execute(&app.state.db)
        .await
        .expect("failed to seed records row");
}

async fn count_records(app: &TestApp, uri: &str) -> i64 {
    let (count,): (i64,) = happyview::db::query_as(&adapt_sql(
        "SELECT COUNT(*) FROM happyview_records WHERE uri = ?",
        app.state.db_backend,
    ))
    .bind(uri)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    count
}

/// A game-record event carrying the record's real CID, so it passes
/// CID verification and reaches indexing.
fn game_event(rkey: &str, action: &str, record: Value) -> RecordEvent {
    let cid = happyview::cid_verify::compute_record_cid(&record).map(|c| c.to_string());
    RecordEvent {
        did: "did:plc:test".into(),
        collection: "games.gamesgamesgamesgames.game".into(),
        rkey: rkey.into(),
        action: action.into(),
        record: Some(record),
        cid,
    }
}

async fn fetch_record_body(app: &TestApp, uri: &str) -> Option<Value> {
    let row: Option<(String,)> = happyview::db::query_as(&adapt_sql(
        "SELECT record FROM happyview_records WHERE uri = ?",
        app.state.db_backend,
    ))
    .bind(uri)
    .fetch_optional(&app.state.db)
    .await
    .unwrap();
    row.map(|(s,)| serde_json::from_str(&s).unwrap())
}

// ---------------------------------------------------------------------------
// Admin CRUD
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn create_then_get_script_round_trips() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = "record.create:com.example.thing";
    create_script(&app, id, "function handle(input) return input.record end").await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_get(
            &format!("/admin/scripts/{}", urlencoding::encode(id)),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let row = json_body(resp).await;
    assert_eq!(row["id"], id);
    assert_eq!(row["script_type"], "lua");
}

#[tokio::test]
#[serial]
async fn list_scripts_returns_all_rows() {
    common::require_db!();
    let app = TestApp::new().await;
    create_script(
        &app,
        "record.create:com.example.thing",
        "function handle(input) return input.record end",
    )
    .await;
    create_script(
        &app,
        "labeler.apply:_actor",
        "function handle(input) return input end",
    )
    .await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/scripts", app.admin_cookie()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let rows = json_body(resp).await;
    let arr = rows.as_array().unwrap();
    assert_eq!(arr.len(), 2);
}

#[tokio::test]
#[serial]
async fn create_rejects_invalid_trigger_prefix() {
    common::require_db!();
    let app = TestApp::new().await;
    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/scripts",
            app.admin_cookie(),
            &json!({
                "id": "garbage:com.example.thing",
                "body": "function handle() end",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let err = json_body(resp).await;
    let msg = err["error"].as_str().unwrap_or("");
    assert!(
        msg.contains("unknown trigger prefix"),
        "expected validation error, got: {msg}"
    );
}

#[tokio::test]
#[serial]
async fn create_rejects_invalid_nsid_suffix() {
    common::require_db!();
    let app = TestApp::new().await;
    // Single-segment NSID — too few segments.
    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/scripts",
            app.admin_cookie(),
            &json!({
                "id": "record.create:foo",
                "body": "function handle() end",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[serial]
async fn create_allows_labeler_apply_actor_special_case() {
    common::require_db!();
    let app = TestApp::new().await;
    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/scripts",
            app.admin_cookie(),
            &json!({
                "id": "labeler.apply:_actor",
                "body": "function handle(input) return input end",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
}

#[tokio::test]
#[serial]
async fn create_rejects_invalid_lua_body() {
    common::require_db!();
    let app = TestApp::new().await;
    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/scripts",
            app.admin_cookie(),
            &json!({
                "id": "record.create:com.example.thing",
                "body": "function handle(", // syntax error
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[serial]
async fn patch_updates_body() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = "record.create:com.example.thing";
    create_script(&app, id, "function handle(input) return input.record end").await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_patch(
            &format!("/admin/scripts/{}", urlencoding::encode(id)),
            app.admin_cookie(),
            &json!({ "body": "function handle() return nil end" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let row = json_body(resp).await;
    assert!(row["body"].as_str().unwrap().contains("return nil"));
}

#[tokio::test]
#[serial]
async fn delete_removes_script() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = "record.delete:com.example.thing";
    create_script(&app, id, "function handle(input) return input.record end").await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_delete(
            &format!("/admin/scripts/{}", urlencoding::encode(id)),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .router
        .clone()
        .oneshot(admin_get(
            &format!("/admin/scripts/{}", urlencoding::encode(id)),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Cascade resolution: action-specific row wins over wildcard
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn cascade_wildcard_runs_when_no_action_specific() {
    common::require_db!();
    let app = TestApp::new().await;
    seed_lexicon(&app, fixtures::game_record_lexicon()).await;

    create_script(
        &app,
        "record.index:games.gamesgamesgamesgames.game",
        // Wildcard — uppercases the title for any action.
        "function handle(input) input.record.title = string.upper(input.record.title); return input.record end",
    )
    .await;

    let _ = handle_record_event(
        &app.state,
        &game_event("rkey1", "create", json!({"title": "test game"})),
    )
    .await;

    let body = fetch_record_body(
        &app,
        "at://did:plc:test/games.gamesgamesgamesgames.game/rkey1",
    )
    .await
    .unwrap();
    assert_eq!(body["title"], "TEST GAME");
}

#[tokio::test]
#[serial]
async fn cascade_action_specific_wins_over_wildcard() {
    common::require_db!();
    let app = TestApp::new().await;
    seed_lexicon(&app, fixtures::game_record_lexicon()).await;

    create_script(
        &app,
        "record.index:games.gamesgamesgamesgames.game",
        "function handle(input) input.record.title = 'WILDCARD'; return input.record end",
    )
    .await;
    create_script(
        &app,
        "record.create:games.gamesgamesgamesgames.game",
        "function handle(input) input.record.title = 'CREATE-SPECIFIC'; return input.record end",
    )
    .await;

    // Create action — specific should win.
    let _ = handle_record_event(
        &app.state,
        &game_event("rk-create", "create", json!({"title": "x"})),
    )
    .await;
    let body = fetch_record_body(
        &app,
        "at://did:plc:test/games.gamesgamesgamesgames.game/rk-create",
    )
    .await
    .unwrap();
    assert_eq!(body["title"], "CREATE-SPECIFIC");

    // Update action — no record.update binding → cascades to wildcard.
    let _ = handle_record_event(
        &app.state,
        &game_event("rk-update", "update", json!({"title": "x"})),
    )
    .await;
    let body = fetch_record_body(
        &app,
        "at://did:plc:test/games.gamesgamesgamesgames.game/rk-update",
    )
    .await
    .unwrap();
    assert_eq!(body["title"], "WILDCARD");
}

#[tokio::test]
#[serial]
async fn no_script_passes_record_through_unchanged() {
    common::require_db!();
    let app = TestApp::new().await;
    seed_lexicon(&app, fixtures::game_record_lexicon()).await;

    let _ = handle_record_event(
        &app.state,
        &game_event("rk1", "create", json!({"title": "untouched"})),
    )
    .await;
    let body = fetch_record_body(
        &app,
        "at://did:plc:test/games.gamesgamesgamesgames.game/rk1",
    )
    .await
    .unwrap();
    assert_eq!(body["title"], "untouched");
}

#[tokio::test]
#[serial]
async fn record_create_returning_nil_skips_indexing() {
    common::require_db!();
    let app = TestApp::new().await;
    seed_lexicon(&app, fixtures::game_record_lexicon()).await;

    create_script(
        &app,
        "record.create:games.gamesgamesgamesgames.game",
        "function handle() return nil end",
    )
    .await;

    let _ = handle_record_event(
        &app.state,
        &game_event("rk1", "create", json!({"title": "doomed"})),
    )
    .await;
    assert_eq!(
        count_records(
            &app,
            "at://did:plc:test/games.gamesgamesgamesgames.game/rk1"
        )
        .await,
        0,
        "nil return should drop the record"
    );
}

// ---------------------------------------------------------------------------
// internal.logging in scripts → event_logs
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn record_event_script_log_writes_event_log_row() {
    common::require_db!();
    let app = TestApp::new().await;
    seed_lexicon(&app, fixtures::game_record_lexicon()).await;

    create_script(
        &app,
        "record.create:games.gamesgamesgamesgames.game",
        "local log = require('internal.logging')\nfunction handle(input) log.info('hello from script'); return input.record end",
    )
    .await;

    let _ = handle_record_event(
        &app.state,
        &game_event("rk1", "create", json!({"title": "anything"})),
    )
    .await;

    // The script's log.info("hello from script") should land in event_logs
    // as a `script.log` row whose subject is the trigger id.
    let row: (String, String) = happyview::db::query_as(&adapt_sql(
        "SELECT subject, detail FROM happyview_event_logs
         WHERE event_type = 'script.log'
         ORDER BY id DESC LIMIT 1",
        app.state.db_backend,
    ))
    .fetch_one(&app.state.db)
    .await
    .expect("expected a script.log row");
    assert_eq!(row.0, "record.create:games.gamesgamesgamesgames.game");
    let detail: Value = serde_json::from_str(&row.1).unwrap();
    assert_eq!(detail["message"], "hello from script");
    assert_eq!(
        detail["trigger"],
        "record.create:games.gamesgamesgamesgames.game"
    );
}

// ---------------------------------------------------------------------------
// Label scripts via URI routing
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
#[ignore = "needs a library plugin this harness does not install"]
async fn label_script_can_drop_record_via_record_delete_local() {
    common::require_db!();
    let app = TestApp::new().await;

    let uri = "at://did:plc:victim/app.bsky.feed.post/rkey1";
    seed_record_row(
        &app,
        uri,
        "did:plc:victim",
        "app.bsky.feed.post",
        "rkey1",
        json!({"text": "hello"}),
    )
    .await;

    create_script(
        &app,
        "labeler.apply:app.bsky.feed.post",
        "local record = require('happyview.record') \
         function handle(input) \
            if input.val == 'spam' then record.delete_local(input.uri) end \
            return input \
         end",
    )
    .await;

    let outcome = run_label_applied_script(
        &app.state,
        LabelAppliedEvent {
            src: "did:plc:labeler".into(),
            uri: uri.into(),
            val: "spam".into(),
            neg: false,
            cts: now_rfc3339(),
            exp: None,
        },
    )
    .await;
    assert!(matches!(outcome, LabelHookOutcome::Continue(_)));
    assert_eq!(count_records(&app, uri).await, 0);
}

#[tokio::test]
#[serial]
#[ignore = "needs a library plugin this harness does not install"]
async fn label_script_can_redact_record_via_save_local() {
    common::require_db!();
    let app = TestApp::new().await;

    let uri = "at://did:plc:author/app.bsky.feed.post/rkey1";
    seed_record_row(
        &app,
        uri,
        "did:plc:author",
        "app.bsky.feed.post",
        "rkey1",
        json!({"text": "original content"}),
    )
    .await;

    create_script(
        &app,
        "labeler.apply:app.bsky.feed.post",
        "local record = require('happyview.record') \
         function handle(input) \
            if input.val == 'redact' then \
                local entry = record.load(input.uri) \
                if entry then \
                    entry.record.text = '[redacted by ' .. input.src .. ']' \
                    record.save_local('app.bsky.feed.post', 'rkey1', entry.record, 'did:plc:author') \
                end \
            end; \
            return input \
         end",
    )
    .await;

    let outcome = run_label_applied_script(
        &app.state,
        LabelAppliedEvent {
            src: "did:plc:labeler".into(),
            uri: uri.into(),
            val: "redact".into(),
            neg: false,
            cts: now_rfc3339(),
            exp: None,
        },
    )
    .await;
    assert!(matches!(outcome, LabelHookOutcome::Continue(_)));

    let body = fetch_record_body(&app, uri).await.unwrap();
    assert_eq!(body["text"], "[redacted by did:plc:labeler]");
}

#[tokio::test]
#[serial]
#[ignore = "needs a library plugin this harness does not install"]
async fn label_script_uri_routes_actor_special_case() {
    common::require_db!();
    let app = TestApp::new().await;
    happyview::db::query("DROP TABLE IF EXISTS script_sentinel")
        .execute(&app.state.db)
        .await
        .unwrap();

    create_script(
        &app,
        "labeler.apply:_actor",
        // Sentinel: write a row into a caller-owned table (sql.raw cannot touch
        // internal HappyView tables) so we can detect that the script ran.
        // The value is a literal rather than a bound parameter because
        // `sql.raw` passes placeholders through untranslated, so a `?` here
        // would only bind on SQLite.
        "local sql = require('happyview.sql') \
         function handle(input) \
            sql.raw('CREATE TABLE IF NOT EXISTS script_sentinel (k TEXT)') \
            sql.raw('INSERT INTO script_sentinel (k) VALUES (\\'fired\\')') \
            return input \
         end",
    )
    .await;

    // Bare DID URI should route to `labeler.apply:_actor`.
    let outcome = run_label_applied_script(
        &app.state,
        LabelAppliedEvent {
            src: "did:plc:labeler".into(),
            uri: "did:plc:somebody".into(),
            val: "imposter".into(),
            neg: false,
            cts: now_rfc3339(),
            exp: None,
        },
    )
    .await;
    assert!(matches!(outcome, LabelHookOutcome::Continue(_)));

    // Sentinel row should exist if the script ran.
    let (count,): (i64,) =
        happyview::db::query_as("SELECT COUNT(*) FROM script_sentinel WHERE k = 'fired'")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(
        count, 1,
        "labeler.apply:_actor should have fired for bare-DID label"
    );
}

#[tokio::test]
#[serial]
#[ignore = "needs a library plugin this harness does not install"]
async fn label_script_calling_record_put_dead_letters_with_clear_message() {
    common::require_db!();
    let app = TestApp::new().await;

    let uri = "at://did:plc:author/app.bsky.feed.post/rkey1";
    seed_record_row(
        &app,
        uri,
        "did:plc:author",
        "app.bsky.feed.post",
        "rkey1",
        json!({"text": "untouched"}),
    )
    .await;

    create_script(
        &app,
        "labeler.apply:app.bsky.feed.post",
        "local record = require('happyview.record') \
         function handle(input) \
            local entry = record.load(input.uri) \
            if entry then record.put(input.uri, { text = 'should fail' }) end \
            return input \
         end",
    )
    .await;

    let outcome = run_label_applied_script(
        &app.state,
        LabelAppliedEvent {
            src: "did:plc:labeler".into(),
            uri: uri.into(),
            val: "anything".into(),
            neg: false,
            cts: now_rfc3339(),
            exp: None,
        },
    )
    .await;
    // Fail-open: the original label still continues even after the script
    // fails its retry budget.
    assert!(matches!(outcome, LabelHookOutcome::Continue(_)));

    // The original record is unchanged.
    let body = fetch_record_body(&app, uri).await.unwrap();
    assert_eq!(body["text"], "untouched");

    // A dead-letter row exists with the no-session message.
    let dl: (String,) = happyview::db::query_as(&adapt_sql(
        "SELECT error FROM happyview_dead_letter_scripts WHERE host_kind = 'label' \
         AND host_id = 'did:plc:labeler' ORDER BY id DESC LIMIT 1",
        app.state.db_backend,
    ))
    .fetch_one(&app.state.db)
    .await
    .expect("expected a dead_letter_scripts row");
    assert!(
        dl.0.contains("NO_SESSION"),
        "expected the no-session error in the dead-letter, got: {}",
        dl.0
    );

    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/dead-letters", app.admin_cookie()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let list = json_body(resp).await;
    let id = list["dead_letters"][0]["id"]
        .as_str()
        .expect("dead letter listed")
        .to_string();

    let resp = app
        .router
        .clone()
        .oneshot(admin_get(
            &format!("/admin/dead-letters/{id}"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/dead-letters/{id}/retry"),
            app.admin_cookie(),
            &json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[serial]
#[ignore = "needs a library plugin this harness does not install"]
async fn record_event_script_can_call_record_delete_local() {
    common::require_db!();
    let app = TestApp::new().await;
    seed_lexicon(&app, fixtures::game_record_lexicon()).await;

    let victim_uri = "at://did:plc:test/games.gamesgamesgamesgames.game/old";
    seed_record_row(
        &app,
        victim_uri,
        "did:plc:test",
        "games.gamesgamesgamesgames.game",
        "old",
        json!({"title": "should-be-gone"}),
    )
    .await;

    create_script(
        &app,
        "record.create:games.gamesgamesgamesgames.game",
        "local record = require('happyview.record') \
         function handle(input) \
            record.delete_local('at://did:plc:test/games.gamesgamesgamesgames.game/old') \
            return input.record \
         end",
    )
    .await;

    let _ = handle_record_event(
        &app.state,
        &game_event("new1", "create", json!({"title": "fresh game"})),
    )
    .await;

    assert_eq!(count_records(&app, victim_uri).await, 0);
    assert_eq!(
        count_records(
            &app,
            "at://did:plc:test/games.gamesgamesgamesgames.game/new1"
        )
        .await,
        1
    );
}

// ---------------------------------------------------------------------------
// Permission gating
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn no_auth_returns_401() {
    common::require_db!();
    let app = TestApp::new().await;
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/scripts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
