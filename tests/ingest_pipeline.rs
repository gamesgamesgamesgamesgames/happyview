//! The one ingest pipeline every record source goes through
//! (`happyview::ingest`). Needs `TEST_DATABASE_URL`; the hook tests also need
//! the `interpreter_echo` fixture.

mod common;

use std::sync::atomic::Ordering;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use happyview::lexicon::ProcedureAction;
use happyview_provider::{Action, IngestOutcome, Origin, RecordEvent, RecordKey, SourceEvent};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;
use common::echo_interpreter;

const GAME: &str = "games.gamesgamesgamesgames.game";
const DID: &str = "did:plc:ingest";

macro_rules! require_fixture {
    () => {
        if !echo_interpreter::is_built() {
            eprintln!("skipping: {}", echo_interpreter::BUILD);
            return;
        }
    };
}

async fn app() -> TestApp {
    let app = TestApp::new().await;
    app.state
        .lexicons
        .put_manual(
            &app.state.db,
            app.state.db_backend,
            happyview::catalog::LexiconWrite {
                lexicon_json: common::fixtures::game_record_lexicon(),
                backfill: false,
                target_collection: Some(GAME.to_string()),
                action: ProcedureAction::Upsert,
                token_cost: None,
                authority_did: None,
            },
        )
        .await
        .expect("store the game lexicon");
    app.state
        .plugin_registry
        .register(echo_interpreter::plugin("lua"))
        .await;
    app
}

async fn install_script(app: &TestApp, trigger: &str, body: &str) {
    app.state
        .scripts
        .put_manual(
            &app.state.db,
            app.state.db_backend,
            happyview::catalog::ScriptWrite {
                id: trigger.to_string(),
                script_type: "lua".to_string(),
                body: body.to_string(),
                description: None,
            },
        )
        .await
        .expect("install script");
}

fn cid_of(body: &Value) -> String {
    happyview::cid_verify::compute_record_cid(body)
        .expect("cid")
        .to_string()
}

fn create(rkey: &str, body: Value, cid: Option<String>, origin: Origin) -> SourceEvent {
    SourceEvent::Record(RecordEvent {
        key: RecordKey {
            did: DID.to_string(),
            collection: GAME.to_string(),
            rkey: rkey.to_string(),
        },
        action: Action::Create,
        cid,
        rev: None,
        body: Some(body),
        time_us: None,
        origin,
        stored: false,
    })
}

fn uri(rkey: &str) -> String {
    format!("at://{DID}/{GAME}/{rkey}")
}

async fn stored(app: &TestApp, rkey: &str) -> Option<Value> {
    app.state
        .records
        .get(&uri(rkey))
        .await
        .expect("read record")
        .map(|r| serde_json::from_str(&r.record).expect("record json"))
}

async fn count_events(app: &TestApp, event_type: &str, subject: &str) -> i64 {
    let (n,): (i64,) = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT COUNT(*) FROM happyview_event_logs WHERE event_type = ? AND subject = ?",
        app.state.db_backend,
    ))
    .bind(event_type)
    .bind(subject)
    .fetch_one(&app.state.db)
    .await
    .expect("count events");
    n
}

#[tokio::test]
#[serial]
async fn a_redelivered_event_changes_nothing() {
    common::require_db!();
    let app = app().await;
    app.state
        .verbose_event_logging
        .store(true, Ordering::Relaxed);
    let body = json!({"$type": GAME, "name": "redelivered"});
    let event = || {
        let SourceEvent::Record(mut ev) =
            create("redo", body.clone(), Some(cid_of(&body)), Origin::Live)
        else {
            unreachable!()
        };
        ev.rev = Some(happyview::tid::tid_from_unix_microseconds(
            1_700_000_000_000_000,
        ));
        SourceEvent::Record(ev)
    };

    for _ in 0..2 {
        assert_eq!(
            happyview::ingest::apply(&app.state, event()).await,
            IngestOutcome::Matched
        );
    }

    let (rows,): (i64,) = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT COUNT(*) FROM happyview_records WHERE uri = ?",
        app.state.db_backend,
    ))
    .bind(uri("redo"))
    .fetch_one(&app.state.db)
    .await
    .expect("count rows");
    assert_eq!(rows, 1);
    assert_eq!(count_events(&app, "record.created", &uri("redo")).await, 1);
}

#[tokio::test]
#[serial]
async fn crawl_live_and_replay_store_the_same_body_through_one_path() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    install_script(
        &app,
        &format!("record.create:{GAME}"),
        r#"returns:{"name":"hooked"}"#,
    )
    .await;
    let body = json!({"$type": GAME, "name": "arrived"});
    let cid = Some(cid_of(&body));

    let live = happyview::ingest::apply(
        &app.state,
        create("live", body.clone(), cid.clone(), Origin::Live),
    )
    .await;
    assert_eq!(live, IngestOutcome::Matched);
    let crawled = happyview::ingest::apply(
        &app.state,
        create(
            "crawl",
            body.clone(),
            cid.clone(),
            Origin::Backfill {
                job_id: "job".into(),
            },
        ),
    )
    .await;
    assert_eq!(crawled, IngestOutcome::Matched);
    let batch = happyview::ingest::apply_batch(
        &app.state,
        vec![create(
            "replay",
            body.clone(),
            cid.clone(),
            Origin::Backfill {
                job_id: "job".into(),
            },
        )],
    )
    .await;
    assert_eq!(batch.outcomes, vec![IngestOutcome::Matched]);
    assert_eq!(batch.changed, vec![true]);

    for rkey in ["live", "crawl", "replay"] {
        assert_eq!(
            stored(&app, rkey).await,
            Some(json!({"name": "hooked"})),
            "{rkey}"
        );
    }
}

#[tokio::test]
#[serial]
async fn a_dead_letter_retry_applies_the_hook_outcome() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    let trigger = format!("record.create:{GAME}");
    install_script(&app, &trigger, "error:runtime").await;
    let body = json!({"$type": GAME, "name": "arrived"});

    let _ = happyview::ingest::apply(
        &app.state,
        create("dl", body.clone(), Some(cid_of(&body)), Origin::Live),
    )
    .await;
    assert_eq!(stored(&app, "dl").await, Some(body), "fail-open stores it");

    install_script(&app, &trigger, r#"returns:{"text":"x"}"#).await;
    let list = app.get_json("/admin/dead-letters").await;
    let id = list["dead_letters"][0]["id"]
        .as_str()
        .expect("dead letter listed")
        .to_string();
    let cookie = app.admin_cookie();
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/admin/dead-letters/{id}/retry"))
                .header(cookie.0, cookie.1)
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );

    assert_eq!(stored(&app, "dl").await, Some(json!({"text": "x"})));
    let (unresolved,): (i64,) = happyview::db::query_as(
        "SELECT COUNT(*) FROM happyview_dead_letter_scripts WHERE resolved_at IS NULL",
    )
    .fetch_one(&app.state.db)
    .await
    .expect("count dead letters");
    assert_eq!(unresolved, 0);
}

#[tokio::test]
#[serial]
async fn a_dead_letter_retry_whose_hook_still_fails_keeps_its_row() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    let trigger = format!("record.create:{GAME}");
    install_script(&app, &trigger, "error:runtime").await;
    let body = json!({"$type": GAME, "name": "arrived"});
    let _ = happyview::ingest::apply(
        &app.state,
        create("dl2", body.clone(), Some(cid_of(&body)), Origin::Live),
    )
    .await;

    let list = app.get_json("/admin/dead-letters").await;
    let id = list["dead_letters"][0]["id"]
        .as_str()
        .expect("dead letter listed")
        .to_string();
    let (status, _) = app
        .post_json_status(&format!("/admin/dead-letters/{id}/retry"), json!({}))
        .await;
    assert_eq!(status, 500);

    let (rows, unresolved): (i64, i64) = happyview::db::query_as(
        "SELECT COUNT(*), SUM(CASE WHEN resolved_at IS NULL THEN 1 ELSE 0 END) \
         FROM happyview_dead_letter_scripts",
    )
    .fetch_one(&app.state.db)
    .await
    .expect("count dead letters");
    assert_eq!((rows, unresolved), (1, 1), "no new dead letter, still open");
}

#[tokio::test]
#[serial]
async fn a_cid_mismatch_is_skipped_and_logged() {
    common::require_db!();
    let app = app().await;
    let body = json!({"$type": GAME, "name": "claimed"});
    let other = cid_of(&json!({"other": true}));

    let outcome =
        happyview::ingest::apply(&app.state, create("cid", body, Some(other), Origin::Live)).await;

    assert_eq!(outcome, IngestOutcome::Skipped);
    assert_eq!(stored(&app, "cid").await, None);
    assert_eq!(
        count_events(&app, "record.cid_mismatch", &uri("cid")).await,
        1
    );
}

/// Postgres only: purges run with `happyview.change_reason` set, so a
/// trigger on the records table can label what it sees.
#[tokio::test]
#[serial]
async fn the_change_reason_is_set_inside_purge_transactions() {
    common::require_db!();
    let app = app().await;
    if app.state.db_backend != happyview::db::DatabaseBackend::Postgres {
        return;
    }
    let did = format!("did:plc:reason{}", uuid::Uuid::new_v4().simple());
    let body = json!({"$type": GAME, "name": "doomed"});
    let event = SourceEvent::Record(RecordEvent {
        key: RecordKey {
            did: did.clone(),
            collection: GAME.to_string(),
            rkey: "r".to_string(),
        },
        action: Action::Create,
        cid: None,
        rev: None,
        body: Some(body),
        time_us: None,
        origin: Origin::Live,
        stored: false,
    });
    assert_eq!(
        happyview::ingest::apply(&app.state, event).await,
        IngestOutcome::Matched
    );

    for sql in [
        "CREATE TABLE IF NOT EXISTS test_change_reasons (reason TEXT)",
        "DELETE FROM test_change_reasons",
        "CREATE OR REPLACE FUNCTION test_change_reason() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN INSERT INTO test_change_reasons VALUES (current_setting('happyview.change_reason', true)); \
         RETURN OLD; END $$",
        "DROP TRIGGER IF EXISTS test_change_reason ON happyview_records_v2",
        "CREATE TRIGGER test_change_reason AFTER DELETE ON happyview_records_v2 \
         FOR EACH ROW EXECUTE FUNCTION test_change_reason()",
    ] {
        happyview::db::query(sql)
            .execute(&app.state.db)
            .await
            .expect(sql);
    }

    let outcome = happyview::ingest::apply(
        &app.state,
        SourceEvent::Account {
            did: did.clone(),
            active: false,
            status: Some("deleted".to_string()),
            as_of_us: None,
            seq: Some(1),
            time_us: Some(1_779_719_000_000_000),
        },
    )
    .await;
    let reasons: Vec<(Option<String>,)> =
        happyview::db::query_as("SELECT reason FROM test_change_reasons")
            .fetch_all(&app.state.db)
            .await
            .expect("read reasons");
    for sql in [
        "DROP TRIGGER test_change_reason ON happyview_records_v2",
        "DROP FUNCTION test_change_reason()",
        "DROP TABLE test_change_reasons",
    ] {
        happyview::db::query(sql)
            .execute(&app.state.db)
            .await
            .expect(sql);
    }

    assert_eq!(outcome, IngestOutcome::Matched);
    assert_eq!(reasons, vec![(Some("purge:deleted".to_string()),)]);
}
