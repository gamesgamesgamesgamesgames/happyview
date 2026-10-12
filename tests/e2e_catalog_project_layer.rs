//! The project layer through the real admin routes: what a file owns answers
//! 409 naming the file, listings say which layer answers, and a dashboard row
//! a file shadows survives untouched.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request};
use happyview::db::adapt_sql;
use happyview::project::ProjectSnapshot;
use http_body_util::BodyExt;
use serde_json::json;
use serial_test::serial;
use tower::ServiceExt;

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

/// The stats page lists collections from the catalog, so a record lexicon
/// only a file defines is a collection like any other.
#[tokio::test]
#[serial]
async fn stats_include_project_record_lexicons() {
    common::require_db!();
    let app = TestApp::new().await;
    app.state.project.store(Arc::new(owning_game()));

    let stats = app.get_json("/admin/stats").await;
    let game = stats["collections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["collection"] == GAME);
    assert_eq!(game.map(|c| c["count"].clone()), Some(json!(0)), "{stats}");
}

/// The records page lists collections from the catalog too.
#[tokio::test]
#[serial]
async fn collection_listings_include_project_record_lexicons() {
    common::require_db!();
    let app = TestApp::new().await;
    app.state.project.store(Arc::new(owning_game()));

    let listed = app.get_json("/admin/records/collections").await;
    assert!(
        listed["collections"]
            .as_array()
            .unwrap()
            .contains(&json!(GAME)),
        "{listed}"
    );
}

const SCRIPT: &str = "xrpc.query:games.gamesgamesgamesgames.listGames";
const SCRIPT_PATH: &str = "src/lexicons/games/gamesgamesgamesgames/listGames.lua";

fn owning_script() -> ProjectSnapshot {
    let mut snap = project::snapshot();
    project::add_script(
        &mut snap,
        SCRIPT,
        "function handle() return {} end",
        SCRIPT_PATH,
    );
    snap
}

fn encoded(id: &str) -> String {
    id.replace(':', "%3A")
}

async fn patch_status(app: &TestApp, path: &str, body: serde_json::Value) -> u16 {
    let cookie = app.admin_cookie();
    let req = Request::builder()
        .method(Method::PATCH)
        .uri(path)
        .header(cookie.0, cookie.1)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let _ = resp.into_body().collect().await;
    status
}

#[tokio::test]
#[serial]
async fn creating_a_script_a_project_file_owns_is_a_409_naming_the_file() {
    common::require_db!();
    let app = TestApp::new().await;
    app.state.project.store(Arc::new(owning_script()));

    let (status, body) = app
        .post_json_status(
            "/admin/scripts",
            json!({ "id": SCRIPT, "body": "function handle() end" }),
        )
        .await;
    assert_eq!(status, 409, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains(SCRIPT_PATH),
        "{body}"
    );

    let listed = app.get_json("/admin/scripts").await;
    let script = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == SCRIPT)
        .unwrap();
    assert_eq!(script["layer"], "project");
    assert_eq!(script["path"], SCRIPT_PATH);
    assert_eq!(script["shadows_stored"], false);
    assert!(script["updated_at"].is_null());
}

#[tokio::test]
#[serial]
async fn every_write_to_a_shadowed_script_is_a_409_and_the_row_survives() {
    common::require_db!();
    let app = TestApp::new().await;
    app.state
        .scripts
        .put_manual(
            &app.state.db,
            app.state.db_backend,
            happyview::catalog::ScriptWrite {
                id: SCRIPT.into(),
                script_type: "lua".into(),
                body: "row".into(),
                description: None,
            },
        )
        .await
        .unwrap();
    app.state.project.store(Arc::new(owning_script()));
    let path = format!("/admin/scripts/{}", encoded(SCRIPT));

    assert_eq!(app.delete_json(&path).await.0, 409);
    assert_eq!(
        patch_status(&app, &path, json!({ "description": "x" })).await,
        409
    );
    assert_eq!(
        app.post_json_status(&format!("{path}/codemod"), json!({ "apply": true }))
            .await
            .0,
        409
    );

    let got = app.get_json(&path).await;
    assert_eq!(got["layer"], "project");
    assert_eq!(got["shadows_stored"], true);

    app.state.project.store(Arc::new(ProjectSnapshot::empty()));
    let got = app.get_json(&path).await;
    assert_eq!(got["layer"], "manual");
    assert_eq!(got["body"], "row");
    assert!(got["description"].is_null(), "the patch never landed");
}

/// Each of these would be refused for some other reason, or not at all, if
/// the handler's own ownership check were missing: a patch carrying a body
/// reaches interpreter validation (400 here, no interpreter claims `lua`)
/// before it reaches the catalog's write, and a codemod that rewrites
/// nothing never reaches the catalog's write at all.
#[tokio::test]
#[serial]
async fn the_handlers_refuse_before_validating_or_rewriting() {
    common::require_db!();
    let app = TestApp::new().await;
    let mut snap = project::snapshot();
    project::add_script(
        &mut snap,
        SCRIPT,
        "function handle(input, ctx)\n  return {}\nend\n",
        SCRIPT_PATH,
    );
    app.state.project.store(Arc::new(snap));
    let path = format!("/admin/scripts/{}", encoded(SCRIPT));

    assert_eq!(
        patch_status(&app, &path, json!({ "body": "function handle() end" })).await,
        409
    );
    assert_eq!(
        app.post_json_status(&format!("{path}/codemod"), json!({ "apply": true }))
            .await
            .0,
        409
    );
}

#[tokio::test]
#[serial]
async fn applying_the_codemod_to_a_non_lua_project_script_is_a_409_not_a_400() {
    common::require_db!();
    let app = TestApp::new().await;
    let mut snap = owning_script();
    snap.scripts.get_mut(SCRIPT).unwrap().language_id = "typescript".to_string();
    app.state.project.store(Arc::new(snap));
    let path = format!("/admin/scripts/{}", encoded(SCRIPT));

    assert_eq!(
        app.post_json_status(&format!("{path}/codemod"), json!({ "apply": true }))
            .await
            .0,
        409
    );
    assert_eq!(
        app.post_json_status(&format!("{path}/codemod"), json!({}))
            .await
            .0,
        400,
        "a preview is not a write and still reports the language"
    );
}

#[tokio::test]
#[serial]
async fn a_bodyless_patch_of_a_stored_unmigrated_script_still_succeeds() {
    common::require_db!();
    let app = TestApp::new().await;
    app.state
        .scripts
        .put_manual(
            &app.state.db,
            app.state.db_backend,
            happyview::catalog::ScriptWrite {
                id: SCRIPT.into(),
                script_type: "lua".into(),
                body: "function handle()\n  return params.q\nend\n".into(),
                description: None,
            },
        )
        .await
        .unwrap();
    let path = format!("/admin/scripts/{}", encoded(SCRIPT));

    assert_eq!(
        patch_status(&app, &path, json!({ "description": "kept" })).await,
        200
    );
    let got = app.get_json(&path).await;
    assert_eq!(got["description"], "kept");
    assert_eq!(got["layer"], "manual");
}
