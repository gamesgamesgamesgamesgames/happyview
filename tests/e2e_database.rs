mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::db::DatabaseBackend;
use http_body_util::BodyExt;
use serde_json::Value;
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;

async fn json_body(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
#[serial]
async fn status_reports_backend_and_vacuum_state() {
    common::require_db!();
    let app = TestApp::new().await;
    if app.state.db_backend == DatabaseBackend::Sqlite {
        // The first scheduled checkpoint is a minute out; drive one so the
        // status has an outcome to report.
        happyview::maintenance::sqlite::checkpoint_truncate(&app.state.db)
            .await
            .expect("checkpoint");
    }

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/database/status")
                .header(app.admin_cookie().0, app.admin_cookie().1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert!(body["backend"].is_string());
    assert!(body["vacuum"].is_object());
    assert!(
        body["journal_size_limit"].is_number(),
        "journal_size_limit missing or not a number: {body}"
    );
    assert!(
        body.as_object()
            .is_some_and(|o| o.contains_key("checkpoint")),
        "checkpoint key missing: {body}"
    );
    if app.state.db_backend == DatabaseBackend::Sqlite {
        let checkpoint = &body["checkpoint"];
        assert!(
            checkpoint.is_object(),
            "checkpoint should be an outcome: {body}"
        );
        assert!(
            checkpoint["at"].is_string(),
            "checkpoint.at missing: {body}"
        );
        assert!(
            checkpoint["busy"].is_boolean(),
            "checkpoint.busy missing: {body}"
        );
        assert!(
            checkpoint["wal_frames"].is_i64(),
            "wal_frames missing: {body}"
        );
        assert!(
            checkpoint["checkpointed_frames"].is_i64(),
            "checkpointed_frames missing: {body}"
        );
    } else {
        assert!(body["checkpoint"].is_null(), "null on Postgres: {body}");
    }
    assert!(
        body["checkpoint_interval_secs"].is_u64(),
        "checkpoint_interval_secs missing: {body}"
    );

    // `disk`/`feasibility` are only meaningful for a file-backed SQLite
    // database (`disk::report` returns `None` for any other backend, which
    // serializes to JSON `null`). Asserting the full shape here — rather than
    // just `is_object()` on the whole body — is what would catch a future
    // `unwrap_or(0)` on `db_fs_free`/`temp_fs_free`: that regression would
    // turn a real disk's free space into a suspicious literal `0` rather than
    // leaving it `null` or a genuine (non-zero, on any real test host) count.
    if app.state.db_backend == DatabaseBackend::Sqlite {
        assert!(body["disk"].is_object(), "disk missing: {body}");
        let disk = &body["disk"];
        assert!(
            disk["db_fs_free"].is_u64() || disk["db_fs_free"].is_null(),
            "db_fs_free must be a number or null, never a stand-in like 0 \
             collapsed from `unwrap_or(0)`: {body}"
        );
        assert!(
            disk["temp_fs_free"].is_u64() || disk["temp_fs_free"].is_null(),
            "temp_fs_free must be a number or null: {body}"
        );
        assert!(
            disk["same_filesystem"].is_boolean(),
            "same_filesystem missing or not a bool: {body}"
        );
        assert!(
            body["feasibility"]["status"].is_string(),
            "feasibility.status missing: {body}"
        );
    } else {
        assert!(
            body["disk"].is_null(),
            "disk should be null on a non-SQLite backend: {body}"
        );
        assert!(
            body["feasibility"].is_null(),
            "feasibility should be null on a non-SQLite backend: {body}"
        );
    }
}

#[tokio::test]
#[serial]
async fn scheduling_and_cancelling_a_vacuum_round_trips() {
    common::require_db!();
    let app = TestApp::new().await;

    let post = |uri: &str, method: &str| {
        Request::builder()
            .method(method)
            .uri(uri.to_string())
            .header(app.admin_cookie().0, app.admin_cookie().1)
            .body(Body::empty())
            .unwrap()
    };

    let resp = app
        .router
        .clone()
        .oneshot(post("/admin/database/vacuum/schedule", "POST"))
        .await
        .unwrap();

    // VACUUM is a SQLite-only operation, so scheduling it on Postgres is
    // refused outright rather than silently armed-but-inert: `status` would
    // otherwise report "scheduled" forever since nothing ever runs it.
    if app.state.db_backend != DatabaseBackend::Sqlite {
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let resp = app
            .router
            .clone()
            .oneshot(post("/admin/database/status", "GET"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = json_body(resp).await;
        assert!(
            body["vacuum"]["requested_at"].is_null(),
            "should not have armed on a non-SQLite backend: {body}"
        );
        return;
    }

    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .router
        .clone()
        .oneshot(post("/admin/database/status", "GET"))
        .await
        .unwrap();
    let body = json_body(resp).await;
    assert!(
        body["vacuum"]["requested_at"].is_string(),
        "not armed: {body}"
    );

    let resp = app
        .router
        .clone()
        .oneshot(post("/admin/database/vacuum/schedule", "DELETE"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .router
        .clone()
        .oneshot(post("/admin/database/status", "GET"))
        .await
        .unwrap();
    let body = json_body(resp).await;
    assert!(
        body["vacuum"]["requested_at"].is_null(),
        "not disarmed: {body}"
    );
}

#[tokio::test]
#[serial]
async fn status_requires_auth() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/database/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[serial]
async fn retrain_refuses_postgres_and_collections_without_records() {
    common::require_db!();
    let app = TestApp::new().await;
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/storage/retrain")
                .header(app.admin_cookie().0, app.admin_cookie().1)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"collection":"app.test.nothing"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let expected = match app.state.db_backend {
        DatabaseBackend::Postgres => StatusCode::BAD_REQUEST,
        DatabaseBackend::Sqlite => StatusCode::NOT_FOUND,
    };
    assert_eq!(resp.status(), expected);
}

/// Seed `n` raw rows of a fresh collection into the SQLite v2 table,
/// returning `(nsid, collection_id)`.
async fn seed_v2_collection(app: &TestApp, n: usize) -> (String, i64) {
    let nsid = format!("app.test.retrain{}", uuid::Uuid::new_v4().simple());
    let (collection_id,): (i64,) =
        happyview::db::query_as("INSERT INTO happyview_collections (nsid) VALUES (?) RETURNING id")
            .bind(&nsid)
            .fetch_one(&app.state.db)
            .await
            .expect("collection");
    let (did_id,): (i64,) =
        happyview::db::query_as("INSERT INTO happyview_dids (did) VALUES (?) RETURNING id")
            .bind(format!("did:plc:r{}", uuid::Uuid::new_v4().simple()))
            .fetch_one(&app.state.db)
            .await
            .expect("did");
    for i in 0..n {
        let body = format!(
            r#"{{"$type":"app.bsky.feed.post","text":"post {i} about retraining","createdAt":"2026-10-12T12:{:02}:{:02}.000Z","langs":["en"]}}"#,
            i % 60,
            (i * 7) % 60
        );
        happyview::db::query(
            "INSERT INTO happyview_records_v2 (did_id, collection_id, rkey, cid, body, created_at) \
             VALUES (?, ?, ?, x'', CAST(? AS BLOB), 0)",
        )
        .bind(did_id)
        .bind(collection_id)
        .bind(format!("r{i}"))
        .bind(body)
        .execute(&app.state.db)
        .await
        .expect("seed row");
    }
    (nsid, collection_id)
}

async fn post_retrain(app: &TestApp, nsid: &str) -> axum::response::Response {
    app.router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/storage/retrain")
                .header(app.admin_cookie().0, app.admin_cookie().1)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "collection": nsid }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
#[serial]
async fn retrain_trains_and_installs_a_dictionary() {
    common::require_db!();
    let app = TestApp::new().await;
    if app.state.db_backend != DatabaseBackend::Sqlite {
        return;
    }
    let (nsid, collection_id) = seed_v2_collection(&app, 1000).await;
    let resp = post_retrain(&app, &nsid).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["collection"], nsid);
    let dict_id = body["dict_id"].as_i64().expect("dict_id");
    assert_eq!(
        app.state.records.bodies().current_dict(collection_id),
        Some(dict_id),
        "new writes use the retrained dictionary"
    );
}

#[tokio::test]
#[serial]
async fn retrain_of_too_few_records_is_unprocessable() {
    common::require_db!();
    let app = TestApp::new().await;
    if app.state.db_backend != DatabaseBackend::Sqlite {
        return;
    }
    let (nsid, collection_id) = seed_v2_collection(&app, 2).await;
    let resp = post_retrain(&app, &nsid).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = json_body(resp).await;
    assert!(
        body["message"].as_str().is_some_and(|m| !m.is_empty()),
        "{body}"
    );
    assert_eq!(app.state.records.bodies().current_dict(collection_id), None);
}
