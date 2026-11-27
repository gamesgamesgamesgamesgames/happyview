//! `identity:*`, `account:*` and `repo:*` scripts, fired from ingest. Needs
//! `TEST_DATABASE_URL` and the `interpreter_echo` fixture, whose
//! `host:log_input` body logs the whole run input as a `script.log` row.

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use happyview_provider::{Action, Origin, RecordEvent, RecordKey, SourceEvent};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;
use common::echo_interpreter;

const GAME: &str = "games.gamesgamesgamesgames.game";
const DID: &str = "did:plc:accountevents";
const STRANGER: &str = "did:plc:notindexed";
/// 2023-11-14T22:13:20Z.
const T: i64 = 1_700_000_000_000_000;

/// A fresh sequence number for each event, as the network gives.
fn next_seq() -> i64 {
    static SEQ: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);
    SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

macro_rules! require_fixture {
    () => {
        if !echo_interpreter::is_built() {
            happyview::test_support::unavailable(echo_interpreter::BUILD);
            return;
        }
    };
}

/// An app indexing `GAME`, with one record of `DID`'s, and the fixture
/// installed for `lua`.
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
                token_cost: None,
                authority_did: None,
                indexed: true,
            },
        )
        .await
        .expect("store the game lexicon");
    // What a served instance tracks, so a resync or a takedown purges.
    app.state
        .collections_tx
        .send_replace(app.state.lexicons.get_record_collections().await);
    app.state
        .plugin_registry
        .register(echo_interpreter::plugin("lua"))
        .await;
    let body = json!({"$type": GAME, "name": "held"});
    let _ = happyview::ingest::apply(
        &app.state,
        SourceEvent::Record(RecordEvent {
            key: RecordKey {
                did: DID.to_string(),
                collection: GAME.to_string(),
                rkey: "held".to_string(),
            },
            action: Action::Create,
            cid: None,
            rev: None,
            body: Some(body),
            time_us: None,
            origin: Origin::Live,
            stored: false,
        }),
    )
    .await;
    app
}

async fn install(app: &TestApp, trigger: &str, body: &str) {
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

/// The run inputs logged by `host:log_input` scripts, oldest first, as
/// `(trigger, input, caller_did)`.
async fn runs(app: &TestApp) -> Vec<(String, Value, Value)> {
    let rows: Vec<(String,)> = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT detail FROM happyview_event_logs WHERE event_type = 'script.log' ORDER BY created_at",
        app.state.db_backend,
    ))
    .fetch_all(&app.state.db)
    .await
    .expect("read script logs");
    rows.into_iter()
        .map(|(detail,)| {
            let detail: Value = serde_json::from_str(&detail).expect("detail json");
            let fields = &detail["fields"];
            (
                detail["trigger"].as_str().unwrap_or_default().to_string(),
                fields["input"].clone(),
                fields["context"]["caller_did"].clone(),
            )
        })
        .collect()
}

fn triggers(runs: &[(String, Value, Value)]) -> Vec<&str> {
    runs.iter().map(|(t, _, _)| t.as_str()).collect()
}

/// An identity event at `T` plus a fresh microsecond, since identity events
/// are known by their time.
fn identity(did: &str, handle: Option<&str>, as_of_us: Option<i64>) -> SourceEvent {
    SourceEvent::Identity {
        did: did.to_string(),
        handle: handle.map(str::to_string),
        as_of_us,
        time_us: Some(T + next_seq()),
    }
}

fn account(did: &str, active: bool, status: Option<&str>, as_of_us: Option<i64>) -> SourceEvent {
    SourceEvent::Account {
        did: did.to_string(),
        active,
        status: status.map(str::to_string),
        as_of_us,
        seq: Some(next_seq()),
        time_us: Some(T),
        collections: None,
    }
}

const ACCOUNT_TRIGGERS: [&str; 5] = [
    "account:update",
    "account:activate",
    "account:deactivate",
    "account:suspend",
    "account:delete",
];

#[tokio::test]
#[serial]
async fn identity_update_names_the_handle_and_the_one_before() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    install(&app, "identity:update", "host:log_input").await;

    let _ = happyview::ingest::apply(&app.state, identity(DID, Some("a.test"), None)).await;
    let _ = happyview::ingest::apply(&app.state, identity(DID, Some("b.test"), None)).await;
    let _ = happyview::ingest::apply(&app.state, identity(DID, None, None)).await;

    let runs = runs(&app).await;
    assert_eq!(triggers(&runs), ["identity:update"; 3]);
    assert_eq!(
        runs[0].1,
        json!({
            "did": DID,
            "handle": "a.test",
            "previous_handle": null,
            "time": runs[0].1["time"],
        })
    );
    assert_eq!(runs[1].1["previous_handle"], json!("a.test"));
    assert_eq!(runs[1].1["handle"], json!("b.test"));
    assert_eq!(
        runs[2].1["handle"],
        json!(null),
        "an event without a handle"
    );
    assert_eq!(runs[2].1["previous_handle"], json!("b.test"));
    // The run carries no caller, so nothing it calls acts with the account's
    // authority: every host write takes its account from the caller.
    assert_eq!(runs[0].2, json!(null), "no caller: {:?}", runs[0]);
}

#[tokio::test]
#[serial]
async fn nothing_fires_for_a_repo_not_indexed_or_a_replayed_marker() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    install(&app, "identity:update", "host:log_input").await;
    for t in ACCOUNT_TRIGGERS {
        install(&app, t, "host:log_input").await;
    }
    install(&app, "repo:sync", "host:log_input").await;

    for event in [
        identity(STRANGER, Some("x.test"), None),
        account(STRANGER, false, Some("deactivated"), None),
        SourceEvent::Sync {
            did: STRANGER.to_string(),
            rev: None,
            as_of_us: None,
            seq: None,
            time_us: Some(T),
            collections: None,
        },
        identity(DID, Some("a.test"), Some(T)),
        account(DID, false, Some("deactivated"), Some(T)),
        SourceEvent::Sync {
            did: DID.to_string(),
            rev: None,
            as_of_us: Some(T),
            seq: None,
            time_us: Some(T),
            collections: Some(vec![]),
        },
    ] {
        let _ = happyview::ingest::apply(&app.state, event).await;
    }
    assert!(runs(&app).await.is_empty());
}

#[tokio::test]
#[serial]
async fn account_events_fire_update_and_what_changed() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    for t in ACCOUNT_TRIGGERS {
        install(&app, t, "host:log_input").await;
    }

    for event in [
        account(DID, false, Some("deactivated"), None),
        account(DID, true, None, None),
        account(DID, true, None, None),
        account(DID, false, Some("suspended"), None),
    ] {
        let _ = happyview::ingest::apply(&app.state, event).await;
    }

    let runs = runs(&app).await;
    assert_eq!(
        triggers(&runs),
        [
            "account:update",
            "account:deactivate",
            "account:update",
            "account:activate",
            "account:update",
            "account:update",
            "account:suspend",
        ]
    );
    assert_eq!(
        runs[0].1,
        json!({
            "did": DID,
            "active": false,
            "status": "deactivated",
            "previous_active": null,
            "previous_status": null,
            "time": "2023-11-14T22:13:20.000000Z",
        })
    );
    assert_eq!(runs[2].1["previous_active"], json!(false));
    assert_eq!(runs[2].1["previous_status"], json!("deactivated"));
    assert_eq!(runs[5].1["previous_active"], json!(true));
}

#[tokio::test]
#[serial]
async fn account_delete_runs_while_the_records_are_still_held() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    install(&app, "account:delete", "host:log_input").await;

    let _ = happyview::ingest::apply(&app.state, account(DID, false, Some("deleted"), None)).await;

    let runs = runs(&app).await;
    assert_eq!(triggers(&runs), ["account:delete"]);
    // The script's row comes before the purge's.
    let order: Vec<(String,)> = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT event_type FROM happyview_event_logs \
         WHERE event_type IN ('script.log', 'jetstream.account_deleted') ORDER BY created_at",
        app.state.db_backend,
    ))
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    let order: Vec<&str> = order.iter().map(|(t,)| t.as_str()).collect();
    assert_eq!(order.first(), Some(&"script.log"), "{order:?}");
    assert!(
        app.state
            .records
            .get(&format!("at://{DID}/{GAME}/held"))
            .await
            .unwrap()
            .is_none(),
        "the account is purged after the script"
    );
}

#[tokio::test]
#[serial]
async fn repo_sync_names_the_rev() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    install(&app, "repo:sync", "host:log_input").await;

    let _ = happyview::ingest::apply(
        &app.state,
        SourceEvent::Sync {
            did: DID.to_string(),
            rev: Some("3kbcq3p7ad400".to_string()),
            as_of_us: None,
            seq: Some(2),
            time_us: Some(T),
            collections: None,
        },
    )
    .await;

    let runs = runs(&app).await;
    assert_eq!(
        triggers(&runs),
        ["repo:sync"],
        "it runs although the resync removed the records"
    );
    assert_eq!(
        runs[0].1,
        json!({ "did": DID, "rev": "3kbcq3p7ad400", "time": "2023-11-14T22:13:20.000000Z" })
    );
    assert!(
        app.state
            .records
            .get(&format!("at://{DID}/{GAME}/held"))
            .await
            .unwrap()
            .is_none(),
        "the resync purged the records first"
    );
}

/// A suspended account's records are purged, but it is still an account
/// this instance indexes: reinstated, it runs `account:activate`.
#[tokio::test]
#[serial]
async fn a_reinstated_account_still_runs_its_scripts() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    for t in ACCOUNT_TRIGGERS {
        install(&app, t, "host:log_input").await;
    }
    let _ =
        happyview::ingest::apply(&app.state, account(DID, false, Some("suspended"), None)).await;
    assert!(
        app.state
            .records
            .get(&format!("at://{DID}/{GAME}/held"))
            .await
            .unwrap()
            .is_none(),
        "a suspension purges"
    );
    // As if the cached answer had expired.
    app.state.repo_event_held.forget(DID);
    let _ = happyview::ingest::apply(&app.state, account(DID, true, None, None)).await;

    let runs = runs(&app).await;
    assert_eq!(
        triggers(&runs),
        [
            "account:update",
            "account:suspend",
            "account:update",
            "account:activate"
        ]
    );
    assert_eq!(runs[3].1["previous_status"], json!("suspended"));
}

/// Once its scripts have heard of a deletion, the account's recorded handle
/// and status are dropped.
#[tokio::test]
#[serial]
async fn a_deleted_accounts_state_is_not_kept() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    install(&app, "identity:update", "host:log_input").await;
    install(&app, "account:delete", "host:log_input").await;
    let _ = happyview::ingest::apply(&app.state, identity(DID, Some("a.test"), None)).await;
    let _ = happyview::ingest::apply(&app.state, account(DID, false, Some("deleted"), None)).await;
    let (rows,): (i64,) = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT COUNT(*) FROM happyview_repo_event_state WHERE did = ?",
        app.state.db_backend,
    ))
    .bind(DID)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(rows, 0);
}

#[tokio::test]
#[serial]
async fn a_failed_run_is_dead_lettered_and_retried_with_its_input() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    install(&app, "identity:update", "error:runtime").await;
    let _ = happyview::ingest::apply(&app.state, identity(DID, Some("a.test"), None)).await;

    let list = app.get_json("/admin/dead-letters").await;
    let row = &list["dead_letters"][0];
    assert_eq!(row["did"], json!(DID), "{list}");
    assert_eq!(row["action"], json!("identity:update"), "{list}");
    let id = row["id"].as_str().expect("dead letter listed").to_string();

    install(&app, "identity:update", "host:log_input").await;
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

    let runs = runs(&app).await;
    assert_eq!(triggers(&runs), ["identity:update"]);
    assert_eq!(runs[0].1["handle"], json!("a.test"));
    let (unresolved,): (i64,) = happyview::db::query_as(
        "SELECT COUNT(*) FROM happyview_dead_letter_scripts WHERE resolved_at IS NULL",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(unresolved, 0);
}

/// An event applied again, as ingest does after a failed write, runs nothing
/// twice: it carries the sequence number it had.
#[tokio::test]
#[serial]
async fn an_event_applied_again_runs_its_scripts_once() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    install(&app, "account:update", "host:log_input").await;
    install(&app, "account:deactivate", "host:log_input").await;
    let event = account(DID, false, Some("deactivated"), None);
    let _ = happyview::ingest::apply(&app.state, event.clone()).await;
    let _ = happyview::ingest::apply(&app.state, event).await;
    assert_eq!(
        triggers(&runs(&app).await),
        ["account:update", "account:deactivate"]
    );
}
