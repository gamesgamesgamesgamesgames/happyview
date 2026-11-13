//! `GET /admin/update-status` serves what the update check knows, to any
//! signed-in dashboard user and to no one else.

mod common;

use axum::body::Body;
use axum::http::Request;
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;
use happyview::update_check::{Offer, Severity, State, UpdateStatus};

fn available() -> UpdateStatus {
    UpdateStatus {
        state: State::Available,
        current: "2.17.3".into(),
        channel: Some("v2".into()),
        available: Some(Offer {
            version: "2.17.4".into(),
            channel: "v2".into(),
            release_url: Some(
                "https://github.com/gamesgamesgamesgamesgames/happyview/releases/tag/v2.17.4"
                    .into(),
            ),
            upgrade_guide_url: happyview::update_check::UPGRADE_GUIDE_URL.into(),
            severity: Severity::Routine,
        }),
        other_line: None,
        checked_at: Some("2026-11-12T00:00:00Z".into()),
        last_error: None,
        managed: false,
    }
}

#[tokio::test]
#[serial]
async fn update_status_needs_a_signed_in_user() {
    common::require_db!();
    let app = TestApp::new().await;
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/update-status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
#[serial]
async fn update_status_serves_what_the_check_found() {
    common::require_db!();
    let app = TestApp::new().await;
    *app.state.update_status.write().await = available();

    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .uri("/admin/update-status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["state"], "available");
    assert_eq!(body["current"], "2.17.3");
    assert_eq!(body["channel"], "v2");
    assert_eq!(body["available"]["version"], "2.17.4");
    assert_eq!(body["available"]["severity"], "routine");
    assert_eq!(
        body["available"]["upgradeGuideUrl"],
        happyview::update_check::UPGRADE_GUIDE_URL
    );
    assert!(body["otherLine"].is_null());
}

/// The last good answer survives a restart, and only for the version that
/// stored it. Stored twice to check the upsert replaces the row.
#[tokio::test]
#[serial]
async fn the_last_answer_is_stored_for_its_own_version() {
    common::require_db!();
    let app = TestApp::new().await;
    let (db, backend) = (&app.state.db, app.state.db_backend);

    let mut first = available();
    first.checked_at = Some("2026-11-11T00:00:00Z".into());
    happyview::update_check::store(db, backend, &first).await;
    happyview::update_check::store(db, backend, &available()).await;

    let loaded = happyview::update_check::load_stored(db, backend, "2.17.3")
        .await
        .expect("stored");
    assert_eq!(loaded, available());
    assert!(
        happyview::update_check::load_stored(db, backend, "2.17.4")
            .await
            .is_none(),
        "an answer stored by another version is not reused"
    );
}
