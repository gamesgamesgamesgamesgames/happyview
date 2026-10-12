//! The project layer through the real admin routes: what a file owns answers
//! 409 naming the file, listings say which layer answers, and a dashboard row
//! a file shadows survives untouched.

mod common;

use std::sync::Arc;

use happyview::db::adapt_sql;
use happyview::project::ProjectSnapshot;
use serde_json::json;
use serial_test::serial;

use common::app::TestApp;
use common::fixtures;
use common::project;

const GAME: &str = "games.gamesgamesgamesgames.game";
const UNRESOLVABLE: &str = "invalid.nowhere.thing";
const UNRESOLVABLE_PATH: &str = "src/lexicons/invalid/nowhere/thing.json";
const GAME_PATH: &str = "src/lexicons/games/gamesgamesgamesgames/game.json";

fn owning_game() -> ProjectSnapshot {
    let mut snap = project::snapshot();
    project::add_lexicon(&mut snap, fixtures::game_record_lexicon(), GAME_PATH);
    snap
}

async fn lexicon_rows(app: &TestApp, id: &str) -> i64 {
    let sql = adapt_sql(
        "SELECT COUNT(*) FROM happyview_lexicons WHERE id = ?",
        app.state.db_backend,
    );
    let (n,): (i64,) = happyview::db::query_as(&sql)
        .bind(id)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    n
}

#[tokio::test]
#[serial]
async fn uploading_a_lexicon_a_project_file_owns_is_a_409_naming_the_file() {
    common::require_db!();
    let app = TestApp::new().await;
    app.state.project.store(Arc::new(owning_game()));

    let (status, body) = app
        .post_json_status(
            "/admin/lexicons",
            json!({ "lexicon_json": fixtures::game_record_lexicon(), "backfill": false }),
        )
        .await;

    assert_eq!(status, 409, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains(GAME_PATH),
        "{body}"
    );
    assert_eq!(
        lexicon_rows(&app, GAME).await,
        0,
        "the project layer never writes a row"
    );

    let listed = app.get_json("/admin/lexicons").await;
    let game = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["id"] == GAME)
        .unwrap();
    assert_eq!(game["layer"], "project");
    assert_eq!(game["source"], "project");
    assert_eq!(game["path"], GAME_PATH);
    assert_eq!(game["shadows_stored"], false);
    assert!(game["created_at"].is_null());
}

#[tokio::test]
#[serial]
async fn deleting_a_lexicon_a_project_file_shadows_is_a_409_and_the_row_survives() {
    common::require_db!();
    let app = TestApp::new().await;
    let (status, _) = app
        .post_json_status(
            "/admin/lexicons",
            json!({ "lexicon_json": fixtures::game_record_lexicon(), "backfill": false }),
        )
        .await;
    assert_eq!(status, 201);
    let listed = app.get_json(&format!("/admin/lexicons/{GAME}")).await;
    assert_eq!(listed["layer"], "manual");
    assert!(listed["path"].is_null());

    app.state.project.store(Arc::new(owning_game()));
    let (status, body) = app.delete_json(&format!("/admin/lexicons/{GAME}")).await;
    assert_eq!(status, 409, "{body}");
    let got = app.get_json(&format!("/admin/lexicons/{GAME}")).await;
    assert_eq!(got["layer"], "project");
    assert_eq!(got["shadows_stored"], true);

    app.state.project.store(Arc::new(ProjectSnapshot::empty()));
    let got = app.get_json(&format!("/admin/lexicons/{GAME}")).await;
    assert_eq!(got["layer"], "manual");
    assert_eq!(got["revision"], 1, "the shadowed row was never written to");
}

/// The first add uses an NSID no authority publishes, so a request that
/// reached resolution would fail some other way. 409 means the check came
/// first. The second add and the delete are caught by the catalog itself.
#[tokio::test]
#[serial]
async fn adding_or_removing_a_network_lexicon_a_project_file_owns_is_a_409() {
    common::require_db!();
    let app = TestApp::new().await;
    let mut snap = owning_game();
    project::add_lexicon(
        &mut snap,
        json!({
            "lexicon": 1,
            "id": UNRESOLVABLE,
            "defs": { "main": { "type": "record", "key": "tid", "record": {
                "type": "object", "properties": {}
            } } }
        }),
        UNRESOLVABLE_PATH,
    );
    app.state.project.store(Arc::new(snap));

    let (status, body) = app
        .post_json_status("/admin/network-lexicons", json!({ "nsid": UNRESOLVABLE }))
        .await;
    assert_eq!(status, 409, "{body}");

    let (status, body) = app
        .post_json_status("/admin/network-lexicons", json!({ "nsid": GAME }))
        .await;
    assert_eq!(status, 409, "{body}");

    let (status, body) = app
        .delete_json(&format!("/admin/network-lexicons/{GAME}"))
        .await;
    assert_eq!(status, 409, "{body}");
}
