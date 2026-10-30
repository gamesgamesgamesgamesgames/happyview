//! Reconciling a collection's stored records with its accounts' PDSes
//! (`happyview.reconcile-collection`, HAP-270).
//!
//! Each account's DID document and PDS are mocked on the app's mock server:
//! the document at `/{did}`, the PDS under `/pds/{did}`.

mod common;

use axum::body::Body;
use axum::http::Request;
use happyview::db::adapt_sql;
use happyview::jobs::native::reconcile_collection::{self as reconcile, Options, Report};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use tower::ServiceExt;

use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, Respond, ResponseTemplate};

use common::app::TestApp;
use common::fixtures;

const GAME: &str = "games.gamesgamesgamesgames.game";
const ALICE: &str = "did:plc:reconcilealice";

/// Run `run` as job `job_id`'s run, as a worker would: claimed, and under
/// the claim's lease. A job's progress is written only from its run.
async fn as_the_jobs_run<F: std::future::Future>(app: &TestApp, job_id: &str, run: F) -> F::Output {
    const OWNER: &str = "reconcile-test";
    happyview::db::query(&adapt_sql(
        "UPDATE happyview_jobs SET status = 'running', claimed_by = ?, attempt = 1 WHERE id = ?",
        app.state.db_backend,
    ))
    .bind(OWNER)
    .bind(job_id)
    .execute(&app.state.db)
    .await
    .expect("claim the job");
    let lease = happyview::jobs::held::Lease {
        owner: OWNER.into(),
        attempt: 1,
    };
    happyview::jobs::held::run_under(happyview::jobs::held::Held::new(job_id, lease), run).await
}

async fn app() -> TestApp {
    let app = TestApp::new().await;
    let (status, body) = app
        .post_json_status(
            "/admin/lexicons",
            json!({ "lexicon_json": fixtures::game_record_lexicon(), "backfill": false }),
        )
        .await;
    assert!(status == 201 || status == 200, "{status}: {body}");
    app
}

fn uri(did: &str, rkey: &str) -> String {
    format!("at://{did}/{GAME}/{rkey}")
}

/// A network row of `did`'s, as Jetstream or a crawl stores one.
async fn seed(app: &TestApp, did: &str, rkey: &str, cid: &str) {
    let sql = adapt_sql(
        "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        app.state.db_backend,
    );
    let now = happyview::db::now_rfc3339();
    happyview::db::query(&sql)
        .bind(uri(did, rkey))
        .bind(did)
        .bind(GAME)
        .bind(rkey)
        .bind(json!({ "$type": GAME, "title": rkey }).to_string())
        .bind(cid)
        .bind(&now)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .expect("seed a record");
}

async fn stored(app: &TestApp, did: &str, rkey: &str) -> bool {
    app.state
        .records
        .get(&uri(did, rkey))
        .await
        .expect("read a record")
        .is_some()
}

fn pds(did: &str) -> String {
    format!("/pds/{did}")
}

/// `did`'s DID document, naming its PDS on the mock server.
async fn document(app: &TestApp, did: &str) {
    Mock::given(method("GET"))
        .and(path(format!("/{did}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixtures::did_document(
                did,
                &format!("{}{}", app.mock_server.uri(), pds(did)),
            )),
        )
        .mount(&app.mock_server)
        .await;
}

/// One listRecords page of `(rkey, cid)`s, with `cursor` when another follows.
fn page(did: &str, records: &[(&str, &str)], cursor: Option<&str>) -> Value {
    json!({
        "records": records.iter().map(|(rkey, cid)| json!({
            "uri": uri(did, rkey),
            "cid": cid,
            "value": { "$type": GAME, "title": rkey },
        })).collect::<Vec<_>>(),
        "cursor": cursor,
    })
}

/// listRecords answering one complete page.
async fn listing(app: &TestApp, did: &str, records: &[(&str, &str)]) {
    Mock::given(method("GET"))
        .and(path(format!(
            "{}/xrpc/com.atproto.repo.listRecords",
            pds(did)
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(did, records, None)))
        .mount(&app.mock_server)
        .await;
}

/// getRecord for `rkey`: gone (400 RecordNotFound), or still there.
async fn get_record(app: &TestApp, did: &str, rkey: &str, gone: bool) {
    let response = if gone {
        ResponseTemplate::new(400).set_body_json(
            json!({ "error": "RecordNotFound", "message": "Could not locate record" }),
        )
    } else {
        ResponseTemplate::new(200).set_body_json(json!({
            "uri": uri(did, rkey),
            "cid": "bafystill",
            "value": { "$type": GAME, "title": rkey },
        }))
    };
    Mock::given(method("GET"))
        .and(path(format!(
            "{}/xrpc/com.atproto.repo.getRecord",
            pds(did)
        )))
        .and(query_param("rkey", rkey))
        .respond_with(response)
        .mount(&app.mock_server)
        .await;
}

fn options(dry_run: bool) -> Options {
    Options {
        collection: GAME.into(),
        dry_run,
        dids: None,
        pds_requests_per_second: Some(20),
        allow_suspicious: vec![],
    }
}

/// Answers `response`, but on its `nth` call first runs `sql` against the
/// app's database and waits for it: what Jetstream or an operator does while
/// the job is waiting on the PDS.
struct Meanwhile {
    pool: sqlx::AnyPool,
    sql: String,
    nth: usize,
    calls: AtomicUsize,
    response: ResponseTemplate,
    runtime: tokio::runtime::Handle,
}

impl Meanwhile {
    fn new(app: &TestApp, sql: &str, nth: usize, response: ResponseTemplate) -> Self {
        Self {
            pool: app.state.db.clone(),
            sql: adapt_sql(sql, app.state.db_backend),
            nth,
            calls: AtomicUsize::new(0),
            response,
            runtime: tokio::runtime::Handle::current(),
        }
    }
}

impl Respond for Meanwhile {
    fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
        if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == self.nth {
            let (pool, sql, runtime) = (self.pool.clone(), self.sql.clone(), self.runtime.clone());
            std::thread::spawn(move || {
                runtime.block_on(async {
                    happyview::db::query(&sql)
                        .execute(&pool)
                        .await
                        .expect("run the statement")
                })
            })
            .join()
            .expect("the statement's thread");
        }
        self.response.clone()
    }
}

fn record_not_found() -> ResponseTemplate {
    ResponseTemplate::new(400)
        .set_body_json(json!({ "error": "RecordNotFound", "message": "Could not locate record" }))
}

fn get_record_path(did: &str) -> String {
    format!("{}/xrpc/com.atproto.repo.getRecord", pds(did))
}

async fn run(app: &TestApp, options: &Options) -> Report {
    reconcile::reconcile(&app.state, options, None)
        .await
        .expect("reconcile runs")
}

#[tokio::test]
async fn a_dry_run_reports_what_an_apply_then_deletes() {
    let app = app().await;
    for (rkey, cid) in [("g1", "bafyg1"), ("g2", "bafyg2"), ("g3", "bafyg3")] {
        seed(&app, ALICE, rkey, cid).await;
    }
    document(&app, ALICE).await;
    // g1 is gone; g3 changed on the PDS.
    listing(&app, ALICE, &[("g2", "bafyg2"), ("g3", "bafynew")]).await;
    get_record(&app, ALICE, "g1", true).await;

    let report = run(&app, &options(true)).await;
    assert_eq!(report.dids_checked, 1);
    assert_eq!(report.missing_total, 1);
    assert_eq!(report.missing[0].uri, uri(ALICE, "g1"));
    assert_eq!(report.missing[0].outcome, "missing");
    assert_eq!(report.deleted, 0);
    assert_eq!(report.stale_cid_total, 1);
    assert_eq!(report.stale_cid[0].pds_cid, "bafynew");
    assert!(stored(&app, ALICE, "g1").await, "a dry run changes nothing");

    let report = run(&app, &options(false)).await;
    assert_eq!(report.deleted, 1);
    assert_eq!(report.missing[0].outcome, "deleted");
    assert!(!stored(&app, ALICE, "g1").await);
    assert!(stored(&app, ALICE, "g2").await);
    // Logged whatever the event-logging setting, so it can be found later.
    let logged = app
        .get_json(&format!(
            "/admin/events?event_type=record.deleted&subject={}",
            urlencoding::encode(&uri(ALICE, "g1"))
        ))
        .await;
    assert_eq!(
        logged["events"][0]["detail"]["origin"],
        reconcile::JOB_TYPE,
        "{logged}"
    );
    assert!(
        stored(&app, ALICE, "g3").await,
        "a stale CID is only reported"
    );
}

#[tokio::test]
async fn a_candidate_the_pds_still_has_is_left_alone() {
    let app = app().await;
    for rkey in ["g1", "g2"] {
        seed(&app, ALICE, rkey, &format!("bafy{rkey}")).await;
    }
    document(&app, ALICE).await;
    // Missing from the listing, but getRecord finds it: created meanwhile,
    // or a listing that lagged.
    listing(&app, ALICE, &[("g2", "bafyg2")]).await;
    get_record(&app, ALICE, "g1", false).await;

    let report = run(&app, &options(false)).await;
    assert_eq!(report.missing_total, 0);
    assert_eq!(report.unconfirmed_total, 1);
    assert_eq!(report.deleted, 0);
    assert!(stored(&app, ALICE, "g1").await);
}

#[tokio::test]
async fn a_suspicious_listing_is_skipped_unless_an_apply_names_the_account() {
    let app = app().await;
    let rkeys: Vec<String> = (0..12).map(|i| format!("s{i:02}")).collect();
    for rkey in &rkeys {
        seed(&app, ALICE, rkey, &format!("bafy{rkey}")).await;
    }
    document(&app, ALICE).await;
    // The PDS lists one of twelve, as a migrated account's old PDS might.
    listing(&app, ALICE, &[("s00", "bafys00")]).await;
    for rkey in &rkeys[1..] {
        get_record(&app, ALICE, rkey, true).await;
    }

    let report = run(&app, &options(false)).await;
    assert_eq!(report.dids_checked, 0);
    assert_eq!(report.dids_skipped.len(), 1);
    assert_eq!(report.dids_skipped[0].reason, "suspicious_listing");
    assert_eq!(report.deleted, 0);
    assert!(stored(&app, ALICE, "s05").await);

    let named = Options {
        allow_suspicious: vec![ALICE.into()],
        ..options(false)
    };
    let report = run(&app, &named).await;
    assert_eq!(report.deleted, 11);
    assert!(stored(&app, ALICE, "s00").await);
    assert!(!stored(&app, ALICE, "s05").await);
}

#[tokio::test]
async fn an_empty_listing_is_suspicious() {
    let app = app().await;
    seed(&app, ALICE, "g1", "bafyg1").await;
    document(&app, ALICE).await;
    listing(&app, ALICE, &[]).await;
    get_record(&app, ALICE, "g1", true).await;

    let report = run(&app, &options(false)).await;
    assert_eq!(report.dids_skipped[0].reason, "suspicious_listing");
    assert!(stored(&app, ALICE, "g1").await);
}

#[tokio::test]
async fn a_listing_that_fails_part_way_skips_the_account() {
    let app = app().await;
    for rkey in ["g1", "g2"] {
        seed(&app, ALICE, rkey, &format!("bafy{rkey}")).await;
    }
    document(&app, ALICE).await;
    let list = format!("{}/xrpc/com.atproto.repo.listRecords", pds(ALICE));
    Mock::given(method("GET"))
        .and(path(list.clone()))
        .and(query_param("cursor", "next"))
        .respond_with(ResponseTemplate::new(502))
        .mount(&app.mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path(list))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            ALICE,
            &[("g2", "bafyg2")],
            Some("next"),
        )))
        .mount(&app.mock_server)
        .await;
    get_record(&app, ALICE, "g1", true).await;

    let report = run(&app, &options(false)).await;
    assert_eq!(report.dids_skipped[0].reason, "listing_failed");
    assert_eq!(report.deleted, 0);
    assert!(stored(&app, ALICE, "g1").await);
}

#[tokio::test]
async fn a_repo_the_pds_cannot_find_skips_the_account() {
    let app = app().await;
    seed(&app, ALICE, "g1", "bafyg1").await;
    document(&app, ALICE).await;
    Mock::given(method("GET"))
        .and(path(format!(
            "{}/xrpc/com.atproto.repo.listRecords",
            pds(ALICE)
        )))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "InvalidRequest",
            "message": format!("Could not find repo: {ALICE}"),
        })))
        .mount(&app.mock_server)
        .await;

    let report = run(&app, &options(false)).await;
    assert_eq!(report.dids_skipped[0].reason, "repo_unavailable");
    assert!(stored(&app, ALICE, "g1").await);
}

#[tokio::test]
async fn a_rate_limited_request_is_retried() {
    let app = app().await;
    for rkey in ["g1", "g2"] {
        seed(&app, ALICE, rkey, &format!("bafy{rkey}")).await;
    }
    document(&app, ALICE).await;
    let list = format!("{}/xrpc/com.atproto.repo.listRecords", pds(ALICE));
    Mock::given(method("GET"))
        .and(path(list.clone()))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&app.mock_server)
        .await;
    listing(&app, ALICE, &[("g2", "bafyg2")]).await;
    get_record(&app, ALICE, "g1", true).await;

    let report = run(&app, &options(false)).await;
    assert!(report.dids_skipped.is_empty(), "{:?}", report.dids_skipped);
    assert_eq!(report.deleted, 1);
}

#[tokio::test]
async fn a_delete_hook_that_keeps_the_record_is_honoured() {
    if !common::echo_interpreter::is_built() {
        eprintln!("skipping: {}", common::echo_interpreter::BUILD);
        return;
    }
    let app = app().await;
    keeping_delete_hook(&app).await;
    for rkey in ["g1", "g2"] {
        seed(&app, ALICE, rkey, &format!("bafy{rkey}")).await;
    }
    document(&app, ALICE).await;
    listing(&app, ALICE, &[("g2", "bafyg2")]).await;
    get_record(&app, ALICE, "g1", true).await;

    let report = run(&app, &options(false)).await;
    assert_eq!(report.missing_total, 1);
    assert_eq!(report.kept_by_hook, 1);
    assert_eq!(report.missing[0].outcome, "kept_by_hook");
    assert_eq!(report.deleted, 0);
    assert!(stored(&app, ALICE, "g1").await, "the hook kept it");
}

#[tokio::test]
async fn a_bare_404_does_not_confirm_a_miss() {
    let app = app().await;
    for rkey in ["g1", "g2", "g3"] {
        seed(&app, ALICE, rkey, &format!("bafy{rkey}")).await;
    }
    document(&app, ALICE).await;
    listing(&app, ALICE, &[("g3", "bafyg3")]).await;
    // A proxy's 404, and a PDS that doesn't route getRecord.
    for (rkey, response) in [
        (
            "g1",
            ResponseTemplate::new(404).set_body_string("Not Found"),
        ),
        (
            "g2",
            ResponseTemplate::new(404).set_body_json(json!({ "error": "MethodNotImplemented" })),
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(get_record_path(ALICE)))
            .and(query_param("rkey", rkey))
            .respond_with(response)
            .mount(&app.mock_server)
            .await;
    }

    let report = run(&app, &options(false)).await;
    assert_eq!(report.missing_total, 0);
    assert_eq!(report.unconfirmed_total, 2);
    assert_eq!(report.deleted, 0);
    assert!(stored(&app, ALICE, "g1").await);
    assert!(stored(&app, ALICE, "g2").await);
}

#[tokio::test]
async fn a_404_naming_record_not_found_confirms_a_miss() {
    let app = app().await;
    for rkey in ["g1", "g2"] {
        seed(&app, ALICE, rkey, &format!("bafy{rkey}")).await;
    }
    document(&app, ALICE).await;
    listing(&app, ALICE, &[("g2", "bafyg2")]).await;
    Mock::given(method("GET"))
        .and(path(get_record_path(ALICE)))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({ "error": "RecordNotFound" })),
        )
        .mount(&app.mock_server)
        .await;

    let report = run(&app, &options(false)).await;
    assert_eq!(report.deleted, 1);
    assert!(!stored(&app, ALICE, "g1").await);
}

#[tokio::test]
async fn an_empty_page_that_carries_a_cursor_fails_the_listing() {
    let app = app().await;
    for rkey in ["g1", "g2"] {
        seed(&app, ALICE, rkey, &format!("bafy{rkey}")).await;
    }
    document(&app, ALICE).await;
    let list = format!("{}/xrpc/com.atproto.repo.listRecords", pds(ALICE));
    Mock::given(method("GET"))
        .and(path(list.clone()))
        .and(query_param("cursor", "next"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(ALICE, &[], Some("more"))))
        .mount(&app.mock_server)
        .await;
    // Followed, the cursor ends the listing: only the empty page's own
    // cursor says it was cut short.
    Mock::given(method("GET"))
        .and(path(list.clone()))
        .and(query_param("cursor", "more"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(ALICE, &[], None)))
        .mount(&app.mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path(list))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            ALICE,
            &[("g2", "bafyg2")],
            Some("next"),
        )))
        .mount(&app.mock_server)
        .await;
    get_record(&app, ALICE, "g1", true).await;

    let report = run(&app, &options(false)).await;
    assert_eq!(report.dids_skipped[0].reason, "listing_failed");
    assert!(
        report.dids_skipped[0].detail.contains("empty page"),
        "{:?}",
        report.dids_skipped
    );
    assert_eq!(report.deleted, 0);
    assert!(stored(&app, ALICE, "g1").await);
}

/// A delete hook that keeps every record (the echo interpreter's
/// `value:none`): a run that reaches it reports `kept_by_hook`.
async fn keeping_delete_hook(app: &TestApp) {
    app.state
        .plugin_registry
        .register(common::echo_interpreter::plugin("lua"))
        .await;
    app.state
        .scripts
        .put_manual(
            &app.state.db,
            app.state.db_backend,
            happyview::catalog::ScriptWrite {
                id: format!("record.delete:{GAME}"),
                script_type: "lua".to_string(),
                body: "value:none".to_string(),
                description: None,
            },
        )
        .await
        .expect("install the delete hook");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_record_recreated_after_its_check_is_kept() {
    if !common::echo_interpreter::is_built() {
        eprintln!("skipping: {}", common::echo_interpreter::BUILD);
        return;
    }
    let app = app().await;
    // Had it run, this hook would keep the record and say so; the recreated
    // record must not reach it at all.
    keeping_delete_hook(&app).await;
    for rkey in ["g1", "g2"] {
        seed(&app, ALICE, rkey, &format!("bafy{rkey}")).await;
    }
    document(&app, ALICE).await;
    listing(&app, ALICE, &[("g2", "bafyg2")]).await;
    // The PDS says g1 is gone; before the job acts, its owner recreates it
    // and Jetstream stores the new one at the same key.
    Mock::given(method("GET"))
        .and(path(get_record_path(ALICE)))
        .and(query_param("rkey", "g1"))
        .respond_with(Meanwhile::new(
            &app,
            "UPDATE happyview_records_v2 SET rev = '3recreated' WHERE rkey = 'g1'",
            1,
            record_not_found(),
        ))
        .mount(&app.mock_server)
        .await;

    let report = run(&app, &options(false)).await;
    assert_eq!(report.missing_total, 1);
    assert_eq!(report.deleted, 0);
    assert_eq!(report.changed_since_check, 1);
    assert_eq!(report.missing[0].outcome, "changed_since_check");
    assert_eq!(report.kept_by_hook, 0, "the hook never ran");
    assert!(
        stored(&app, ALICE, "g1").await,
        "the recreated record stays"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pause_stops_a_listing_between_pages() {
    let app = app().await;
    for rkey in ["g1", "g2"] {
        seed(&app, ALICE, rkey, &format!("bafy{rkey}")).await;
    }
    document(&app, ALICE).await;
    let (status, created) = post(&app, json!({ "collection": GAME, "dry_run": false }), None).await;
    assert_eq!(status, 202, "{created}");
    let job_id = created["job_id"].as_str().expect("a job id").to_string();
    let list = format!("{}/xrpc/com.atproto.repo.listRecords", pds(ALICE));
    Mock::given(method("GET"))
        .and(path(list.clone()))
        .and(query_param("cursor", "next"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(ALICE, &[], None)))
        .expect(0)
        .mount(&app.mock_server)
        .await;
    // The operator pauses while the first page is being read.
    Mock::given(method("GET"))
        .and(path(list))
        .respond_with(Meanwhile::new(
            &app,
            &format!("UPDATE happyview_jobs SET status = 'pausing' WHERE id = '{job_id}'"),
            1,
            ResponseTemplate::new(200).set_body_json(page(
                ALICE,
                &[("g2", "bafyg2")],
                Some("next"),
            )),
        ))
        .mount(&app.mock_server)
        .await;
    get_record(&app, ALICE, "g1", true).await;

    let report = as_the_jobs_run(
        &app,
        &job_id,
        reconcile::reconcile(&app.state, &options(false), Some(&job_id)),
    )
    .await
    .expect("reconcile runs");
    assert_eq!(report.stopped.as_deref(), Some("pausing"));
    assert!(report.dids_skipped.is_empty(), "{:?}", report.dids_skipped);
    assert_eq!(report.deleted, 0);
    assert!(stored(&app, ALICE, "g1").await);
    let job = app.get_json(&format!("/admin/jobs/{job_id}")).await;
    assert_eq!(job["progress"]["report"]["stopped"], "pausing", "{job}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancel_stops_inside_an_account_and_keeps_what_it_did() {
    let app = app().await;
    let rkeys: Vec<String> = (0..8).map(|i| format!("c{i}")).collect();
    for rkey in &rkeys {
        seed(&app, ALICE, rkey, &format!("bafy{rkey}")).await;
    }
    document(&app, ALICE).await;
    listing(
        &app,
        ALICE,
        &[("c5", "bafyc5"), ("c6", "bafyc6"), ("c7", "bafyc7")],
    )
    .await;
    let (status, created) = post(&app, json!({ "collection": GAME, "dry_run": false }), None).await;
    assert_eq!(status, 202, "{created}");
    let job_id = created["job_id"].as_str().expect("a job id").to_string();
    // The operator cancels while the second record is being checked.
    Mock::given(method("GET"))
        .and(path(get_record_path(ALICE)))
        .respond_with(Meanwhile::new(
            &app,
            &format!("UPDATE happyview_jobs SET status = 'cancelling' WHERE id = '{job_id}'"),
            2,
            record_not_found(),
        ))
        .mount(&app.mock_server)
        .await;

    let report = as_the_jobs_run(
        &app,
        &job_id,
        reconcile::reconcile(&app.state, &options(false), Some(&job_id)),
    )
    .await
    .expect("reconcile runs");
    assert_eq!(report.stopped.as_deref(), Some("cancelling"));
    assert_eq!(report.deleted, 2, "{:?}", report.missing);
    assert!(!stored(&app, ALICE, "c1").await);
    assert!(stored(&app, ALICE, "c2").await, "nothing after the cancel");

    let job = app.get_json(&format!("/admin/jobs/{job_id}")).await;
    assert_eq!(job["progress"]["report"]["deleted"], 2, "{job}");
    assert_eq!(
        job["progress"]["report"]["missing"][1]["uri"],
        uri(ALICE, "c1")
    );
}

#[tokio::test]
async fn a_listing_still_rate_limited_after_its_tries_skips_the_account() {
    let app = app().await;
    for rkey in ["g1", "g2"] {
        seed(&app, ALICE, rkey, &format!("bafy{rkey}")).await;
    }
    document(&app, ALICE).await;
    Mock::given(method("GET"))
        .and(path(format!(
            "{}/xrpc/com.atproto.repo.listRecords",
            pds(ALICE)
        )))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .expect(5)
        .mount(&app.mock_server)
        .await;
    get_record(&app, ALICE, "g1", true).await;

    let report = run(&app, &options(false)).await;
    assert_eq!(report.dids_skipped[0].reason, "rate_limited");
    assert_eq!(report.deleted, 0);
    assert!(stored(&app, ALICE, "g1").await);
}

#[tokio::test]
async fn a_collection_that_is_not_indexed_is_refused() {
    let app = TestApp::new().await;
    let (status, body) = app
        .post_json_status(
            "/admin/lexicons",
            json!({ "lexicon_json": fixtures::game_record_lexicon(), "backfill": false, "indexed": false }),
        )
        .await;
    assert!(status == 201 || status == 200, "{status}: {body}");

    let refused = reconcile::reconcile(&app.state, &options(false), None)
        .await
        .expect_err("refused");
    assert!(refused.contains("not an indexed"), "{refused}");
    let (status, body) = post(&app, json!({ "collection": GAME }), None).await;
    assert_eq!(status, 400, "{body}");

    let schemas = reconcile::refusal(&app.state, "com.atproto.lexicon.schema").expect("refused");
    assert!(schemas.contains("lexicon schemas"), "{schemas}");
}

#[tokio::test]
async fn a_delete_hook_that_fails_keeps_the_record() {
    if !common::echo_interpreter::is_built() {
        eprintln!("skipping: {}", common::echo_interpreter::BUILD);
        return;
    }
    let app = app().await;
    app.state
        .plugin_registry
        .register(common::echo_interpreter::plugin("lua"))
        .await;
    app.state
        .scripts
        .put_manual(
            &app.state.db,
            app.state.db_backend,
            happyview::catalog::ScriptWrite {
                id: format!("record.delete:{GAME}"),
                script_type: "lua".to_string(),
                body: "error:runtime".to_string(),
                description: None,
            },
        )
        .await
        .expect("install the delete hook");
    for rkey in ["g1", "g2"] {
        seed(&app, ALICE, rkey, &format!("bafy{rkey}")).await;
    }
    document(&app, ALICE).await;
    listing(&app, ALICE, &[("g2", "bafyg2")]).await;
    get_record(&app, ALICE, "g1", true).await;

    let report = run(&app, &options(false)).await;
    assert_eq!(report.hook_failed, 1);
    assert_eq!(report.deleted, 0);
    assert_eq!(report.missing[0].outcome, "hook_failed");
    assert!(stored(&app, ALICE, "g1").await, "a failed hook keeps it");
}

/// POSTs `body` to the reconcile route, as the admin or with `key`.
async fn post(app: &TestApp, body: Value, key: Option<&str>) -> (u16, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/admin/records/collection/reconcile")
        .header("content-type", "application/json");
    request = match key {
        Some(key) => request.header("authorization", format!("Bearer {key}")),
        None => {
            let (name, value) = app.admin_cookie();
            request.header(name, value)
        }
    };
    let response = app
        .router
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn the_route_enqueues_a_job_and_checks_what_it_is_asked() {
    let app = app().await;

    let (status, body) = post(&app, json!({ "collection": GAME }), None).await;
    assert_eq!(status, 202, "{body}");
    let id = body["job_id"].as_str().expect("a job id").to_string();
    let job = app.get_json(&format!("/admin/jobs/{id}")).await;
    assert_eq!(job["job_type"], reconcile::JOB_TYPE);
    assert_eq!(job["input"]["dry_run"], true, "a dry run unless asked");

    let (status, _) = post(&app, json!({ "collection": "app.test.unknown" }), None).await;
    assert_eq!(status, 400, "not a record collection");
    let (status, _) = post(&app, json!({ "collection": "" }), None).await;
    assert_eq!(status, 400);
    let (status, _) = post(
        &app,
        json!({ "collection": GAME, "pds_requests_per_second": 50 }),
        None,
    )
    .await;
    assert_eq!(status, 400, "over the rate cap");

    // A dry run needs records:read; an apply, records:delete.
    let (status, created) = app
        .post_json_status(
            "/admin/api-keys",
            json!({ "name": "reader", "permissions": ["records:read"] }),
        )
        .await;
    assert_eq!(status, 201, "{created}");
    let key = created["key"].as_str().expect("a key").to_string();
    let (status, body) = post(&app, json!({ "collection": GAME }), Some(&key)).await;
    assert_eq!(status, 202, "{body}");
    let (status, _) = post(
        &app,
        json!({ "collection": GAME, "dry_run": false }),
        Some(&key),
    )
    .await;
    assert_eq!(status, 403);
}
