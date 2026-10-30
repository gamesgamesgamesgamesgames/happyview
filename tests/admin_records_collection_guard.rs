//! `DELETE /admin/records/collection` and a collection a shared record store
//! serves.
//!
//! This is a change in behaviour: the route used to answer `202`, run, and
//! report success having emptied only this instance's overlay rows while the
//! shared records they union with stayed exactly where they were.

mod common;

use common::app::TestApp;
use common::fake_storage::FakeStorage;
use common::fixtures;
use serde_json::json;

const GAME: &str = "games.gamesgamesgamesgames.game";

async fn delete_jobs(app: &TestApp) -> Vec<serde_json::Value> {
    let listed = app.get_json("/admin/jobs").await;
    listed
        .get("jobs")
        .and_then(|j| j.as_array())
        .map(|jobs| {
            jobs.iter()
                .filter(|j| j["job_type"] == "happyview.delete-collection")
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn a_collection_a_shared_store_serves_is_refused() {
    let app = TestApp::new_with_external_storage(std::sync::Arc::new(
        FakeStorage::claiming(&[GAME]).await,
    ))
    .await;
    let (status, body) = app
        .post_json_status(
            "/admin/lexicons",
            json!({ "lexicon_json": fixtures::game_record_lexicon(), "backfill": false }),
        )
        .await;
    assert!(status == 201 || status == 200, "{status}: {body}");

    // The probe says so before anyone asks.
    let listed = app.get_json("/admin/records/collections").await;
    assert!(
        listed["shared"]
            .as_array()
            .is_some_and(|s| s.iter().any(|c| c == GAME)),
        "the collection list should mark it shared: {listed}"
    );

    let (status, body) = app
        .delete_json(&format!("/admin/records/collection?collection={GAME}"))
        .await;
    assert_eq!(status, 400, "{body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|e| e.contains("shared record store")),
        "the same reason the lexicon route gives: {body}"
    );
    assert!(
        delete_jobs(&app).await.is_empty(),
        "nothing should have been enqueued"
    );
}

#[tokio::test]
async fn a_local_collection_still_deletes() {
    let app = TestApp::new().await;
    let (status, body) = app
        .post_json_status(
            "/admin/lexicons",
            json!({ "lexicon_json": fixtures::game_record_lexicon(), "backfill": false }),
        )
        .await;
    assert!(status == 201 || status == 200, "{status}: {body}");

    let listed = app.get_json("/admin/records/collections").await;
    assert_eq!(
        listed["shared"].as_array().map(Vec::len),
        Some(0),
        "nothing is shared here: {listed}"
    );

    let (status, body) = app
        .delete_json(&format!("/admin/records/collection?collection={GAME}"))
        .await;
    assert_eq!(status, 202, "{body}");
    assert!(body["job_id"].as_str().is_some(), "{body}");
    assert_eq!(delete_jobs(&app).await.len(), 1);
}
