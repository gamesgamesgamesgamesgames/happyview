//! Deleting a lexicon, and the opt-in that also deletes its indexed records.
//!
//! One `TestApp` for every case: two of them in one file contend for the
//! shared test database and take minutes rather than seconds.

mod common;

use common::app::TestApp;
use common::fixtures;
use serde_json::json;

const GAME: &str = "games.gamesgamesgamesgames.game";
const LIST: &str = "games.gamesgamesgamesgames.listGames";

/// Every delete-collection job this instance has enqueued, newest first.
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
async fn deleting_a_lexicon_can_also_delete_its_records() {
    let app = TestApp::new().await;

    // A record lexicon, and the count the confirm dialog asks for.
    let (status, body) = app
        .post_json_status(
            "/admin/lexicons",
            json!({ "lexicon_json": fixtures::game_record_lexicon(), "backfill": false }),
        )
        .await;
    assert_eq!(status, 201, "{body}");

    let counted = app
        .get_json(&format!("/admin/lexicons/{GAME}/record-count"))
        .await;
    assert_eq!(counted["collection"], GAME, "{counted}");
    assert_eq!(counted["count"], 0, "an empty collection counts zero");

    // A query lexicon stores nothing of its own: it reads the record
    // lexicon's collection through target_collection. Offering to delete
    // "its" records would delete another lexicon's, so it is refused...
    let (status, body) = app
        .post_json_status(
            "/admin/lexicons",
            json!({
                "lexicon_json": fixtures::list_games_query_lexicon(),
                "backfill": false,
                "target_collection": GAME,
            }),
        )
        .await;
    assert_eq!(status, 201, "{body}");

    let (status, body) = app
        .delete_json(&format!("/admin/lexicons/{LIST}?delete_records=true"))
        .await;
    assert_eq!(status, 400, "{body}");
    let message = body["error"].as_str().unwrap_or_default();
    assert!(message.contains("query"), "{body}");
    assert!(
        message.contains(GAME),
        "the refusal should name whose records those are: {body}"
    );

    // ...and the refusal must leave the lexicon alone. Removing it and then
    // refusing would orphan exactly what this route exists to tidy up.
    let still_there = app.get_json(&format!("/admin/lexicons/{LIST}")).await;
    assert_eq!(still_there["id"], LIST, "{still_there}");

    let (status, body) = app
        .delete_json(&format!("/admin/lexicons/{LIST}/record-count"))
        .await;
    assert_eq!(status, 405, "record-count takes GET, not DELETE: {body}");
    let counted = app
        .get_json(&format!("/admin/lexicons/{LIST}/record-count"))
        .await;
    assert!(
        counted["error"]
            .as_str()
            .is_some_and(|e| e.contains("query")),
        "a query lexicon has no count of its own: {counted}"
    );

    // Without the opt-in nothing is enqueued: a lexicon delete has never
    // touched the records and must not start.
    let (status, body) = app.delete_json(&format!("/admin/lexicons/{GAME}")).await;
    assert_eq!(status, 204, "{body}");
    assert!(
        delete_jobs(&app).await.is_empty(),
        "a plain lexicon delete must enqueue no record deletion"
    );

    // With the opt-in: 202 and the job id, so the dashboard can follow it.
    // 201 on a fresh revision, 200 if the revision survived the delete;
    // which of the two is not what this test is about.
    let (status, body) = app
        .post_json_status(
            "/admin/lexicons",
            json!({ "lexicon_json": fixtures::game_record_lexicon(), "backfill": false }),
        )
        .await;
    assert!(status == 201 || status == 200, "{status}: {body}");

    let (status, body) = app
        .delete_json(&format!("/admin/lexicons/{GAME}?delete_records=true"))
        .await;
    assert_eq!(status, 202, "{body}");
    assert_eq!(body["collection"], GAME, "{body}");
    let job_id = body["job_id"].as_str().expect("a job id").to_string();

    let jobs = delete_jobs(&app).await;
    assert_eq!(jobs.len(), 1, "exactly one delete was enqueued: {jobs:?}");
    assert_eq!(jobs[0]["id"].as_str(), Some(job_id.as_str()));
    assert_eq!(
        jobs[0]["input"]["collection"], GAME,
        "the job deletes the collection the lexicon named: {:?}",
        jobs[0]
    );

    // The lexicon itself is gone either way.
    let gone = app.get_json(&format!("/admin/lexicons/{GAME}")).await;
    assert!(gone["error"].as_str().is_some(), "{gone}");
}

/// A collection a shared record store serves is not this instance's to
/// empty, and the refusal has to come before the lexicon is removed.
///
/// The delete would reach only this tenant's overlay rows: the shared
/// records those union with are read with `SELECT` and nothing more, so they
/// would stay exactly where they are and the job would report success having
/// removed a fraction of what the operator was looking at.
#[tokio::test]
async fn a_shared_collection_refuses_the_records_option() {
    let storage = std::sync::Arc::new(common::fake_storage::FakeStorage::claiming(&[GAME]).await);
    let app = TestApp::new_with_external_storage(storage).await;

    let (status, body) = app
        .post_json_status(
            "/admin/lexicons",
            json!({ "lexicon_json": fixtures::game_record_lexicon(), "backfill": false }),
        )
        .await;
    assert!(status == 201 || status == 200, "{status}: {body}");

    // The count still answers — reading a shared collection is fine — but it
    // says the records are not this instance's to delete, which is what the
    // confirm reads to decide whether to offer the option at all.
    let counted = app
        .get_json(&format!("/admin/lexicons/{GAME}/record-count"))
        .await;
    assert_eq!(counted["deletable"], false, "{counted}");
    assert!(
        counted["not_deletable_reason"]
            .as_str()
            .is_some_and(|r| r.contains("shared record store")),
        "{counted}"
    );

    let (status, body) = app
        .delete_json(&format!("/admin/lexicons/{GAME}?delete_records=true"))
        .await;
    assert_eq!(status, 400, "{body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|e| e.contains("shared record store")),
        "{body}"
    );

    // And it is still there: refusing after removing it would orphan exactly
    // what the refusal is protecting.
    let still_there = app.get_json(&format!("/admin/lexicons/{GAME}")).await;
    assert_eq!(still_there["id"], GAME, "{still_there}");
    assert!(
        delete_jobs(&app).await.is_empty(),
        "nothing should have been enqueued"
    );

    // Without the option the lexicon deletes as it always has.
    let (status, body) = app.delete_json(&format!("/admin/lexicons/{GAME}")).await;
    assert_eq!(status, 204, "{body}");
}

/// The network-lexicon route offers the same option on the same terms, so
/// neither door is the lenient one.
#[tokio::test]
async fn removing_a_network_lexicon_can_also_delete_its_records() {
    let app = TestApp::new().await;

    // A network-sourced row, then a catalogue reload, because the catalogue
    // holds its lexicons in memory and this went in behind it.
    let now = happyview::db::now_rfc3339();
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_lexicons \
           (id, lexicon_json, backfill, source, authority_did, last_fetched_at, created_at) \
         VALUES (?, ?, 0, 'network', ?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(GAME)
        .bind(serde_json::to_string(&fixtures::game_record_lexicon()).unwrap())
        .bind("did:plc:someauthority")
        .bind(&now)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .expect("seed the network lexicon");
    app.state
        .lexicons
        .load_from_db(&app.state.db)
        .await
        .expect("reload the catalogue");

    let (status, body) = app
        .delete_json(&format!(
            "/admin/network-lexicons/{GAME}?delete_records=true"
        ))
        .await;
    assert_eq!(status, 202, "{body}");
    assert_eq!(body["collection"], GAME, "{body}");
    let job_id = body["job_id"].as_str().expect("a job id").to_string();

    let jobs = delete_jobs(&app).await;
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    assert_eq!(jobs[0]["id"].as_str(), Some(job_id.as_str()));
    assert_eq!(jobs[0]["input"]["collection"], GAME);
}
