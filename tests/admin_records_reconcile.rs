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
use tower::ServiceExt;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

use common::app::TestApp;
use common::fixtures;

const GAME: &str = "games.gamesgamesgamesgames.game";
const ALICE: &str = "did:plc:reconcilealice";

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
    assert_eq!(report.deleted, 0);
    assert_eq!(report.stale_cid_total, 1);
    assert_eq!(report.stale_cid[0].pds_cid, "bafynew");
    assert!(stored(&app, ALICE, "g1").await, "a dry run changes nothing");

    let report = run(&app, &options(false)).await;
    assert_eq!(report.deleted, 1);
    assert!(!stored(&app, ALICE, "g1").await);
    assert!(stored(&app, ALICE, "g2").await);
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
    for rkey in ["g1", "g2"] {
        seed(&app, ALICE, rkey, &format!("bafy{rkey}")).await;
    }
    document(&app, ALICE).await;
    listing(&app, ALICE, &[("g2", "bafyg2")]).await;
    get_record(&app, ALICE, "g1", true).await;

    let report = run(&app, &options(false)).await;
    assert_eq!(report.missing_total, 1);
    assert_eq!(report.kept_by_hook, 1);
    assert_eq!(report.deleted, 0);
    assert!(stored(&app, ALICE, "g1").await, "the hook kept it");
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
