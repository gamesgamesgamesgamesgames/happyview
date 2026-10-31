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
use happyview::db::adapt_sql;
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

/// A `happyview.delete-collection` job already in the queue is refused by the
/// job as well, not only by the routes that enqueue it.
///
/// The three enqueue sites now refuse a shared collection, so a job reaching
/// the worker for one was queued when the answer was different: before the
/// upgrade that added the refusal, or before the store claimed the collection.
/// Without a guard inside the job, the worker picks it up after the deploy and
/// does the exact thing the refusal exists to stop — empties this instance's
/// overlay and reports the count as a success.
#[tokio::test]
async fn a_queued_job_for_a_shared_collection_is_failed_rather_than_run() {
    let storage = std::sync::Arc::new(FakeStorage::claiming(&[GAME]).await);
    let app = TestApp::new_with_external_storage(storage.clone()).await;

    // Seeded directly, because the routes would refuse to enqueue it: this is
    // the row an upgraded instance finds waiting for it.
    let id = "11111111-1111-4111-8111-111111111111";
    let sql = adapt_sql(
        "INSERT INTO happyview_jobs (id, job_type, status, input, progress, created_by, created_at) VALUES (?, ?, 'pending', ?, '{}', ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(id)
        .bind("happyview.delete-collection")
        .bind(json!({ "collection": GAME }).to_string())
        .bind(&app.admin_did)
        .bind(happyview::db::now_rfc3339())
        .execute(&app.state.db)
        .await
        .expect("could not seed the queued job");

    let worker = tokio::spawn(happyview::jobs::worker::run_worker(app.state.clone()));
    let (status, error) = await_terminal_job(&app, id).await;
    worker.abort();

    assert_eq!(status, "failed", "the job should not have run: {error:?}");
    assert!(
        error
            .as_deref()
            .is_some_and(|e| e.contains("shared record store")),
        "the same reason the routes give: {error:?}"
    );
    // The behaviour, not just the status: the store was never asked to delete.
    assert!(
        storage.collection_deletes.lock().unwrap().is_empty(),
        "the job asked the store for a batch delete: {:?}",
        storage.collection_deletes.lock().unwrap()
    );
}

/// A job's `status` and `error` once it is neither pending nor running.
async fn await_terminal_job(app: &TestApp, id: &str) -> (String, Option<String>) {
    let sql = adapt_sql(
        "SELECT status, error FROM happyview_jobs WHERE id = ?",
        app.state.db_backend,
    );
    for _ in 0..600 {
        let row = happyview::db::query_as::<(String, Option<String>)>(&sql)
            .bind(id)
            .fetch_one(&app.state.db)
            .await
            .expect("the seeded job should still be there");
        if !matches!(row.0.as_str(), "pending" | "running") {
            return row;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the job never reached a terminal status");
}
