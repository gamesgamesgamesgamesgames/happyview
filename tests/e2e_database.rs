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
