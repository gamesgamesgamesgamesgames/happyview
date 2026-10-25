//! The backfill job system's database-backed tests. They need a host,
//! which the Jetstream source crate cannot build, so they stay here and
//! drive it through `AppHost`.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;
use uuid::Uuid;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;

use happyview_provider::{AdminCaller, AdminUser};
use happyview_source_jetstream::admin::*;
use happyview_source_jetstream::backfill::errors::*;
use happyview_source_jetstream::backfill::retry::*;
use happyview_source_jetstream::backfill::types::*;
use happyview_source_jetstream::backfill::*;

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339};
use crate::profile;
use crate::record_store::PreparedRecord;

use super::jobs;

/// Write one `listRecords` page through `record_store` on the backfill pool:
/// one transaction, tombstoned rows skipped, a repeated URI written once with
/// its last copy, refs for exactly the rows inserted or changed. Returns the
/// URIs written.
async fn write_records_page(
    state: &AppState,
    batch: &[PreparedRecord],
) -> Result<Vec<String>, sqlx::Error> {
    Ok(state
        .records
        .write_network(&state.backfill_db, batch)
        .await?
        .into_iter()
        .map(|changed| changed.uri)
        .collect())
}

use crate::test_support::{memory_pool, test_state_with_pool};
use wiremock::matchers::{method, path, path_regex, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::lexicon::{ParsedLexicon, ProcedureAction};

/// With automatic backfill off, nothing is queued: no job row, no task.
#[tokio::test]
async fn automatic_backfill_is_refused_when_off() {
    let mut state = crate::test_support::test_state_with_pool(
        crate::test_support::migrated_memory_pool().await,
    );
    state.config.auto_backfill = false;
    let started = state
        .start_automatic_backfill("com.example.thing".into(), "did:plc:dev")
        .await
        .unwrap();
    assert!(started.is_none());
    let (count,): (i64,) = crate::db::query_as("SELECT COUNT(*) FROM happyview_backfill_jobs")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

const POST: &str = "app.test.post";
const LIKE: &str = "app.test.like";

// -----------------------------------------------------------------------
// Account-targeted jobs
// -----------------------------------------------------------------------

async fn migrated_state() -> AppState {
    test_state_with_pool(crate::test_support::migrated_memory_pool().await)
}

#[tokio::test]
async fn a_gap_replay_job_row_is_created_with_its_floor() {
    // A restart's resume must never see the job without its floor, or
    // it would replay the whole archive from seq 0.
    let mut states = vec![migrated_state().await];
    if let Some(state) = crate::test_support::test_state_from_env().await {
        states.push(state);
    }
    for state in states {
        let job_id = create_gap_replay_job(&jobs(&state), 20)
            .await
            .expect("gap job");
        let row: (Option<i64>, String, String, Option<String>) = crate::db::query_as(&adapt_sql(
            "SELECT replay_after_seq, source, scope, collection FROM happyview_backfill_jobs WHERE id = ?",
            state.db_backend,
        ))
        .bind(&job_id)
        .fetch_one(&state.db)
        .await
        .expect("the job row");
        assert_eq!(
            row,
            (Some(20), "replay".to_string(), "network".to_string(), None)
        );
    }
}

#[tokio::test]
async fn replay_columns_and_tables_exist_on_both_backends() {
    let mut states = vec![migrated_state().await];
    if let Some(state) = crate::test_support::test_state_from_env().await {
        states.push(state);
    }
    for state in states {
        seed_job(
            &state,
            "replay-schema",
            None,
            "network",
            "pending",
            2,
            false,
        )
        .await;
        let (source, bytes, failed): (String, i64, i32) = crate::db::query_as(&adapt_sql(
            "SELECT source, replay_bytes, replay_blocks_failed FROM happyview_backfill_jobs WHERE id = ?",
            state.db_backend,
        ))
        .bind("replay-schema")
        .fetch_one(&state.db)
        .await
        .expect("replay columns");
        assert_eq!((source.as_str(), bytes, failed), ("crawl", 0, 0));

        crate::db::query(&adapt_sql(
            "INSERT INTO happyview_backfill_replay_sweeps \
             (job_id, sweep, kind, collections, dids, after_seq, cursor_seq) \
             VALUES (?, 0, 'commits', '[]', NULL, ?, ?)",
            state.db_backend,
        ))
        .bind("replay-schema")
        .bind(40_000_000_000_i64)
        .bind(40_000_000_000_i64)
        .execute(&state.db)
        .await
        .expect("a seq past 2^31 fits");
        crate::db::query(&adapt_sql(
            "INSERT INTO happyview_backfill_replay_retries (job_id, segment, block_index) VALUES (?, 'seg_0000000001.jss', -1)",
            state.db_backend,
        ))
        .bind("replay-schema")
        .execute(&state.db)
        .await
        .expect("retries table");
        crate::db::query(&adapt_sql(
            "DELETE FROM happyview_backfill_jobs WHERE id = ?",
            state.db_backend,
        ))
        .bind("replay-schema")
        .execute(&state.db)
        .await
        .expect("delete job");
        let left: i64 = crate::db::query_scalar(&adapt_sql(
            "SELECT COUNT(*) FROM happyview_backfill_replay_sweeps WHERE job_id = ?",
            state.db_backend,
        ))
        .bind("replay-schema")
        .fetch_one(&state.db)
        .await
        .expect("count");
        assert_eq!(left, 0, "sweeps cascade with their job");
    }
}

async fn seed_legacy_repo(
    state: &AppState,
    job_id: &str,
    did: &str,
    pds: Option<&str>,
    status: &str,
) {
    let sql = adapt_sql(
        "INSERT INTO happyview_backfill_repos (job_id, did, pds_endpoint, status) VALUES (?, ?, ?, ?)",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(job_id)
        .bind(did)
        .bind(pds)
        .bind(status)
        .execute(&state.db)
        .await
        .expect("seed legacy repo");
}

// -----------------------------------------------------------------------
// Bounded queue
// -----------------------------------------------------------------------

#[test]
fn the_discovery_window_defaults_and_rejects_nonsense() {
    assert_eq!(parse_discovery_window(None), DEFAULT_DISCOVERY_WINDOW);
    assert_eq!(parse_discovery_window(Some("0")), DEFAULT_DISCOVERY_WINDOW);
    assert_eq!(parse_discovery_window(Some("-5")), DEFAULT_DISCOVERY_WINDOW);
    assert_eq!(
        parse_discovery_window(Some("banana")),
        DEFAULT_DISCOVERY_WINDOW
    );
    assert_eq!(parse_discovery_window(Some("2000")), 2000);
}

#[test]
fn the_window_refuses_a_reservation_that_would_overflow_and_settles() {
    let window = QueueWindow::new(5, 2);
    assert!(window.try_reserve(3));
    assert!(!window.try_reserve(1), "full");
    window.settle(3, 1);
    assert_eq!(window.queued.load(Ordering::Acquire), 3);
    window.release(3);
    assert!(window.try_reserve(5));
}

#[allow(clippy::too_many_arguments)]
async fn seed_job(
    state: &AppState,
    id: &str,
    collection: Option<&str>,
    scope: &str,
    stage: &str,
    queue_version: i32,
    discovery_complete: bool,
) {
    let sql = adapt_sql(
        "INSERT INTO happyview_backfill_jobs \
         (id, collection, scope, status, stage, total_repos, queue_version, discovery_complete, created_at) \
         VALUES (?, ?, ?, 'running', ?, 0, ?, ?, '2026-01-01T00:00:00+00:00')",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(id)
        .bind(collection)
        .bind(scope)
        .bind(stage)
        .bind(queue_version)
        .bind(i32::from(discovery_complete))
        .execute(&state.db)
        .await
        .expect("seed job");
}

async fn seed_unit(state: &AppState, job_id: &str, collection: &str, did: &str, pds: Option<&str>) {
    let sql = adapt_sql(
        "INSERT INTO happyview_backfill_queue (job_id, collection, did, pds_endpoint) VALUES (?, ?, ?, ?)",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(job_id)
        .bind(collection)
        .bind(did)
        .bind(pds)
        .execute(&state.db)
        .await
        .expect("seed unit");
}

async fn count_for_job(state: &AppState, table: &str, job_id: &str) -> i64 {
    let sql = adapt_sql(
        &format!("SELECT COUNT(*) FROM {table} WHERE job_id = ?"),
        state.db_backend,
    );
    crate::db::query_as::<(i64,)>(&sql)
        .bind(job_id)
        .fetch_one(&state.db)
        .await
        .expect("count rows")
        .0
}

/// A job row's progress, with unset counters read as zero.
#[derive(Debug)]
struct JobRow {
    status: String,
    total_repos: i32,
    resolved_repos: i32,
    processed_repos: i32,
    total_records: i32,
    queue_version: i32,
    discovery_complete: i32,
}

async fn job_row(state: &AppState, job_id: &str) -> JobRow {
    let sql = adapt_sql(
        "SELECT status, total_repos, resolved_repos, processed_repos, total_records, queue_version, discovery_complete \
         FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    );
    #[allow(clippy::type_complexity)]
    let (status, total, resolved, processed, records, queue_version, discovery_complete): (
        String,
        Option<i32>,
        Option<i32>,
        Option<i32>,
        Option<i32>,
        i32,
        i32,
    ) = crate::db::query_as(&sql)
        .bind(job_id)
        .fetch_one(&state.db)
        .await
        .expect("job row");
    JobRow {
        status,
        total_repos: total.unwrap_or(0),
        resolved_repos: resolved.unwrap_or(0),
        processed_repos: processed.unwrap_or(0),
        total_records: records.unwrap_or(0),
        queue_version,
        discovery_complete,
    }
}

fn record_lexicon(nsid: &str) -> (serde_json::Value, ParsedLexicon) {
    let lexicon = serde_json::json!({
        "lexicon": 1,
        "id": nsid,
        "defs": {"main": {"type": "record", "key": "tid"}},
    });
    let parsed = ParsedLexicon::parse(
        lexicon.clone(),
        1,
        Some(nsid.to_string()),
        ProcedureAction::Upsert,
        None,
    )
    .expect("parse lexicon");
    (lexicon, parsed)
}

/// A migrated state whose relay, PLC and PDS are all `mock`, with
/// `collections` registered as record lexicons through the catalog.
async fn pipeline_state(mock: &MockServer, collections: &[&str]) -> AppState {
    let mut state = migrated_state().await;
    state.config.relay_url = mock.uri();
    state.config.plc_url = mock.uri();
    for nsid in collections {
        state
            .lexicons
            .put_manual(
                &state.db,
                state.db_backend,
                crate::catalog::LexiconWrite {
                    lexicon_json: record_lexicon(nsid).0,
                    backfill: true,
                    target_collection: Some(nsid.to_string()),
                    action: ProcedureAction::Upsert,
                    token_cost: None,
                    authority_did: None,
                    indexed: true,
                },
            )
            .await
            .expect("seed lexicon");
    }
    track_collections(&state).await;
    state
}

/// Tell the job system the catalog's record collections, as the admin
/// routes and project installs do: it backfills only what the host tracks.
async fn track_collections(state: &AppState) {
    state
        .collections_tx
        .send_replace(state.lexicons.get_record_collections().await);
}

fn did_doc(did: &str, pds: &str) -> serde_json::Value {
    serde_json::json!({
        "id": did,
        "service": [{"id": "#atproto_pds", "type": "AtprotoPersonalDataServer", "serviceEndpoint": pds}],
    })
}

async fn mount_relay_page(
    mock: &MockServer,
    collection: &str,
    cursor: Option<&str>,
    dids: &[&str],
    next: Option<&str>,
) {
    let builder = Mock::given(method("GET"))
        .and(path("/xrpc/com.atproto.sync.listReposByCollection"))
        .and(query_param("collection", collection));
    let builder = match cursor {
        Some(c) => builder.and(query_param("cursor", c)),
        None => builder.and(query_param_is_missing("cursor")),
    };
    let repos: Vec<serde_json::Value> =
        dids.iter().map(|d| serde_json::json!({"did": d})).collect();
    builder
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"repos": repos, "cursor": next})),
        )
        .mount(mock)
        .await;
}

/// Every DID resolves to a document naming `mock` as its PDS.
async fn mount_plc(mock: &MockServer) {
    let pds = mock.uri();
    Mock::given(method("GET"))
        .and(path_regex(r"^/did:plc:[a-z0-9]+$"))
        .respond_with(move |req: &wiremock::Request| {
            let did = req.url.path().trim_start_matches('/').to_string();
            ResponseTemplate::new(200).set_body_json(did_doc(&did, &pds))
        })
        .mount(mock)
        .await;
}

/// `count` records for `(did, collection)`, each with a CID that verifies,
/// fetched exactly once.
async fn mount_records(mock: &MockServer, did: &str, collection: &str, count: usize) {
    let records: Vec<serde_json::Value> = (0..count)
        .map(|i| {
            let value = serde_json::json!({"$type": collection, "text": format!("{did} {i}")});
            let cid = crate::cid_verify::compute_record_cid(&value)
                .expect("cid")
                .to_string();
            serde_json::json!({"uri": format!("at://{did}/{collection}/r{i}"), "cid": cid, "value": value})
        })
        .collect();
    Mock::given(method("GET"))
        .and(path("/xrpc/com.atproto.repo.listRecords"))
        .and(query_param("repo", did))
        .and(query_param("collection", collection))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"records": records})),
        )
        .expect(1)
        .mount(mock)
        .await;
}

/// Fails the test, when the mock server drops, if any `listRecords` call
/// reached no other mock.
async fn forbid_other_record_fetches(mock: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/xrpc/com.atproto.repo.listRecords"))
        .respond_with(ResponseTemplate::new(500))
        .with_priority(10)
        .expect(0)
        .mount(mock)
        .await;
}

async fn run_to_end(state: &AppState, job_id: &str, window: i64) {
    tokio::time::timeout(
        Duration::from_secs(60),
        run_backfill_job_with(jobs(state), job_id.to_string(), window),
    )
    .await
    .expect("the backfill job should finish rather than hang");
}

fn bounded_queue(job_id: &str) -> JobQueue {
    JobQueue {
        job_id: Arc::new(job_id.to_string()),
        version: QueueVersion::Bounded,
        window: Arc::new(QueueWindow::new(DEFAULT_DISCOVERY_WINDOW, 0)),
        discovery_done: Arc::new(AtomicBool::new(true)),
        completion_failures: Arc::new(AtomicU32::new(0)),
    }
}

#[tokio::test]
async fn the_resolver_reads_a_legacy_job_a_page_at_a_time() {
    let state = migrated_state().await;
    seed_job(
        &state,
        "page-job",
        None,
        "network",
        "resolving_and_fetching",
        1,
        true,
    )
    .await;
    let mut expected = Vec::new();
    for i in 0..2500 {
        let did = format!("did:plc:p{i:05}");
        let resolved = i % 10 == 0;
        seed_legacy_repo(
            &state,
            "page-job",
            &did,
            resolved.then_some("https://pds.test"),
            "pending",
        )
        .await;
        if !resolved {
            expected.push(WorkUnit::all_collections(did));
        }
    }
    assert_eq!(
        read_all_unresolved(&state, &JobQueue::legacy("page-job")).await,
        expected
    );
}

#[tokio::test]
async fn the_resolver_reads_a_bounded_queue_in_collection_then_did_order() {
    let state = migrated_state().await;
    seed_job(
        &state,
        "page-bounded",
        None,
        "network",
        "resolving_and_fetching",
        2,
        true,
    )
    .await;
    let mut expected = Vec::new();
    // LIKE sorts before POST; 1200 units cross the 1000-unit page inside
    // the second collection.
    for collection in [LIKE, POST] {
        for i in 0..600 {
            let did = format!("did:plc:b{i:04}");
            let resolved = i % 7 == 0;
            seed_unit(
                &state,
                "page-bounded",
                collection,
                &did,
                resolved.then_some("https://pds.test"),
            )
            .await;
            if !resolved {
                expected.push(WorkUnit {
                    did,
                    collection: collection.to_string(),
                });
            }
        }
    }
    assert_eq!(
        read_all_unresolved(&state, &bounded_queue("page-bounded")).await,
        expected
    );
}

async fn read_all_unresolved(state: &AppState, queue: &JobQueue) -> Vec<WorkUnit> {
    let mut seen = Vec::new();
    let mut after: Option<WorkUnit> = None;
    loop {
        let page = unresolved_page(&jobs(state), queue, after.as_ref())
            .await
            .expect("page");
        assert!(page.len() as i64 <= RESOLVE_PAGE_SIZE);
        let Some(last) = page.last().cloned() else {
            return seen;
        };
        after = Some(last);
        seen.extend(page);
    }
}

#[tokio::test]
async fn the_backlog_of_resolved_units_is_read_a_page_at_a_time() {
    let state = migrated_state().await;
    seed_job(
        &state,
        "backlog",
        None,
        "network",
        "resolving_and_fetching",
        2,
        true,
    )
    .await;
    let mut expected = Vec::new();
    for i in 0..1500 {
        let did = format!("did:plc:r{i:04}");
        let resolved = i % 3 != 0;
        seed_unit(
            &state,
            "backlog",
            POST,
            &did,
            resolved.then_some("https://pds.test"),
        )
        .await;
        if resolved {
            expected.push((
                WorkUnit {
                    did,
                    collection: POST.to_string(),
                },
                "https://pds.test".to_string(),
            ));
        }
    }
    let queue = bounded_queue("backlog");
    let mut seen = Vec::new();
    let mut after: Option<WorkUnit> = None;
    loop {
        let page = resolved_page(&jobs(&state), &queue, after.as_ref())
            .await
            .expect("page");
        assert!(page.len() as i64 <= RESOLVE_PAGE_SIZE);
        let Some((last, _)) = page.last().cloned() else {
            break;
        };
        after = Some(last);
        seen.extend(page);
    }
    assert_eq!(seen, expected);
}

#[tokio::test]
async fn a_network_job_fetches_each_repo_only_for_the_collection_it_was_found_under() {
    let mock = MockServer::start().await;
    let state = pipeline_state(&mock, &[POST, LIKE]).await;
    mount_relay_page(&mock, POST, None, &["did:plc:a", "did:plc:b"], None).await;
    mount_relay_page(&mock, LIKE, None, &["did:plc:a"], None).await;
    mount_plc(&mock).await;
    mount_records(&mock, "did:plc:a", POST, 1).await;
    mount_records(&mock, "did:plc:b", POST, 2).await;
    mount_records(&mock, "did:plc:a", LIKE, 1).await;
    // `did:plc:b` was never listed under LIKE, so it is never fetched for it.
    forbid_other_record_fetches(&mock).await;

    let job_id = create_backfill_job(&jobs(&state), None, &[])
        .await
        .expect("create job");
    run_to_end(&state, &job_id, DEFAULT_DISCOVERY_WINDOW).await;

    let job = job_row(&state, &job_id).await;
    assert_eq!(job.status, "completed");
    assert_eq!(
        (
            job.total_repos,
            job.resolved_repos,
            job.processed_repos,
            job.total_records
        ),
        (3, 3, 3, 4)
    );
    assert_eq!((job.queue_version, job.discovery_complete), (2, 1));
    assert_eq!(
        count_for_job(&state, "happyview_backfill_queue", &job_id).await,
        0,
        "completed units are deleted"
    );
    assert_eq!(
        count_for_job(&state, "happyview_backfill_completions", &job_id).await,
        3
    );
    let (repos, completed, stats_records): (i64, i64, i64) = crate::db::query_as(
        "SELECT repos, completed_repos, records FROM happyview_backfill_pds_stats WHERE job_id = ? AND pds_endpoint = ?",
    )
    .bind(&job_id)
    .bind(mock.uri())
    .fetch_one(&state.db)
    .await
    .expect("pds stats");
    assert_eq!((repos, completed, stats_records), (3, 3, 4));
    let lookups = mock
        .received_requests()
        .await
        .expect("requests are recorded")
        .iter()
        .filter(|r| r.url.path() == "/did:plc:a")
        .count();
    assert_eq!(
        lookups, 1,
        "a DID queued under two collections is resolved once"
    );
}

/// The all-collections job takes its targets from the catalog, so a record
/// lexicon only a project file defines is backfilled like any other.
#[tokio::test]
async fn an_all_collections_job_includes_a_project_only_record_lexicon() {
    let mock = MockServer::start().await;
    let state = pipeline_state(&mock, &[]).await;
    let mut snap = crate::project::ProjectSnapshot::empty();
    snap.lexicons.insert(
        POST.to_string(),
        crate::project::ProjectLexicon {
            parsed: record_lexicon(POST).1,
            path: "src/lexicons/post.json".into(),
            backfill: false,
            indexed: true,
        },
    );
    state.project.store(Arc::new(snap));
    track_collections(&state).await;
    mount_relay_page(&mock, POST, None, &["did:plc:a"], None).await;
    mount_plc(&mock).await;
    mount_records(&mock, "did:plc:a", POST, 1).await;
    forbid_other_record_fetches(&mock).await;

    let job_id = create_backfill_job(&jobs(&state), None, &[])
        .await
        .expect("create job");
    run_to_end(&state, &job_id, DEFAULT_DISCOVERY_WINDOW).await;

    let job = job_row(&state, &job_id).await;
    assert_eq!((job.status.as_str(), job.total_records), ("completed", 1));
}

#[tokio::test]
async fn discovery_waits_for_the_window_before_the_next_relay_page() {
    let mock = MockServer::start().await;
    let state = pipeline_state(&mock, &[POST]).await;
    mount_relay_page(&mock, POST, None, &["did:plc:a", "did:plc:b"], Some("c1")).await;
    mount_relay_page(
        &mock,
        POST,
        Some("c1"),
        &["did:plc:c", "did:plc:d"],
        Some("c2"),
    )
    .await;
    mount_relay_page(&mock, POST, Some("c2"), &["did:plc:e"], None).await;
    mount_plc(&mock).await;
    for did in [
        "did:plc:a",
        "did:plc:b",
        "did:plc:c",
        "did:plc:d",
        "did:plc:e",
    ] {
        mount_records(&mock, did, POST, 1).await;
    }
    forbid_other_record_fetches(&mock).await;

    let job_id = create_backfill_job(&jobs(&state), Some(POST), &[])
        .await
        .expect("create job");
    run_to_end(&state, &job_id, 2).await;

    let requests = mock
        .received_requests()
        .await
        .expect("requests are recorded");
    let is_relay =
        |r: &&wiremock::Request| r.url.path() == "/xrpc/com.atproto.sync.listReposByCollection";
    assert!(
        requests
            .iter()
            .filter(is_relay)
            .all(|r| r.url.query_pairs().any(|(k, v)| k == "limit" && v == "2")),
        "a relay page must fit in the window"
    );
    let first_fetch = requests
        .iter()
        .position(|r| r.url.path() == "/xrpc/com.atproto.repo.listRecords")
        .expect("records were fetched");
    let second_page = requests
        .iter()
        .position(|r| r.url.query_pairs().any(|(k, v)| k == "cursor" && v == "c1"))
        .expect("the second relay page was requested");
    assert!(
        second_page > first_fetch,
        "the second relay page must wait until a unit from the first completes"
    );
    let job = job_row(&state, &job_id).await;
    assert_eq!(
        (job.status.as_str(), job.total_repos, job.processed_repos),
        ("completed", 5, 5)
    );
    assert_eq!(
        count_for_job(&state, "happyview_backfill_queue", &job_id).await,
        0
    );
}

#[tokio::test]
async fn a_resumed_discovery_continues_from_its_saved_cursor() {
    let mock = MockServer::start().await;
    let state = pipeline_state(&mock, &[POST]).await;
    seed_job(
        &state,
        "resume-cursor",
        Some(POST),
        "network",
        "resolving_and_fetching",
        2,
        false,
    )
    .await;
    crate::db::query(
        "INSERT INTO happyview_backfill_cursors (job_id, collection, relay_cursor, done) VALUES ('resume-cursor', ?, 'c1', 0)",
    )
    .bind(POST)
    .execute(&state.db)
    .await
    .expect("seed cursor");
    Mock::given(method("GET"))
        .and(path("/xrpc/com.atproto.sync.listReposByCollection"))
        .and(query_param_is_missing("cursor"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&mock)
        .await;
    mount_relay_page(&mock, POST, Some("c1"), &["did:plc:late"], None).await;
    mount_plc(&mock).await;
    mount_records(&mock, "did:plc:late", POST, 1).await;
    forbid_other_record_fetches(&mock).await;

    run_to_end(&state, "resume-cursor", DEFAULT_DISCOVERY_WINDOW).await;

    let job = job_row(&state, "resume-cursor").await;
    assert_eq!(
        (
            job.status.as_str(),
            job.processed_repos,
            job.discovery_complete
        ),
        ("completed", 1, 1)
    );
    let (done,): (i32,) = crate::db::query_as(
        "SELECT done FROM happyview_backfill_cursors WHERE job_id = 'resume-cursor'",
    )
    .fetch_one(&state.db)
    .await
    .expect("cursor row");
    assert_eq!(done, 1);
}

/// Review focus 1: deferred resolutions occupy the window. If only the
/// final pass retried them, discovery would wait for room forever.
#[tokio::test]
async fn a_window_full_of_deferred_resolutions_still_drains() {
    let mock = MockServer::start().await;
    let state = pipeline_state(&mock, &[POST]).await;
    mount_relay_page(&mock, POST, None, &["did:plc:slow"], Some("c1")).await;
    mount_relay_page(&mock, POST, Some("c1"), &["did:plc:next"], None).await;
    // The first lookup of `slow` fails retryably, so it sits deferred in
    // a window of one while discovery waits for room.
    Mock::given(method("GET"))
        .and(path("/did:plc:slow"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&mock)
        .await;
    mount_plc(&mock).await;
    mount_records(&mock, "did:plc:slow", POST, 1).await;
    mount_records(&mock, "did:plc:next", POST, 1).await;
    forbid_other_record_fetches(&mock).await;

    let job_id = create_backfill_job(&jobs(&state), Some(POST), &[])
        .await
        .expect("create job");
    run_to_end(&state, &job_id, 1).await;

    let job = job_row(&state, &job_id).await;
    assert_eq!((job.status.as_str(), job.processed_repos), ("completed", 2));
}

/// One PDS, more units than its worker's channel holds, and slow fetches.
/// The units its worker cannot take yet wait in the dispatcher. Once the
/// window is full nothing new arrives to prompt a retry, so the waiting
/// units must be retried on their own or the job stalls.
#[tokio::test]
async fn units_waiting_on_a_busy_pds_worker_are_dispatched_without_new_arrivals() {
    let mock = MockServer::start().await;
    let state = pipeline_state(&mock, &[POST]).await;
    let dids: Vec<String> = (0..151).map(|i| format!("did:plc:u{i:04}")).collect();
    let dids: Vec<&str> = dids.iter().map(String::as_str).collect();
    mount_relay_page(&mock, POST, None, &dids[..150], Some("c1")).await;
    mount_relay_page(&mock, POST, Some("c1"), &dids[150..], None).await;
    mount_plc(&mock).await;
    Mock::given(method("GET"))
        .and(path("/xrpc/com.atproto.repo.listRecords"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"records": []}))
                .set_delay(Duration::from_millis(30)),
        )
        .expect(151)
        .mount(&mock)
        .await;

    let job_id = create_backfill_job(&jobs(&state), Some(POST), &[])
        .await
        .expect("create job");
    run_to_end(&state, &job_id, 150).await;

    let job = job_row(&state, &job_id).await;
    assert_eq!(
        (job.status.as_str(), job.processed_repos),
        ("completed", 151)
    );
    assert_eq!(
        count_for_job(&state, "happyview_backfill_queue", &job_id).await,
        0
    );
}

/// Review focus 2: `did:plc:a` is enqueued while the resolver is past it
/// (on `did:plc:m`), and discovery finishes before that pass ends. The
/// pass began before discovery finished, so another pass must follow.
#[tokio::test]
async fn a_unit_enqueued_behind_the_resolver_while_discovery_finishes_is_still_resolved() {
    let mock = MockServer::start().await;
    let state = pipeline_state(&mock, &[POST]).await;
    let pds = mock.uri();
    mount_relay_page(&mock, POST, None, &["did:plc:m"], Some("c1")).await;
    Mock::given(method("GET"))
        .and(path("/xrpc/com.atproto.sync.listReposByCollection"))
        .and(query_param("cursor", "c1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"repos": [{"did": "did:plc:a"}]}))
                .set_delay(Duration::from_millis(300)),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/did:plc:m"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(did_doc("did:plc:m", &pds))
                .set_delay(Duration::from_millis(900)),
        )
        .with_priority(1)
        .mount(&mock)
        .await;
    mount_plc(&mock).await;
    mount_records(&mock, "did:plc:m", POST, 1).await;
    mount_records(&mock, "did:plc:a", POST, 1).await;
    forbid_other_record_fetches(&mock).await;

    let job_id = create_backfill_job(&jobs(&state), Some(POST), &[])
        .await
        .expect("create job");
    run_to_end(&state, &job_id, DEFAULT_DISCOVERY_WINDOW).await;

    let job = job_row(&state, &job_id).await;
    assert_eq!(
        job.processed_repos, 2,
        "the unit behind the cursor must be fetched too"
    );
    assert_eq!(
        count_for_job(&state, "happyview_backfill_queue", &job_id).await,
        0
    );
}

/// A discovery that cannot write its queue fails the job; it never lets
/// the job finish as completed on whatever had been queued.
#[tokio::test]
async fn a_discovery_that_cannot_write_its_queue_fails_the_job() {
    let mock = MockServer::start().await;
    let state = pipeline_state(&mock, &[POST]).await;
    crate::db::query(
        "CREATE TRIGGER refuse_cursors BEFORE INSERT ON happyview_backfill_cursors \
         BEGIN SELECT RAISE(ABORT, 'cursor writes refused'); END",
    )
    .execute(&state.db)
    .await
    .expect("create trigger");
    mount_relay_page(&mock, POST, None, &["did:plc:a"], None).await;
    mount_plc(&mock).await;
    forbid_other_record_fetches(&mock).await;

    let job_id = create_backfill_job(&jobs(&state), Some(POST), &[])
        .await
        .expect("create job");
    run_to_end(&state, &job_id, DEFAULT_DISCOVERY_WINDOW).await;

    let (status, error): (String, Option<String>) =
        crate::db::query_as("SELECT status, error FROM happyview_backfill_jobs WHERE id = ?")
            .bind(&job_id)
            .fetch_one(&state.db)
            .await
            .expect("job row");
    assert_eq!(status, "failed");
    let error = error.expect("a failed job says why");
    assert!(error.contains("cursor writes refused"), "{error}");
}

/// A collection with a record lexicon the catalog does not index is not
/// tracked, so a job naming it fails and says so.
#[tokio::test]
async fn a_job_for_an_unindexed_collection_fails_naming_why() {
    let mock = MockServer::start().await;
    let state = pipeline_state(&mock, &[]).await;
    state
        .lexicons
        .upsert_unindexed(record_lexicon(POST).1)
        .await;
    track_collections(&state).await;
    forbid_other_record_fetches(&mock).await;

    let job_id = create_backfill_job(&jobs(&state), Some(POST), &[])
        .await
        .expect("create job");
    run_to_end(&state, &job_id, DEFAULT_DISCOVERY_WINDOW).await;

    let (status, error): (String, Option<String>) =
        crate::db::query_as("SELECT status, error FROM happyview_backfill_jobs WHERE id = ?")
            .bind(&job_id)
            .fetch_one(&state.db)
            .await
            .expect("job row");
    assert_eq!(status, "failed");
    assert_eq!(
        error.as_deref(),
        Some(format!("no indexed record lexicon for collection '{POST}'").as_str())
    );
}

/// The same for a discovery that cannot read where it left off.
#[tokio::test]
async fn a_discovery_that_cannot_read_its_cursors_fails_the_job() {
    let mock = MockServer::start().await;
    let state = pipeline_state(&mock, &[POST]).await;
    crate::db::query("DROP TABLE happyview_backfill_cursors")
        .execute(&state.db)
        .await
        .expect("drop cursors");
    forbid_other_record_fetches(&mock).await;

    let job_id = create_backfill_job(&jobs(&state), Some(POST), &[])
        .await
        .expect("create job");
    run_to_end(&state, &job_id, DEFAULT_DISCOVERY_WINDOW).await;

    let job = job_row(&state, &job_id).await;
    assert_eq!((job.status.as_str(), job.discovery_complete), ("failed", 0));
}

/// A unit resolved from the DID cache sends no request, so it must leave
/// the PLC directory's rate-limit cooldown exactly as it found it.
#[tokio::test]
async fn a_cache_hit_leaves_the_plc_cooldown_in_force() {
    let mock = MockServer::start().await;
    let state = pipeline_state(&mock, &[POST, LIKE]).await;
    let reset = chrono::Utc::now().timestamp() + 60;
    Mock::given(method("GET"))
        .and(path("/did:plc:y"))
        .respond_with(
            ResponseTemplate::new(429).insert_header("ratelimit-reset", reset.to_string()),
        )
        .mount(&mock)
        .await;
    seed_job(
        &state,
        "cooldown",
        None,
        "network",
        "resolving_and_fetching",
        2,
        true,
    )
    .await;
    for (collection, did) in [
        (POST, "did:plc:x"),
        (LIKE, "did:plc:x"),
        (POST, "did:plc:y"),
    ] {
        seed_unit(&state, "cooldown", collection, did, None).await;
    }
    let (tx, _rx) = mpsc::channel(16);
    let ctx = ResolverContext {
        state: jobs(&state),
        queue: bounded_queue("cooldown"),
        resolved: Arc::new(AtomicI32::new(0)),
        cancelled: Arc::new(AtomicBool::new(false)),
        recorder: Arc::new(
            happyview_source_jetstream::backfill::errors::ErrorRecorder::new(
                &jobs(&state),
                "cooldown",
            )
            .await,
        ),
        concurrency: 1,
        tx,
    };
    let mut run = ResolverRun {
        cooldowns: HostCooldowns::new(),
        deferred: DeferredQueue::new(),
        deferred_units: HashSet::new(),
        recorded: HashSet::new(),
        resolved_dids: ResolvedDids::default(),
        max_attempts: 3,
        attempted: 0,
        next_cancel_check: i32::MAX,
    };
    run.resolved_dids.insert("did:plc:x", &mock.uri());
    let host = profile::did_doc_host(&state.config.plc_url, "did:plc:y");

    let limited =
        profile::resolve_pds_endpoint_once(&state.http, &state.config.plc_url, "did:plc:y").await;
    let y = WorkUnit {
        did: "did:plc:y".to_string(),
        collection: POST.to_string(),
    };
    assert!(handle_resolution(&ctx, &mut run, y, limited, 1).await);
    let gate = run
        .cooldowns
        .eligible_at(&host)
        .expect("PLC is cooling down");
    assert!(gate > std::time::Instant::now() + Duration::from_secs(30));

    // A cache hit while scanning a page...
    let x_post = WorkUnit {
        did: "did:plc:x".to_string(),
        collection: POST.to_string(),
    };
    assert!(on_cached(&ctx, &mut run, x_post, mock.uri()).await);
    // ...and one while draining deferred retries.
    let x_like = WorkUnit {
        did: "did:plc:x".to_string(),
        collection: LIKE.to_string(),
    };
    let item = DeferredItem {
        payload: x_like,
        host: host.clone(),
        attempts: 1,
        eligible_at: std::time::Instant::now(),
    };
    assert!(retry_deferred_resolution(&ctx, &mut run, item).await);

    assert_eq!(run.cooldowns.eligible_at(&host), Some(gate));
    assert_eq!(run.cooldowns.consecutive_failures(&host), 1);
    assert_eq!(ctx.resolved.load(Ordering::Relaxed), 2);
}

/// A unit whose completion could not be committed is still queued when the
/// run ends. The job is paused with that work left, never completed.
#[tokio::test]
async fn a_run_that_leaves_units_queued_pauses_rather_than_completes() {
    let mock = MockServer::start().await;
    let state = pipeline_state(&mock, &[POST]).await;
    crate::db::query(
        "CREATE TRIGGER refuse_unit_deletes BEFORE DELETE ON happyview_backfill_queue \
         BEGIN SELECT RAISE(ABORT, 'unit deletes refused'); END",
    )
    .execute(&state.db)
    .await
    .expect("create trigger");
    mount_plc(&mock).await;
    mount_records(&mock, "did:plc:stuck", POST, 1).await;
    forbid_other_record_fetches(&mock).await;

    let job_id = create_backfill_job(&jobs(&state), Some(POST), &["did:plc:stuck".to_string()])
        .await
        .expect("create job");
    run_to_end(&state, &job_id, DEFAULT_DISCOVERY_WINDOW).await;

    let job = job_row(&state, &job_id).await;
    assert_eq!(job.status, "paused");
    assert_eq!(
        count_for_job(&state, "happyview_backfill_queue", &job_id).await,
        1
    );
    let (error,): (Option<String>,) =
        crate::db::query_as("SELECT error FROM happyview_backfill_jobs WHERE id = ?")
            .bind(&job_id)
            .fetch_one(&state.db)
            .await
            .expect("job error");
    assert_eq!(
        error.as_deref(),
        Some("paused: 1 units could not be completed; resume to retry")
    );
    let (logged,): (i64,) = crate::db::query_as(
        "SELECT COUNT(*) FROM happyview_event_logs WHERE event_type = 'backfill.units_left' AND subject = ?",
    )
    .bind(POST)
    .fetch_one(&state.db)
    .await
    .expect("count events");
    assert_eq!(logged, 1);
}

/// A relay rate limit can ask for a two-minute wait; a pause must not have
/// to sit it out.
#[tokio::test]
async fn a_pause_interrupts_a_relay_rate_limit_wait() {
    let mock = MockServer::start().await;
    let state = pipeline_state(&mock, &[POST]).await;
    Mock::given(method("GET"))
        .and(path("/xrpc/com.atproto.sync.listReposByCollection"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "120"))
        .mount(&mock)
        .await;

    let job_id = create_backfill_job(&jobs(&state), Some(POST), &[])
        .await
        .expect("create job");
    let pauser = {
        let state = state.clone();
        let job_id = job_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            request_pause(&jobs(&state), &job_id).await;
        })
    };
    tokio::time::timeout(
        Duration::from_secs(20),
        run_backfill_job_with(jobs(&state), job_id.clone(), DEFAULT_DISCOVERY_WINDOW),
    )
    .await
    .expect("the pause should not wait out the rate limit");
    pauser.await.expect("pauser");

    let job = job_row(&state, &job_id).await;
    assert_eq!((job.status.as_str(), job.discovery_complete), ("paused", 0));
}

#[tokio::test]
async fn an_account_job_fetches_every_collection_for_its_account() {
    let mock = MockServer::start().await;
    let state = pipeline_state(&mock, &[POST, LIKE]).await;
    mount_plc(&mock).await;
    mount_records(&mock, "did:plc:t1", POST, 1).await;
    mount_records(&mock, "did:plc:t1", LIKE, 2).await;
    forbid_other_record_fetches(&mock).await;

    let job_id = create_backfill_job(&jobs(&state), None, &["did:plc:t1".to_string()])
        .await
        .expect("create job");
    run_to_end(&state, &job_id, DEFAULT_DISCOVERY_WINDOW).await;

    let job = job_row(&state, &job_id).await;
    assert_eq!(
        (
            job.status.as_str(),
            job.total_repos,
            job.processed_repos,
            job.total_records
        ),
        ("completed", 1, 1, 3)
    );
    assert_eq!(
        count_for_job(&state, "happyview_backfill_queue", &job_id).await,
        0
    );
    let (collection,): (String,) = crate::db::query_as(
        "SELECT collection FROM happyview_backfill_completions WHERE job_id = ?",
    )
    .bind(&job_id)
    .fetch_one(&state.db)
    .await
    .expect("completion row");
    assert_eq!(collection, "");
}

#[tokio::test]
async fn a_pre_upgrade_job_past_discovery_finishes_on_its_existing_rows() {
    let mock = MockServer::start().await;
    let state = pipeline_state(&mock, &[POST]).await;
    let pds = mock.uri();
    seed_job(
        &state,
        "legacy-fetching",
        Some(POST),
        "network",
        "resolving_and_fetching",
        1,
        true,
    )
    .await;
    crate::db::query(
        "UPDATE happyview_backfill_jobs SET total_records = 5 WHERE id = 'legacy-fetching'",
    )
    .execute(&state.db)
    .await
    .expect("seed records");
    seed_legacy_repo(&state, "legacy-fetching", "did:plc:x", None, "pending").await;
    seed_legacy_repo(
        &state,
        "legacy-fetching",
        "did:plc:y",
        Some(&pds),
        "pending",
    )
    .await;
    seed_legacy_repo(
        &state,
        "legacy-fetching",
        "did:plc:z",
        Some(&pds),
        "completed",
    )
    .await;
    mount_plc(&mock).await;
    mount_records(&mock, "did:plc:x", POST, 1).await;
    mount_records(&mock, "did:plc:y", POST, 1).await;
    forbid_other_record_fetches(&mock).await;

    run_to_end(&state, "legacy-fetching", DEFAULT_DISCOVERY_WINDOW).await;

    let rows: Vec<(String, String, i32)> = crate::db::query_as(
        "SELECT did, status, records_fetched FROM happyview_backfill_repos WHERE job_id = 'legacy-fetching' ORDER BY did",
    )
    .fetch_all(&state.db)
    .await
    .expect("legacy rows");
    assert_eq!(
        rows,
        vec![
            ("did:plc:x".to_string(), "completed".to_string(), 1),
            ("did:plc:y".to_string(), "completed".to_string(), 1),
            ("did:plc:z".to_string(), "completed".to_string(), 0),
        ]
    );
    let job = job_row(&state, "legacy-fetching").await;
    assert_eq!(
        (
            job.status.as_str(),
            job.resolved_repos,
            job.processed_repos,
            job.total_records,
            job.queue_version
        ),
        ("completed", 3, 3, 7, 1)
    );
    assert_eq!(
        count_for_job(&state, "happyview_backfill_queue", "legacy-fetching").await,
        0
    );
}

/// Several repos on one PDS fetch concurrently. Each fetch writes its
/// pages in transactions, so a fetch the job stopped polling mid-page
/// would hold the (here, only) connection while the job waited on it.
#[tokio::test]
async fn a_pre_upgrade_job_in_the_fetching_stage_finishes_its_rows() {
    let mock = MockServer::start().await;
    let state = pipeline_state(&mock, &[POST]).await;
    let pds = mock.uri();
    seed_job(
        &state,
        "legacy-fetch-stage",
        Some(POST),
        "network",
        "fetching_records",
        1,
        true,
    )
    .await;
    for did in ["did:plc:f1", "did:plc:f2", "did:plc:f3"] {
        seed_legacy_repo(&state, "legacy-fetch-stage", did, Some(&pds), "pending").await;
        mount_records(&mock, did, POST, 2).await;
    }
    forbid_other_record_fetches(&mock).await;

    run_to_end(&state, "legacy-fetch-stage", DEFAULT_DISCOVERY_WINDOW).await;

    let job = job_row(&state, "legacy-fetch-stage").await;
    assert_eq!(
        (job.status.as_str(), job.processed_repos, job.total_records),
        ("completed", 3, 6)
    );
    let (completed,): (i64,) = crate::db::query_as(
        "SELECT COUNT(*) FROM happyview_backfill_repos WHERE job_id = 'legacy-fetch-stage' AND status = 'completed'",
    )
    .fetch_one(&state.db)
    .await
    .expect("count completed");
    assert_eq!(completed, 3);
}

async fn assert_a_discovering_legacy_job_restarts(
    state: &AppState,
    mock: &MockServer,
    job_id: &str,
    [p, q, r]: [&str; 3],
) {
    seed_job(
        state,
        job_id,
        Some(POST),
        "network",
        "discovering_repos",
        1,
        true,
    )
    .await;
    seed_legacy_repo(state, job_id, p, None, "pending").await;
    seed_legacy_repo(state, job_id, q, None, "pending").await;
    mount_relay_page(mock, POST, None, &[q, r], None).await;
    mount_plc(mock).await;
    mount_records(mock, q, POST, 1).await;
    mount_records(mock, r, POST, 1).await;
    // `p` was only in the discarded partial list.
    forbid_other_record_fetches(mock).await;

    run_to_end(state, job_id, DEFAULT_DISCOVERY_WINDOW).await;

    assert_eq!(
        count_for_job(state, "happyview_backfill_repos", job_id).await,
        0
    );
    let job = job_row(state, job_id).await;
    assert_eq!(
        (
            job.status.as_str(),
            job.total_repos,
            job.processed_repos,
            job.queue_version,
            job.discovery_complete
        ),
        ("completed", 2, 2, 2, 1)
    );
    assert_eq!(
        count_for_job(state, "happyview_backfill_queue", job_id).await,
        0
    );
}

#[tokio::test]
async fn a_pre_upgrade_job_still_discovering_restarts_on_the_bounded_queue() {
    let mock = MockServer::start().await;
    let state = pipeline_state(&mock, &[POST]).await;
    assert_a_discovering_legacy_job_restarts(
        &state,
        &mock,
        "legacy-discovering",
        ["did:plc:p", "did:plc:q", "did:plc:r"],
    )
    .await;
}

/// Completes `RECENT_COMPLETIONS_PER_JOB + COMPLETIONS_TRIM_EVERY + 5`
/// units. Completing a unit never trims (R10); the scheduled trim runs on
/// the job's every `COMPLETIONS_TRIM_EVERY`th completion only.
async fn assert_the_completion_log_is_trimmed(state: &AppState, job_id: &str) {
    seed_job(
        state,
        job_id,
        Some(POST),
        "network",
        "resolving_and_fetching",
        2,
        true,
    )
    .await;
    let queue = bounded_queue(job_id);
    let total = RECENT_COMPLETIONS_PER_JOB + i64::from(COMPLETIONS_TRIM_EVERY) + 5;
    for i in 0..total {
        let unit = WorkUnit {
            did: format!("did:plc:{i:05}"),
            collection: POST.to_string(),
        };
        complete_unit(&jobs(state), &queue, &unit, "https://pds.test", 1)
            .await
            .expect("complete unit");
    }
    assert_eq!(
        count_for_job(state, "happyview_backfill_completions", job_id).await,
        total,
        "completing a unit does not trim"
    );

    trim_completions_on_schedule(&jobs(state), &queue, COMPLETIONS_TRIM_EVERY + 1).await;
    assert_eq!(
        count_for_job(state, "happyview_backfill_completions", job_id).await,
        total,
        "off schedule, nothing is trimmed"
    );

    trim_completions_on_schedule(&jobs(state), &queue, COMPLETIONS_TRIM_EVERY * 11).await;
    assert_eq!(
        count_for_job(state, "happyview_backfill_completions", job_id).await,
        RECENT_COMPLETIONS_PER_JOB
    );
    let sql = adapt_sql(
        "SELECT did FROM happyview_backfill_completions WHERE job_id = ? ORDER BY id LIMIT 1",
        state.db_backend,
    );
    let (oldest,): (String,) = crate::db::query_as(&sql)
        .bind(job_id)
        .fetch_one(&state.db)
        .await
        .expect("oldest kept");
    assert_eq!(
        oldest,
        format!("did:plc:{:05}", total - RECENT_COMPLETIONS_PER_JOB)
    );
    let job = job_row(state, job_id).await;
    assert_eq!(
        (i64::from(job.processed_repos), i64::from(job.total_records)),
        (total, total)
    );
}

#[tokio::test]
async fn the_completion_log_keeps_only_the_most_recent_units() {
    let state = migrated_state().await;
    assert_the_completion_log_is_trimmed(&state, "log-job").await;
}

/// Every SQL increment of a job counter holds at `i32::MAX` rather than
/// failing (Postgres) or leaving a value no reader can decode (SQLite).
async fn assert_job_counters_saturate(state: &AppState, job_id: &str) {
    seed_job(
        state,
        job_id,
        Some(POST),
        "network",
        "resolving_and_fetching",
        2,
        true,
    )
    .await;
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET total_repos = ?, resolved_repos = ?, processed_repos = ?, total_records = ? WHERE id = ?",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(i32::MAX - 1)
        .bind(i32::MAX)
        .bind(i32::MAX - 1)
        .bind(i32::MAX - 3)
        .bind(job_id)
        .execute(&state.db)
        .await
        .expect("seed counters");
    let queue = bounded_queue(job_id);
    let unit = WorkUnit {
        did: format!("did:plc:sat{job_id}"),
        collection: POST.to_string(),
    };
    seed_unit(state, job_id, POST, &unit.did, None).await;

    enqueue_discovered_page(
        &jobs(state),
        job_id,
        POST,
        &[format!("did:plc:more{job_id}")],
        None,
    )
    .await
    .expect("enqueue");
    mark_resolved(&jobs(state), &queue, &unit, "https://pds.test")
        .await
        .expect("mark resolved");
    complete_unit(&jobs(state), &queue, &unit, "https://pds.test", 10)
        .await
        .expect("complete unit");
    add_late_records(&jobs(state), &queue, "https://pds.test", 10)
        .await
        .expect("late records");

    let job = job_row(state, job_id).await;
    assert_eq!(
        (
            job.total_repos,
            job.resolved_repos,
            job.processed_repos,
            job.total_records
        ),
        (i32::MAX, i32::MAX, i32::MAX, i32::MAX)
    );
}

#[tokio::test]
async fn job_counters_saturate_instead_of_overflowing() {
    let state = migrated_state().await;
    assert_job_counters_saturate(&state, "sat-job").await;
}

#[tokio::test]
async fn job_counters_saturate_instead_of_overflowing_on_postgres() {
    let Some(state) = crate::test_support::test_state_from_env().await else {
        eprintln!("skipped (TEST_DATABASE_URL not set)");
        return;
    };
    let job_id = format!("sat-{}", Uuid::new_v4().simple());
    assert_job_counters_saturate(&state, &job_id).await;
    delete_postgres_job(&state, &job_id, &[]).await;
}

#[test]
fn in_memory_counters_saturate() {
    let counter = AtomicI32::new(i32::MAX - 1);
    assert_eq!(saturating_fetch_add(&counter, 5), i32::MAX);
    assert_eq!(saturating_fetch_add(&counter, 1), i32::MAX);
    assert_eq!(counter.load(Ordering::Relaxed), i32::MAX);
}

#[test]
fn completion_failures_trip_only_when_consecutive() {
    let queue = bounded_queue("trip");
    for _ in 1..MAX_CONSECUTIVE_COMPLETION_FAILURES {
        assert!(!queue.record_completion(false));
    }
    assert!(!queue.record_completion(true), "a success resets the run");
    for _ in 1..MAX_CONSECUTIVE_COMPLETION_FAILURES {
        assert!(!queue.record_completion(false));
    }
    assert!(!queue.completions_failing());
    assert!(queue.record_completion(false));
    assert!(queue.completions_failing());
}

/// When every unit completion fails, the job fails after
/// `MAX_CONSECUTIVE_COMPLETION_FAILURES` of them instead of releasing
/// slots for the whole queue.
#[tokio::test]
async fn a_job_whose_completions_keep_failing_fails() {
    let mock = MockServer::start().await;
    let state = pipeline_state(&mock, &[POST]).await;
    crate::db::query(
        "CREATE TRIGGER refuse_unit_deletes BEFORE DELETE ON happyview_backfill_queue \
         BEGIN SELECT RAISE(ABORT, 'unit deletes refused'); END",
    )
    .execute(&state.db)
    .await
    .expect("create trigger");
    mount_plc(&mock).await;
    Mock::given(method("GET"))
        .and(path("/xrpc/com.atproto.repo.listRecords"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"records": []})))
        .mount(&mock)
        .await;

    let dids: Vec<String> = (0..200).map(|i| format!("did:plc:stuck{i:03}")).collect();
    let job_id = create_backfill_job(&jobs(&state), Some(POST), &dids)
        .await
        .expect("create job");
    run_to_end(&state, &job_id, DEFAULT_DISCOVERY_WINDOW).await;

    let (status, error): (String, Option<String>) =
        crate::db::query_as("SELECT status, error FROM happyview_backfill_jobs WHERE id = ?")
            .bind(&job_id)
            .fetch_one(&state.db)
            .await
            .expect("job row");
    assert_eq!(status, "failed");
    assert_eq!(
        error.as_deref(),
        Some(
            "stopped after 20 unit completions in a row failed to commit; \
             see the backfill.write_failed events for the cause"
        )
    );
    let (attempts,): (i64,) = crate::db::query_as(
        "SELECT COUNT(*) FROM happyview_event_logs WHERE event_type = 'backfill.write_failed' \
         AND json_extract(detail, '$.write') = 'complete_unit'",
    )
    .fetch_one(&state.db)
    .await
    .expect("count failures");
    assert!(
        (i64::from(MAX_CONSECUTIVE_COMPLETION_FAILURES)..100).contains(&attempts),
        "the run stops soon after the limit, not at the end of the queue: {attempts}"
    );
}

// -----------------------------------------------------------------------
// Bounded queue on Postgres
// -----------------------------------------------------------------------

/// A Postgres state whose relay, PLC and PDS are `mock`, with POST in the
/// registry. Jobs name their collection, so no lexicon row is needed.
async fn postgres_pipeline_state(mock: &MockServer) -> Option<AppState> {
    let mut state = crate::test_support::test_state_from_env().await?;
    state.config.relay_url = mock.uri();
    state.config.plc_url = mock.uri();
    state.lexicons.upsert(record_lexicon(POST).1).await;
    track_collections(&state).await;
    Some(state)
}

async fn delete_postgres_job(state: &AppState, job_id: &str, dids: &[&str]) {
    for did in dids {
        let sql = adapt_sql(
            "DELETE FROM happyview_records WHERE did = ?",
            state.db_backend,
        );
        crate::db::query(&sql)
            .bind(*did)
            .execute(&state.db)
            .await
            .expect("delete records");
    }
    let sql = adapt_sql(
        "DELETE FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(job_id)
        .execute(&state.db)
        .await
        .expect("delete job");
}

#[tokio::test]
async fn a_network_job_runs_on_the_bounded_queue_on_postgres() {
    let mock = MockServer::start().await;
    let Some(state) = postgres_pipeline_state(&mock).await else {
        eprintln!("skipped (TEST_DATABASE_URL not set)");
        return;
    };
    let tag = Uuid::new_v4().simple().to_string();
    let a = format!("did:plc:a{tag}");
    let b = format!("did:plc:b{tag}");
    let c = format!("did:plc:c{tag}");
    mount_relay_page(&mock, POST, None, &[&a, &b], Some("c1")).await;
    mount_relay_page(&mock, POST, Some("c1"), &[&c], None).await;
    mount_plc(&mock).await;
    mount_records(&mock, &a, POST, 1).await;
    mount_records(&mock, &b, POST, 2).await;
    mount_records(&mock, &c, POST, 1).await;
    forbid_other_record_fetches(&mock).await;

    let job_id = create_backfill_job(&jobs(&state), Some(POST), &[])
        .await
        .expect("create job");
    run_to_end(&state, &job_id, 2).await;

    let job = job_row(&state, &job_id).await;
    assert_eq!(
        (
            job.status.as_str(),
            job.total_repos,
            job.resolved_repos,
            job.processed_repos,
            job.total_records,
            job.discovery_complete
        ),
        ("completed", 3, 3, 3, 4, 1)
    );
    assert_eq!(
        count_for_job(&state, "happyview_backfill_queue", &job_id).await,
        0
    );
    assert_eq!(
        count_for_job(&state, "happyview_backfill_completions", &job_id).await,
        3
    );
    let sql = adapt_sql(
        "SELECT repos, completed_repos, records FROM happyview_backfill_pds_stats WHERE job_id = ?",
        state.db_backend,
    );
    let stats: (i64, i64, i64) = crate::db::query_as(&sql)
        .bind(&job_id)
        .fetch_one(&state.db)
        .await
        .expect("pds stats");
    assert_eq!(stats, (3, 3, 4));
    let sql = adapt_sql(
        "SELECT relay_cursor, done FROM happyview_backfill_cursors WHERE job_id = ?",
        state.db_backend,
    );
    let cursor: (Option<String>, i32) = crate::db::query_as(&sql)
        .bind(&job_id)
        .fetch_one(&state.db)
        .await
        .expect("cursor row");
    assert_eq!(cursor, (None, 1));

    delete_postgres_job(&state, &job_id, &[&a, &b, &c]).await;
}

#[tokio::test]
async fn a_pre_upgrade_job_still_discovering_restarts_on_postgres() {
    let mock = MockServer::start().await;
    let Some(state) = postgres_pipeline_state(&mock).await else {
        eprintln!("skipped (TEST_DATABASE_URL not set)");
        return;
    };
    let tag = Uuid::new_v4().simple().to_string();
    let job_id = format!("legacy-{tag}");
    let dids = [
        format!("did:plc:p{tag}"),
        format!("did:plc:q{tag}"),
        format!("did:plc:r{tag}"),
    ];
    let [p, q, r] = [&dids[0], &dids[1], &dids[2]].map(String::as_str);
    assert_a_discovering_legacy_job_restarts(&state, &mock, &job_id, [p, q, r]).await;
    delete_postgres_job(&state, &job_id, &[p, q, r]).await;
}

#[tokio::test]
async fn the_completion_log_is_trimmed_on_postgres() {
    let Some(state) = crate::test_support::test_state_from_env().await else {
        eprintln!("skipped (TEST_DATABASE_URL not set)");
        return;
    };
    let job_id = format!("log-{}", Uuid::new_v4().simple());
    assert_the_completion_log_is_trimmed(&state, &job_id).await;
    delete_postgres_job(&state, &job_id, &[]).await;
}

/// Resolves `did:` entries to themselves and `<name>.test` handles to
/// `did:plc:<name>`; anything else fails.
fn fake_resolve(input: String) -> std::future::Ready<Result<String, AdminError>> {
    let result = if input.starts_with("did:") {
        Ok(input)
    } else if let Some(name) = input.trim_start_matches('@').strip_suffix(".test") {
        Ok(format!("did:plc:{name}"))
    } else {
        Err(AdminError::BadRequest(format!(
            "could not resolve handle {input}"
        )))
    };
    std::future::ready(result)
}

#[tokio::test]
async fn accounts_are_resolved_and_deduplicated_in_order() {
    let dids = resolve_backfill_accounts(
        vec![
            " did:plc:bob ".into(),
            "alice.test".into(),
            "did:plc:alice".into(),
            "did:plc:bob".into(),
            "".into(),
        ],
        fake_resolve,
    )
    .await
    .expect("resolve accounts");

    assert_eq!(
        dids,
        vec!["did:plc:bob".to_string(), "did:plc:alice".to_string()]
    );
}

#[tokio::test]
async fn blank_only_dids_are_refused() {
    let result = resolve_backfill_accounts(vec!["  ".into(), "".into()], fake_resolve).await;
    assert!(
        matches!(result, Err(AdminError::BadRequest(_))),
        "got {result:?}"
    );
}

/// `create_backfill` appends `did` to the `dids` list, so a blank `did`
/// on its own arrives as a single blank entry.
#[tokio::test]
async fn a_blank_did_alone_is_refused() {
    let result = resolve_backfill_accounts(vec!["".into()], fake_resolve).await;
    assert!(
        matches!(result, Err(AdminError::BadRequest(_))),
        "got {result:?}"
    );
}

#[tokio::test]
async fn no_accounts_resolves_to_no_dids() {
    let dids = resolve_backfill_accounts(Vec::new(), fake_resolve)
        .await
        .expect("empty list is a network backfill");
    assert!(dids.is_empty());
}

#[tokio::test]
async fn any_unresolvable_account_fails_the_request_naming_each_one() {
    let err = resolve_backfill_accounts(
        vec![
            "alice.test".into(),
            "nope.example".into(),
            "also-bad.example".into(),
        ],
        fake_resolve,
    )
    .await
    .expect_err("should refuse");

    let AdminError::BadRequest(msg) = err else {
        panic!("expected BadRequest, got {err:?}");
    };
    assert!(msg.contains("nope.example"), "{msg}");
    assert!(msg.contains("also-bad.example"), "{msg}");
}

#[tokio::test]
async fn more_than_the_account_cap_is_refused_before_resolving() {
    let inputs: Vec<String> = (0..=MAX_BACKFILL_ACCOUNTS)
        .map(|i| format!("did:plc:a{i}"))
        .collect();
    let calls = std::sync::atomic::AtomicUsize::new(0);

    let result = resolve_backfill_accounts(inputs, |input| {
        calls.fetch_add(1, Ordering::Relaxed);
        fake_resolve(input)
    })
    .await;

    assert!(matches!(result, Err(AdminError::BadRequest(_))));
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn a_multi_account_job_is_seeded_and_skips_discovery() {
    let state = migrated_state().await;
    let dids = vec!["did:plc:a".to_string(), "did:plc:b".to_string()];

    let job_id = create_backfill_job(&jobs(&state), Some("dummy.collection"), &dids)
        .await
        .expect("create job");

    let (did, scope, stage, total): (Option<String>, String, String, Option<i32>) =
        crate::db::query_as(
            "SELECT did, scope, stage, total_repos FROM happyview_backfill_jobs WHERE id = ?",
        )
        .bind(&job_id)
        .fetch_one(&state.db)
        .await
        .expect("job row");
    assert_eq!(did, None);
    assert_eq!(scope, "dids");
    assert_eq!(stage, "resolving_pds");
    assert_eq!(total, Some(2));

    let repos: Vec<(String,)> = crate::db::query_as(
        "SELECT did FROM happyview_backfill_queue WHERE job_id = ? AND collection = '' ORDER BY did",
    )
    .bind(&job_id)
    .fetch_all(&state.db)
    .await
    .expect("repo rows");
    assert_eq!(repos.into_iter().map(|(d,)| d).collect::<Vec<_>>(), dids);
}

#[tokio::test]
async fn a_single_account_job_records_its_did() {
    let state = migrated_state().await;

    let job_id = create_backfill_job(&jobs(&state), None, &["did:plc:solo".to_string()])
        .await
        .expect("create job");

    let (did, scope): (Option<String>, String) =
        crate::db::query_as("SELECT did, scope FROM happyview_backfill_jobs WHERE id = ?")
            .bind(&job_id)
            .fetch_one(&state.db)
            .await
            .expect("job row");
    assert_eq!(did.as_deref(), Some("did:plc:solo"));
    assert_eq!(scope, "dids");
}

#[tokio::test]
async fn a_job_without_accounts_discovers_across_the_network() {
    let state = migrated_state().await;

    let job_id = create_backfill_job(&jobs(&state), Some("dummy.collection"), &[])
        .await
        .expect("create job");

    let (did, scope, stage, queue_version, discovery_complete): (
        Option<String>,
        String,
        String,
        i32,
        i32,
    ) = crate::db::query_as(
        "SELECT did, scope, stage, queue_version, discovery_complete FROM happyview_backfill_jobs WHERE id = ?",
    )
    .bind(&job_id)
    .fetch_one(&state.db)
    .await
    .expect("job row");
    assert_eq!(did, None);
    assert_eq!(scope, "network");
    assert_eq!(stage, "pending");
    assert_eq!((queue_version, discovery_complete), (2, 0));
}

#[tokio::test]
async fn the_legacy_did_field_is_merged_into_dids() {
    let state = migrated_state().await;

    let (status, Json(body)) = create_backfill(
        State(jobs(&state)),
        super_auth(&state),
        Json(CreateBackfillBody {
            collection: None,
            did: Some("did:plc:legacy".into()),
            dids: Some(vec!["did:plc:other".into()]),
            source: None,
        }),
    )
    .await
    .expect("create backfill");
    assert_eq!(status, StatusCode::CREATED);
    let job_id = body["id"].as_str().expect("id").to_string();

    let (scope,): (String,) =
        crate::db::query_as("SELECT scope FROM happyview_backfill_jobs WHERE id = ?")
            .bind(&job_id)
            .fetch_one(&state.db)
            .await
            .expect("job row");
    assert_eq!(scope, "dids");

    let repos: Vec<(String,)> = crate::db::query_as(
        "SELECT did FROM happyview_backfill_queue WHERE job_id = ? AND collection = '' ORDER BY did",
    )
    .bind(&job_id)
    .fetch_all(&state.db)
    .await
    .expect("repo rows");
    assert_eq!(
        repos.into_iter().map(|(d,)| d).collect::<Vec<_>>(),
        vec!["did:plc:legacy".to_string(), "did:plc:other".to_string()]
    );
}

async fn state_with_job(
    job_id: &str,
    status: &str,
    stage: &str,
    error_counts: Option<&str>,
) -> AppState {
    let pool = memory_pool().await;
    crate::db::query(
        "CREATE TABLE happyview_backfill_jobs (
            id TEXT PRIMARY KEY,
            status TEXT NOT NULL,
            stage TEXT NOT NULL,
            total_repos INTEGER,
            resolved_repos INTEGER,
            processed_repos INTEGER,
            total_records INTEGER,
            error_counts TEXT,
            discovery_complete INTEGER NOT NULL DEFAULT 1
        )",
    )
    .execute(&pool)
    .await
    .unwrap_or_else(|e| panic!("create happyview_backfill_jobs table: {e}"));

    crate::db::query(
        "INSERT INTO happyview_backfill_jobs \
         (id, status, stage, total_repos, resolved_repos, processed_repos, total_records, error_counts) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(job_id)
    .bind(status)
    .bind(stage)
    .bind(10i32)
    .bind(4i32)
    .bind(2i32)
    .bind(50i32)
    .bind(error_counts)
    .execute(&pool)
    .await
    .unwrap_or_else(|e| panic!("insert backfill job: {e}"));

    test_state_with_pool(pool)
}

#[tokio::test]
async fn snapshot_reflects_current_row() {
    let state = state_with_job(
        "job-1",
        "running",
        "resolving_and_fetching",
        Some(r#"{"dns_failure":2}"#),
    )
    .await;

    let event = build_job_snapshot(&jobs(&state), "job-1")
        .await
        .expect("snapshot for existing job");

    match event {
        happyview_source_jetstream::backfill::types::BackfillEvent::JobSnapshot {
            job_id,
            status,
            stage,
            total_repos,
            resolved_repos,
            processed_repos,
            total_records,
            error_counts,
            discovery_complete,
        } => {
            assert_eq!(job_id, "job-1");
            assert_eq!(status, "running");
            assert_eq!(stage, "resolving_and_fetching");
            assert_eq!(total_repos, Some(10));
            assert_eq!(resolved_repos, Some(4));
            assert_eq!(processed_repos, Some(2));
            assert_eq!(total_records, Some(50));
            assert_eq!(error_counts, serde_json::json!({"dns_failure": 2}));
            assert!(discovery_complete);
        }
        other => panic!("expected JobSnapshot, got {other:?}"),
    }
}

#[tokio::test]
async fn snapshot_defaults_error_counts_when_null() {
    let state = state_with_job("job-2", "completed", "completed", None).await;

    let event = build_job_snapshot(&jobs(&state), "job-2")
        .await
        .expect("snapshot for existing job");

    match event {
        happyview_source_jetstream::backfill::types::BackfillEvent::JobSnapshot {
            error_counts,
            ..
        } => {
            assert_eq!(error_counts, serde_json::json!({}));
        }
        other => panic!("expected JobSnapshot, got {other:?}"),
    }
}

#[tokio::test]
async fn snapshot_is_none_for_unknown_job() {
    let state = state_with_job("job-3", "running", "discovering_repos", None).await;

    assert!(
        build_job_snapshot(&jobs(&state), "does-not-exist")
            .await
            .is_none()
    );
}

// -----------------------------------------------------------------------
// Detail endpoints
// -----------------------------------------------------------------------

async fn seed_completion(state: &AppState, job_id: &str, did: &str, records: i32) {
    let sql = adapt_sql(
        "INSERT INTO happyview_backfill_completions (job_id, did, collection, pds_endpoint, records_fetched) \
         VALUES (?, ?, 'app.test.post', 'https://pds.test', ?)",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(job_id)
        .bind(did)
        .bind(records)
        .execute(&state.db)
        .await
        .expect("seed completion");
}

async fn seed_pds_stats(state: &AppState, job_id: &str, pds: &str, repos: i64) {
    let sql = adapt_sql(
        "INSERT INTO happyview_backfill_pds_stats (job_id, pds_endpoint, repos) VALUES (?, ?, ?)",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(job_id)
        .bind(pds)
        .bind(repos)
        .execute(&state.db)
        .await
        .expect("seed pds stats");
}

async fn seed_cursor(state: &AppState, job_id: &str) {
    let sql = adapt_sql(
        "INSERT INTO happyview_backfill_cursors (job_id, collection, relay_cursor, done) VALUES (?, 'app.test.post', NULL, 1)",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(job_id)
        .execute(&state.db)
        .await
        .expect("seed cursor");
}

/// Give a seeded job its final status and when it got there.
async fn finish_job(state: &AppState, job_id: &str, status: &str, completed_at: &str) {
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET status = ?, completed_at = ? WHERE id = ?",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(status)
        .bind(completed_at)
        .bind(job_id)
        .execute(&state.db)
        .await
        .expect("finish job");
}

/// One row in every per-job table a flush or the retention sweep clears.
async fn seed_every_detail(state: &AppState, job_id: &str) {
    seed_unit(state, job_id, POST, "did:plc:left", None).await;
    seed_cursor(state, job_id).await;
    seed_completion(state, job_id, "did:plc:done", 1).await;
    seed_pds_stats(state, job_id, "https://pds.test", 1).await;
    seed_legacy_repo(state, job_id, "did:plc:legacy", None, "completed").await;
}

const EVERY_DETAIL_TABLE: [&str; 5] = [
    "happyview_backfill_queue",
    "happyview_backfill_cursors",
    "happyview_backfill_completions",
    "happyview_backfill_pds_stats",
    "happyview_backfill_repos",
];

async fn assert_detail_rows(state: &AppState, job_id: &str, expected: i64) {
    for table in EVERY_DETAIL_TABLE {
        assert_eq!(
            count_for_job(state, table, job_id).await,
            expected,
            "{table} rows of {job_id}"
        );
    }
}

fn repos_query(
    phase: Option<&str>,
    cursor: Option<String>,
    limit: i32,
) -> axum::extract::Query<ReposQuery> {
    axum::extract::Query(ReposQuery {
        phase: phase.map(str::to_string),
        cursor,
        limit: Some(limit),
    })
}

fn dids(
    response: &happyview_source_jetstream::backfill::types::BackfillReposResponse,
) -> Vec<&str> {
    response.repos.iter().map(|r| r.did.as_str()).collect()
}

#[tokio::test]
async fn the_job_list_says_which_queue_a_job_is_on() {
    let state = migrated_state().await;
    seed_job(
        &state,
        "bounded",
        Some(POST),
        "network",
        "resolving_and_fetching",
        2,
        false,
    )
    .await;
    seed_job(
        &state,
        "legacy",
        Some(POST),
        "network",
        "completed",
        1,
        true,
    )
    .await;

    let Json(jobs) = backfill_status(State(jobs(&state)), super_auth(&state))
        .await
        .expect("status");
    let job = |id: &str| jobs.iter().find(|j| j.id == id).expect("job listed");
    assert!(!job("bounded").discovery_complete);
    assert_eq!(
        job("bounded").recent_completions_limit,
        Some(RECENT_COMPLETIONS_PER_JOB)
    );
    assert!(job("legacy").discovery_complete);
    assert_eq!(job("legacy").recent_completions_limit, None);
}

#[tokio::test]
async fn a_bounded_job_lists_its_recent_completions_newest_first() {
    let state = migrated_state().await;
    seed_job(
        &state,
        "recent",
        Some(POST),
        "network",
        "resolving_and_fetching",
        2,
        true,
    )
    .await;
    for (did, records) in [("did:plc:1", 1), ("did:plc:2", 2), ("did:plc:3", 3)] {
        seed_completion(&state, "recent", did, records).await;
    }

    let Json(first) = backfill_repos(
        State(jobs(&state)),
        Path("recent".to_string()),
        super_auth(&state),
        repos_query(Some("fetched"), None, 2),
    )
    .await
    .expect("first page");
    assert_eq!(dids(&first), ["did:plc:3", "did:plc:2"]);
    assert!(first.repos.iter().all(|r| r.status == "completed"));
    assert_eq!(first.repos[0].records_fetched, 3);
    assert_eq!(
        first.repos[0].pds_endpoint.as_deref(),
        Some("https://pds.test")
    );

    let Json(rest) = backfill_repos(
        State(jobs(&state)),
        Path("recent".to_string()),
        super_auth(&state),
        repos_query(Some("fetched"), first.cursor.clone(), 2),
    )
    .await
    .expect("second page");
    assert_eq!(dids(&rest), ["did:plc:1"]);
    assert_eq!(rest.cursor, None);
}

#[tokio::test]
async fn a_bounded_job_lists_queued_units_by_collection_then_did() {
    let state = migrated_state().await;
    seed_job(
        &state,
        "queued",
        None,
        "network",
        "resolving_and_fetching",
        2,
        false,
    )
    .await;
    seed_unit(
        &state,
        "queued",
        LIKE,
        "did:plc:b",
        Some("https://pds.test"),
    )
    .await;
    seed_unit(&state, "queued", POST, "did:plc:a", None).await;
    seed_unit(
        &state,
        "queued",
        POST,
        "did:plc:c",
        Some("https://pds.test"),
    )
    .await;
    // The same repo under a second collection is a second unit.
    seed_unit(&state, "queued", POST, "did:plc:b", None).await;

    let Json(resolved) = backfill_repos(
        State(jobs(&state)),
        Path("queued".to_string()),
        super_auth(&state),
        repos_query(Some("resolved"), None, 1),
    )
    .await
    .expect("resolved page");
    assert_eq!(dids(&resolved), ["did:plc:b"]);
    let Json(resolved_rest) = backfill_repos(
        State(jobs(&state)),
        Path("queued".to_string()),
        super_auth(&state),
        repos_query(Some("resolved"), resolved.cursor.clone(), 1),
    )
    .await
    .expect("resolved rest");
    assert_eq!(dids(&resolved_rest), ["did:plc:c"]);
    assert_eq!(resolved_rest.cursor, None);

    let mut every = Vec::new();
    let mut cursor = None;
    loop {
        let Json(page) = backfill_repos(
            State(jobs(&state)),
            Path("queued".to_string()),
            super_auth(&state),
            repos_query(Some("discovered"), cursor, 2),
        )
        .await
        .expect("queued page");
        assert!(page.repos.iter().all(|r| r.status == "pending"));
        every.extend(page.repos.into_iter().map(|r| r.did));
        cursor = page.cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(every, ["did:plc:b", "did:plc:a", "did:plc:b", "did:plc:c"]);
}

#[tokio::test]
async fn an_account_job_pages_its_queue_with_the_all_collections_unit() {
    let state = migrated_state().await;
    seed_job(
        &state,
        "accounts",
        None,
        "dids",
        "resolving_and_fetching",
        2,
        true,
    )
    .await;
    for did in ["did:plc:x", "did:plc:y"] {
        seed_unit(&state, "accounts", ALL_COLLECTIONS, did, None).await;
    }

    let Json(first) = backfill_repos(
        State(jobs(&state)),
        Path("accounts".to_string()),
        super_auth(&state),
        repos_query(None, None, 1),
    )
    .await
    .expect("first page");
    assert_eq!(dids(&first), ["did:plc:x"]);
    let Json(rest) = backfill_repos(
        State(jobs(&state)),
        Path("accounts".to_string()),
        super_auth(&state),
        repos_query(None, first.cursor.clone(), 1),
    )
    .await
    .expect("second page");
    assert_eq!(dids(&rest), ["did:plc:y"]);
    assert_eq!(rest.cursor, None);
}

#[tokio::test]
async fn a_legacy_job_keeps_listing_its_repo_rows() {
    let state = migrated_state().await;
    seed_job(&state, "old", Some(POST), "network", "completed", 1, true).await;
    seed_legacy_repo(&state, "old", "did:plc:y", None, "pending").await;
    seed_legacy_repo(
        &state,
        "old",
        "did:plc:z",
        Some("https://pds.test"),
        "completed",
    )
    .await;
    let sql = adapt_sql(
        "UPDATE happyview_backfill_repos SET records_fetched = 4 WHERE did = 'did:plc:z'",
        state.db_backend,
    );
    crate::db::query(&sql)
        .execute(&state.db)
        .await
        .expect("set records");

    let Json(fetched) = backfill_repos(
        State(jobs(&state)),
        Path("old".to_string()),
        super_auth(&state),
        repos_query(Some("fetched"), None, 10),
    )
    .await
    .expect("fetched");
    assert_eq!(dids(&fetched), ["did:plc:z"]);
    assert_eq!(fetched.repos[0].records_fetched, 4);

    let Json(first) = backfill_repos(
        State(jobs(&state)),
        Path("old".to_string()),
        super_auth(&state),
        repos_query(None, None, 1),
    )
    .await
    .expect("first");
    assert_eq!(dids(&first), ["did:plc:y"]);
    assert_eq!(
        first.cursor.as_deref(),
        Some("did:plc:y"),
        "a DID cursor, as before"
    );
    let Json(rest) = backfill_repos(
        State(jobs(&state)),
        Path("old".to_string()),
        super_auth(&state),
        repos_query(None, first.cursor.clone(), 1),
    )
    .await
    .expect("rest");
    assert_eq!(dids(&rest), ["did:plc:z"]);

    crate::db::query(&adapt_sql(
        "INSERT INTO happyview_backfill_repos (job_id, did, pds_endpoint, status, records_fetched) \
         VALUES ('old', 'did:plc:w', 'https://pds.test', 'completed', 6)",
        state.db_backend,
    ))
    .execute(&state.db)
    .await
    .expect("seed another repo");
    let Json(summary) = backfill_pds_summary(
        State(jobs(&state)),
        Path("old".to_string()),
        super_auth(&state),
    )
    .await
    .expect("summary");
    let rows: Vec<(&str, i64, i64, i64)> = summary
        .pds_endpoints
        .iter()
        .map(|p| {
            (
                p.pds_endpoint.as_str(),
                p.total_repos,
                p.completed_repos,
                p.total_records,
            )
        })
        .collect();
    assert_eq!(rows, [("https://pds.test", 2, 2, 10)]);
}

#[tokio::test]
async fn a_bounded_job_rejects_a_cursor_it_did_not_issue() {
    let state = migrated_state().await;
    seed_job(
        &state,
        "cursor",
        Some(POST),
        "network",
        "resolving_and_fetching",
        2,
        true,
    )
    .await;
    for phase in [Some("fetched"), Some("resolved"), None] {
        let page = backfill_repos(
            State(jobs(&state)),
            Path("cursor".to_string()),
            super_auth(&state),
            repos_query(phase, Some("did:plc:abc".to_string()), 10),
        )
        .await;
        assert!(
            matches!(page, Err(AdminError::BadRequest(_))),
            "phase {phase:?}"
        );
    }
}

#[tokio::test]
async fn the_pds_summary_reads_the_aggregate_for_a_bounded_job() {
    let state = migrated_state().await;
    seed_job(
        &state,
        "summary",
        Some(POST),
        "network",
        "resolving_and_fetching",
        2,
        true,
    )
    .await;
    let sql = adapt_sql(
        "INSERT INTO happyview_backfill_pds_stats (job_id, pds_endpoint, repos, completed_repos, records, errors) VALUES \
         ('summary', 'https://a.test', 5, 3, 40, 1), ('summary', 'https://b.test', 9, 9, 90, 0), \
         ('summary', 'https://c.test', 1, 1, 5000000000, 0)",
        state.db_backend,
    );
    crate::db::query(&sql)
        .execute(&state.db)
        .await
        .expect("seed stats");

    let Json(summary) = backfill_pds_summary(
        State(jobs(&state)),
        Path("summary".to_string()),
        super_auth(&state),
    )
    .await
    .expect("summary");
    let rows: Vec<(&str, i64, i64, i64)> = summary
        .pds_endpoints
        .iter()
        .map(|p| {
            (
                p.pds_endpoint.as_str(),
                p.total_repos,
                p.completed_repos,
                p.total_records,
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("https://b.test", 9, 9, 90),
            ("https://a.test", 5, 3, 40),
            ("https://c.test", 1, 1, 5_000_000_000),
        ]
    );
}

#[tokio::test]
async fn an_unfinished_bounded_job_refuses_a_flush() {
    let state = migrated_state().await;
    for status in ["running", "pausing", "paused", "cancelling"] {
        seed_job(
            &state,
            status,
            Some(POST),
            "network",
            "resolving_and_fetching",
            2,
            false,
        )
        .await;
        let sql = adapt_sql(
            "UPDATE happyview_backfill_jobs SET status = ? WHERE id = ?",
            state.db_backend,
        );
        crate::db::query(&sql)
            .bind(status)
            .bind(status)
            .execute(&state.db)
            .await
            .expect("set status");
        seed_every_detail(&state, status).await;

        let flushed = flush_backfill_details(
            State(jobs(&state)),
            Path(status.to_string()),
            super_auth(&state),
        )
        .await;

        assert!(matches!(flushed, Err(AdminError::Conflict(_))), "{status}");
        assert_detail_rows(&state, status, 1).await;
    }
}

#[tokio::test]
async fn flushing_a_finished_bounded_job_clears_every_row_it_left() {
    let state = migrated_state().await;
    for status in FINISHED_STATUSES {
        seed_job(
            &state,
            status,
            Some(POST),
            "network",
            "resolving_and_fetching",
            2,
            false,
        )
        .await;
        finish_job(&state, status, status, "2026-01-01T00:00:00+00:00").await;
        seed_every_detail(&state, status).await;

        let flushed = flush_backfill_details(
            State(jobs(&state)),
            Path(status.to_string()),
            super_auth(&state),
        )
        .await
        .expect("flush");

        assert_eq!(flushed, StatusCode::NO_CONTENT);
        assert_detail_rows(&state, status, 0).await;
    }
}

#[tokio::test]
async fn a_legacy_job_is_flushed_in_any_state_as_before() {
    let state = migrated_state().await;
    seed_job(
        &state,
        "old-running",
        Some(POST),
        "network",
        "fetching_records",
        1,
        true,
    )
    .await;
    seed_legacy_repo(&state, "old-running", "did:plc:legacy", None, "pending").await;

    flush_backfill_details(
        State(jobs(&state)),
        Path("old-running".to_string()),
        super_auth(&state),
    )
    .await
    .expect("flush");

    assert_eq!(
        count_for_job(&state, "happyview_backfill_repos", "old-running").await,
        0
    );
}

#[tokio::test]
async fn flushing_every_job_clears_only_finished_ones() {
    let state = migrated_state().await;
    for status in FINISHED_STATUSES {
        seed_job(
            &state,
            status,
            Some(POST),
            "network",
            "resolving_and_fetching",
            2,
            false,
        )
        .await;
        finish_job(&state, status, status, "2026-01-01T00:00:00+00:00").await;
        seed_every_detail(&state, status).await;
    }
    seed_job(
        &state,
        "live",
        Some(POST),
        "network",
        "resolving_and_fetching",
        2,
        false,
    )
    .await;
    seed_every_detail(&state, "live").await;

    flush_all_backfill_details(State(jobs(&state)), super_auth(&state))
        .await
        .expect("flush all");

    for status in FINISHED_STATUSES {
        assert_detail_rows(&state, status, 0).await;
    }
    assert_detail_rows(&state, "live", 1).await;
}

#[tokio::test]
async fn the_retention_sweep_clears_every_detail_table_of_old_jobs() {
    let state = migrated_state().await;
    for status in FINISHED_STATUSES {
        let old = format!("old-{status}");
        seed_job(&state, &old, Some(POST), "network", "completed", 2, true).await;
        finish_job(&state, &old, status, "2020-01-01T00:00:00+00:00").await;
        seed_every_detail(&state, &old).await;
    }
    seed_job(
        &state,
        "new-done",
        Some(POST),
        "network",
        "completed",
        2,
        true,
    )
    .await;
    finish_job(&state, "new-done", "completed", "2099-01-01T00:00:00+00:00").await;
    seed_every_detail(&state, "new-done").await;
    // A paused job is not finished, whatever its completed_at says.
    seed_job(
        &state,
        "paused",
        Some(POST),
        "network",
        "resolving_and_fetching",
        2,
        false,
    )
    .await;
    finish_job(&state, "paused", "paused", "2020-01-01T00:00:00+00:00").await;
    seed_every_detail(&state, "paused").await;

    let deleted = sweep_backfill_details(&jobs(&state), "2026-01-01T00:00:00+00:00")
        .await
        .expect("sweep");

    assert_eq!(deleted, 15);
    for status in FINISHED_STATUSES {
        assert_detail_rows(&state, &format!("old-{status}"), 0).await;
    }
    assert_detail_rows(&state, "new-done", 1).await;
    assert_detail_rows(&state, "paused", 1).await;
}

/// `n` rows in every per-job table, so a batch of fewer than `n` takes
/// several rounds to clear each one.
async fn seed_many_details(state: &AppState, job_id: &str, n: usize) {
    for i in 0..n {
        let did = format!("did:plc:many{i}");
        seed_unit(state, job_id, POST, &did, None).await;
        seed_completion(state, job_id, &did, 1).await;
        seed_pds_stats(state, job_id, &format!("https://pds{i}.test"), 1).await;
        seed_legacy_repo(state, job_id, &did, None, "completed").await;
        let sql = adapt_sql(
            "INSERT INTO happyview_backfill_cursors (job_id, collection, done) VALUES (?, ?, 1)",
            state.db_backend,
        );
        crate::db::query(&sql)
            .bind(job_id)
            .bind(format!("app.test.c{i}"))
            .execute(&state.db)
            .await
            .expect("seed cursor");
    }
}

/// Flush and sweep a job holding more rows than one delete batch; `tag`
/// keeps the jobs apart from other tests sharing a Postgres database.
async fn assert_deletes_run_in_batches(state: &AppState, tag: &str) -> Vec<String> {
    let flushed = format!("batched-flush-{tag}");
    seed_job(state, &flushed, Some(POST), "network", "completed", 2, true).await;
    finish_job(state, &flushed, "completed", "2026-01-01T00:00:00+00:00").await;
    seed_many_details(state, &flushed, 5).await;

    flush_job_details(&jobs(state), &flushed, 2)
        .await
        .expect("flush");
    assert_detail_rows(state, &flushed, 0).await;

    // Finished before any other test's job, so the sweep touches only it.
    let swept = format!("batched-sweep-{tag}");
    seed_job(state, &swept, Some(POST), "network", "completed", 2, true).await;
    finish_job(state, &swept, "cancelled", "1999-01-01T00:00:00+00:00").await;
    seed_many_details(state, &swept, 5).await;

    let deleted = sweep_backfill_details_in_batches(&jobs(state), "2000-01-01T00:00:00+00:00", 2)
        .await
        .expect("sweep");
    assert!(deleted >= 25, "deleted {deleted}");
    assert_detail_rows(state, &swept, 0).await;
    vec![flushed, swept]
}

#[tokio::test]
async fn flush_and_retention_delete_in_batches() {
    let state = migrated_state().await;
    assert_deletes_run_in_batches(&state, "sqlite").await;

    seed_job(
        &state,
        "batched-all",
        Some(POST),
        "network",
        "completed",
        2,
        true,
    )
    .await;
    finish_job(&state, "batched-all", "failed", "2026-01-01T00:00:00+00:00").await;
    seed_many_details(&state, "batched-all", 5).await;
    flush_finished_details(&jobs(&state), 2)
        .await
        .expect("flush all");
    assert_detail_rows(&state, "batched-all", 0).await;
}

#[tokio::test]
async fn flush_and_retention_delete_in_batches_on_postgres() {
    let Some(state) = crate::test_support::test_state_from_env().await else {
        eprintln!("skipped (TEST_DATABASE_URL not set)");
        return;
    };
    let tag = Uuid::new_v4().simple().to_string();
    for job_id in assert_deletes_run_in_batches(&state, &tag).await {
        let sql = adapt_sql(
            "DELETE FROM happyview_backfill_jobs WHERE id = ?",
            state.db_backend,
        );
        crate::db::query(&sql)
            .bind(&job_id)
            .execute(&state.db)
            .await
            .expect("delete job");
    }
}

#[tokio::test]
async fn deleting_a_job_deletes_every_row_it_left() {
    let state = migrated_state().await;
    seed_job(
        &state,
        "gone",
        Some(POST),
        "network",
        "resolving_and_fetching",
        2,
        false,
    )
    .await;
    finish_job(&state, "gone", "cancelled", "2026-01-01T00:00:00+00:00").await;
    seed_every_detail(&state, "gone").await;

    let sql = adapt_sql(
        "DELETE FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind("gone")
        .execute(&state.db)
        .await
        .expect("delete job");

    assert_detail_rows(&state, "gone", 0).await;
}

// -----------------------------------------------------------------------
// Errors API
// -----------------------------------------------------------------------

async fn state_with_errors_job(
    job_id: &str,
    collection: Option<&str>,
    error_counts_json: &str,
) -> AppState {
    let state = migrated_state().await;
    crate::db::query(
        "INSERT INTO happyview_backfill_jobs \
         (id, collection, did, status, stage, created_at, error_counts) \
         VALUES (?, ?, NULL, 'completed', 'completed', '2026-01-01T00:00:00+00:00', ?)",
    )
    .bind(job_id)
    .bind(collection)
    .bind(error_counts_json)
    .execute(&state.db)
    .await
    .unwrap_or_else(|e| panic!("insert backfill job: {e}"));
    state
}

fn super_auth(_state: &AppState) -> AdminUser {
    AdminUser(AdminCaller {
        did: "did:plc:admin".to_string(),
        permissions: vec!["backfill:create".into(), "backfill:read".into()],
        platform: false,
    })
}

async fn insert_error(
    state: &AppState,
    job_id: &str,
    did: &str,
    phase: &str,
    kind: BackfillErrorKind,
) {
    crate::db::query(
        "INSERT INTO happyview_backfill_errors \
         (job_id, did, collection, phase, kind, message, attempts, last_at) \
         VALUES (?, ?, NULL, ?, ?, 'boom', 1, '2026-01-01T00:00:00+00:00')",
    )
    .bind(job_id)
    .bind(did)
    .bind(phase)
    .bind(kind.as_str())
    .execute(&state.backfill_db)
    .await
    .unwrap_or_else(|e| panic!("insert backfill error: {e}"));
}

#[tokio::test]
async fn errors_list_reports_exact_counts_and_capped_flag() {
    let state = state_with_errors_job(
        "job-err-1",
        Some("dummy.collection"),
        r#"{"dns_failure":2,"repo_not_found":1}"#,
    )
    .await;
    insert_error(
        &state,
        "job-err-1",
        "did:plc:a",
        "resolve",
        BackfillErrorKind::DnsFailure,
    )
    .await;
    insert_error(
        &state,
        "job-err-1",
        "did:plc:b",
        "resolve",
        BackfillErrorKind::DnsFailure,
    )
    .await;
    insert_error(
        &state,
        "job-err-1",
        "did:plc:c",
        "fetch",
        BackfillErrorKind::RepoNotFound,
    )
    .await;

    let response = backfill_errors_list(
        State(jobs(&state)),
        Path("job-err-1".to_string()),
        super_auth(&state),
        axum::extract::Query(BackfillErrorsQuery {
            kind: None,
            cursor: None,
            limit: None,
        }),
    )
    .await
    .expect("list errors")
    .0;

    assert_eq!(response.errors.len(), 3);
    assert!(!response.capped);

    let dns = response
        .counts
        .iter()
        .find(|c| c.kind == "dns_failure")
        .expect("dns_failure present");
    assert_eq!(dns.count, 2);
    assert!(dns.retryable);

    let repo = response
        .counts
        .iter()
        .find(|c| c.kind == "repo_not_found")
        .expect("repo_not_found present");
    assert_eq!(repo.count, 1);
    assert!(!repo.retryable);

    // Zero-count kinds are skipped rather than zero-filled.
    assert!(!response.counts.iter().any(|c| c.kind == "timeout"));
}

#[tokio::test]
async fn errors_list_counts_are_live_before_the_first_flush() {
    // `error_counts` is only written by a terminal `ErrorRecorder::flush`,
    // so mid-run it is empty (or, after a crash, stale) while detail rows
    // pile up underneath. Counting the table is what puts an Errors row on
    // the dashboard during the hours a backfill actually takes.
    let state = state_with_errors_job("job-err-live", None, "{}").await;
    for (did, kind) in [
        ("did:plc:a", BackfillErrorKind::DnsFailure),
        ("did:plc:b", BackfillErrorKind::DnsFailure),
        ("did:plc:c", BackfillErrorKind::RepoNotFound),
    ] {
        insert_error(&state, "job-err-live", did, "resolve", kind).await;
    }

    let response = backfill_errors_list(
        State(jobs(&state)),
        Path("job-err-live".to_string()),
        super_auth(&state),
        axum::extract::Query(BackfillErrorsQuery {
            kind: None,
            cursor: None,
            limit: None,
        }),
    )
    .await
    .expect("list errors")
    .0;

    let dns = response
        .counts
        .iter()
        .find(|c| c.kind == "dns_failure")
        .expect("dns_failure present despite an empty error_counts");
    assert_eq!(dns.count, 2);
    assert_eq!(
        response
            .counts
            .iter()
            .find(|c| c.kind == "repo_not_found")
            .map(|c| c.count),
        Some(1)
    );
}

#[tokio::test]
async fn errors_list_counts_keep_the_json_where_it_exceeds_the_table() {
    // Above `ERROR_DETAIL_CAP` rows stop being written, so the flushed JSON
    // is the only remaining record of how many failures there were. The
    // table must raise a count, never lower one.
    let state =
        state_with_errors_job("job-err-max", None, r#"{"dns_failure":9000,"timeout":1}"#).await;
    insert_error(
        &state,
        "job-err-max",
        "did:plc:a",
        "resolve",
        BackfillErrorKind::DnsFailure,
    )
    .await;
    for did in ["did:plc:b", "did:plc:c"] {
        insert_error(
            &state,
            "job-err-max",
            did,
            "resolve",
            BackfillErrorKind::Timeout,
        )
        .await;
    }

    let response = backfill_errors_list(
        State(jobs(&state)),
        Path("job-err-max".to_string()),
        super_auth(&state),
        axum::extract::Query(BackfillErrorsQuery {
            kind: None,
            cursor: None,
            limit: None,
        }),
    )
    .await
    .expect("list errors")
    .0;

    let count = |kind: &str| {
        response
            .counts
            .iter()
            .find(|c| c.kind == kind)
            .map(|c| c.count)
    };
    // JSON wins where it is higher...
    assert_eq!(count("dns_failure"), Some(9000));
    // ...and the table wins where it is.
    assert_eq!(count("timeout"), Some(2));
}

#[test]
fn a_retryable_give_up_is_recorded_in_preference_to_a_permanent_one() {
    // Only one give-up per DID reaches the recorder; `retry-failed`
    // selects on retryability, so the retryable one is what keeps the DID
    // reachable by a retry job.
    use happyview_source_jetstream::backfill::errors::BackfillFailure;

    let failed = |kind: BackfillErrorKind| FetchOutcome::Failed {
        count: 0,
        cursor: None,
        failure: BackfillFailure {
            kind,
            message: String::new(),
            retry_after: None,
        },
    };

    let mut failures = vec![
        (
            "app.bsky.feed.post".to_string(),
            failed(BackfillErrorKind::RepoNotFound),
        ),
        (
            "app.bsky.feed.like".to_string(),
            failed(BackfillErrorKind::Other),
        ),
        (
            "app.bsky.graph.follow".to_string(),
            failed(BackfillErrorKind::Timeout),
        ),
    ];
    retryable_give_ups_first(&mut failures);

    assert_eq!(failures[0].0, "app.bsky.graph.follow");
    // The rest keep their original relative order, so the log stays
    // predictable.
    assert_eq!(failures[1].0, "app.bsky.feed.post");
    assert_eq!(failures[2].0, "app.bsky.feed.like");
}

#[tokio::test]
async fn errors_list_reports_capped_once_the_total_reaches_the_cap() {
    let state = state_with_errors_job(
        "job-err-cap",
        None,
        &format!(r#"{{"dns_failure":{ERROR_DETAIL_CAP}}}"#),
    )
    .await;

    let response = backfill_errors_list(
        State(jobs(&state)),
        Path("job-err-cap".to_string()),
        super_auth(&state),
        axum::extract::Query(BackfillErrorsQuery {
            kind: None,
            cursor: None,
            limit: None,
        }),
    )
    .await
    .expect("list errors")
    .0;

    assert!(response.capped);
    assert_eq!(response.cap, ERROR_DETAIL_CAP);
}

#[tokio::test]
async fn errors_list_filters_by_kind_and_paginates_by_cursor() {
    let state = state_with_errors_job("job-err-2", None, "{}").await;
    for (did, kind) in [
        ("did:plc:a", BackfillErrorKind::DnsFailure),
        ("did:plc:b", BackfillErrorKind::DnsFailure),
        ("did:plc:c", BackfillErrorKind::RepoNotFound),
    ] {
        insert_error(&state, "job-err-2", did, "resolve", kind).await;
    }

    let response = backfill_errors_list(
        State(jobs(&state)),
        Path("job-err-2".to_string()),
        super_auth(&state),
        axum::extract::Query(BackfillErrorsQuery {
            kind: Some("dns_failure".to_string()),
            cursor: None,
            limit: Some(1),
        }),
    )
    .await
    .expect("list errors")
    .0;

    assert_eq!(response.errors.len(), 1);
    assert_eq!(response.errors[0].did, "did:plc:a");
    assert_eq!(response.errors[0].kind, "dns_failure");
    assert_eq!(response.cursor.as_deref(), Some("did:plc:a"));

    // Turn the page: the cursor from page 1 must reach the remaining
    // dns_failure row (did:plc:b) and not did:plc:c, which is filtered
    // out by kind, and the second page must terminate the pagination.
    let page2 = backfill_errors_list(
        State(jobs(&state)),
        Path("job-err-2".to_string()),
        super_auth(&state),
        axum::extract::Query(BackfillErrorsQuery {
            kind: Some("dns_failure".to_string()),
            cursor: response.cursor.clone(),
            limit: Some(1),
        }),
    )
    .await
    .expect("list errors page 2")
    .0;

    assert_eq!(page2.errors.len(), 1);
    assert_eq!(page2.errors[0].did, "did:plc:b");
    assert_eq!(page2.errors[0].kind, "dns_failure");
    assert_eq!(page2.cursor, None, "second page should end the pagination");
}

#[tokio::test]
async fn retry_failed_seeds_new_job_from_retryable_kinds_only() {
    let state = state_with_errors_job(
        "job-retry-1",
        Some("dummy.collection"),
        r#"{"dns_failure":2,"repo_not_found":1}"#,
    )
    .await;
    insert_error(
        &state,
        "job-retry-1",
        "did:plc:a",
        "resolve",
        BackfillErrorKind::DnsFailure,
    )
    .await;
    insert_error(
        &state,
        "job-retry-1",
        "did:plc:b",
        "resolve",
        BackfillErrorKind::DnsFailure,
    )
    .await;
    insert_error(
        &state,
        "job-retry-1",
        "did:plc:c",
        "fetch",
        BackfillErrorKind::RepoNotFound,
    )
    .await;

    let (status, Json(body)) = retry_failed_backfill(
        State(jobs(&state)),
        super_auth(&state),
        Path("job-retry-1".to_string()),
        Json(RetryFailedBody { kinds: None }),
    )
    .await
    .expect("retry-failed");

    assert_eq!(status, StatusCode::CREATED);
    let new_job_id = body["id"].as_str().expect("id field").to_string();
    assert_ne!(new_job_id, "job-retry-1");

    let (stage, scope): (String, String) =
        crate::db::query_as("SELECT stage, scope FROM happyview_backfill_jobs WHERE id = ?")
            .bind(&new_job_id)
            .fetch_one(&state.backfill_db)
            .await
            .expect("new job row");
    assert_eq!(stage, "resolving_pds");
    assert_eq!(scope, "dids");

    let mut repo_dids: Vec<String> = crate::db::query_as::<(String,)>(
        "SELECT did FROM happyview_backfill_queue WHERE job_id = ? ORDER BY did",
    )
    .bind(&new_job_id)
    .fetch_all(&state.backfill_db)
    .await
    .expect("repo rows")
    .into_iter()
    .map(|(did,)| did)
    .collect();
    repo_dids.sort();
    assert_eq!(
        repo_dids,
        vec!["did:plc:a".to_string(), "did:plc:b".to_string()],
        "only the retryable dns_failure DIDs should be re-seeded"
    );
}

#[tokio::test]
async fn retry_failed_honors_explicit_kinds_beyond_the_default_retryable_set() {
    let state = state_with_errors_job(
        "job-retry-2",
        Some("dummy.collection"),
        r#"{"repo_not_found":1}"#,
    )
    .await;
    insert_error(
        &state,
        "job-retry-2",
        "did:plc:a",
        "fetch",
        BackfillErrorKind::RepoNotFound,
    )
    .await;

    let (status, Json(body)) = retry_failed_backfill(
        State(jobs(&state)),
        super_auth(&state),
        Path("job-retry-2".to_string()),
        Json(RetryFailedBody {
            kinds: Some(vec!["repo_not_found".to_string()]),
        }),
    )
    .await
    .expect("retry-failed with an explicit non-retryable kind");

    assert_eq!(status, StatusCode::CREATED);
    let new_job_id = body["id"].as_str().expect("id field").to_string();
    let (did,): (String,) =
        crate::db::query_as("SELECT did FROM happyview_backfill_queue WHERE job_id = ?")
            .bind(&new_job_id)
            .fetch_one(&state.backfill_db)
            .await
            .expect("repo row");
    assert_eq!(did, "did:plc:a");
}

#[tokio::test]
async fn retry_failed_rejects_when_nothing_retryable() {
    let state = state_with_errors_job(
        "job-retry-3",
        Some("dummy.collection"),
        r#"{"repo_not_found":1}"#,
    )
    .await;
    insert_error(
        &state,
        "job-retry-3",
        "did:plc:a",
        "fetch",
        BackfillErrorKind::RepoNotFound,
    )
    .await;

    let err = retry_failed_backfill(
        State(jobs(&state)),
        super_auth(&state),
        Path("job-retry-3".to_string()),
        Json(RetryFailedBody { kinds: None }),
    )
    .await
    .expect_err("should reject a job with no retryable failures");

    assert!(matches!(err, AdminError::BadRequest(_)));
}

#[tokio::test]
async fn retry_failed_returns_not_found_for_unknown_job() {
    let state = state_with_errors_job("job-retry-4", None, "{}").await;

    let err = retry_failed_backfill(
        State(jobs(&state)),
        super_auth(&state),
        Path("does-not-exist".to_string()),
        Json(RetryFailedBody { kinds: None }),
    )
    .await
    .expect_err("should 404 for an unknown job");

    assert!(matches!(err, AdminError::NotFound(_)));
}

// -----------------------------------------------------------------------
// Page writes
// -----------------------------------------------------------------------

fn prepared(rkey: &str, body: serde_json::Value) -> PreparedRecord {
    PreparedRecord {
        did: "did:plc:page".to_string(),
        collection: POST.to_string(),
        rkey: rkey.to_string(),
        record_json: body.to_string(),
        cid: format!("bafy{rkey}"),
        rev: None,
    }
}

async fn ref_count(state: &AppState, uri: &str) -> i64 {
    let sql = adapt_sql(
        "SELECT COUNT(*) FROM happyview_record_refs WHERE source_uri = ?",
        state.db_backend,
    );
    crate::db::query_as::<(i64,)>(&sql)
        .bind(uri)
        .fetch_one(&state.db)
        .await
        .expect("count refs")
        .0
}

/// An identical page writes nothing and leaves refs alone (they are
/// deleted here so a rewrite would show); an edited row is written and
/// its refs rebuilt.
async fn assert_page_writes_only_changes(state: &AppState, prefix: &str) {
    let subject = "at://did:plc:t/app.test.post/1";
    let page = vec![
        prepared(
            &format!("{prefix}a"),
            serde_json::json!({"subject": subject}),
        ),
        prepared(&format!("{prefix}b"), serde_json::json!({"text": "plain"})),
    ];
    let linked = page[0].uri();

    let written = write_records_page(state, &page).await.expect("first write");
    assert_eq!(written.len(), 2);
    assert_eq!(ref_count(state, &linked).await, 1);

    let sql = adapt_sql(
        "DELETE FROM happyview_record_refs WHERE source_uri = ?",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(&linked)
        .execute(&state.db)
        .await
        .expect("delete refs");
    let again = write_records_page(state, &page)
        .await
        .expect("identical write");
    assert!(
        again.is_empty(),
        "an identical page writes nothing: {again:?}"
    );
    assert_eq!(
        ref_count(state, &linked).await,
        0,
        "unchanged rows keep their refs untouched"
    );

    let mut edited = page;
    edited[0].record_json = serde_json::json!({"subject": subject, "text": "edited"}).to_string();
    let changed = write_records_page(state, &edited)
        .await
        .expect("edited write");
    assert_eq!(changed, vec![linked.clone()]);
    assert_eq!(
        ref_count(state, &linked).await,
        1,
        "a changed row gets its refs rebuilt"
    );
}

#[tokio::test]
async fn a_backfill_page_skips_unchanged_rows() {
    let state = migrated_state().await;
    assert_page_writes_only_changes(&state, "s").await;
}

#[tokio::test]
async fn a_backfill_page_skips_unchanged_rows_on_postgres() {
    let Some(state) = crate::test_support::test_state_from_env().await else {
        eprintln!("skipped (TEST_DATABASE_URL not set)");
        return;
    };
    let prefix = format!("p{}", Uuid::new_v4().simple());
    assert_page_writes_only_changes(&state, &prefix).await;

    let sql = adapt_sql(
        "DELETE FROM happyview_records WHERE uri LIKE ?",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(format!("at://did:plc:page/{POST}/{prefix}%"))
        .execute(&state.db)
        .await
        .expect("clean up");
}

/// A page far past `limit` (more bound parameters than either backend
/// takes in one statement) is written in chunks, and a URI repeated in a
/// page is written once, with its last copy.
async fn assert_oversized_page_with_repeats_is_written(state: &AppState, prefix: &str) {
    let mut page: Vec<PreparedRecord> = (0..9000)
        .map(|i| prepared(&format!("{prefix}n{i:05}"), serde_json::json!({"n": i})))
        .collect();
    let repeated = page[150].uri();
    let mut last = prepared(
        &format!("{prefix}n00150"),
        serde_json::json!({"subject": "at://did:plc:t/app.test.post/1"}),
    );
    last.cid = "bafylast".to_string();
    page.push(last);

    let written = write_records_page(state, &page)
        .await
        .expect("oversized write");
    assert_eq!(written.len(), 9000);

    let sql = adapt_sql(
        "SELECT COUNT(*) FROM happyview_records WHERE uri LIKE ?",
        state.db_backend,
    );
    let (rows,): (i64,) = crate::db::query_as(&sql)
        .bind(format!("at://did:plc:page/{POST}/{prefix}%"))
        .fetch_one(&state.db)
        .await
        .expect("count rows");
    assert_eq!(rows, 9000);
    let sql = adapt_sql(
        "SELECT cid FROM happyview_records WHERE uri = ?",
        state.db_backend,
    );
    let (cid,): (String,) = crate::db::query_as(&sql)
        .bind(&repeated)
        .fetch_one(&state.db)
        .await
        .expect("repeated row");
    assert_eq!(cid, "bafylast", "the last copy wins");
    assert_eq!(ref_count(state, &repeated).await, 1);
}

#[tokio::test]
async fn an_oversized_page_with_repeats_is_written() {
    let state = migrated_state().await;
    assert_oversized_page_with_repeats_is_written(&state, "o").await;
}

#[tokio::test]
async fn an_oversized_page_with_repeats_is_written_on_postgres() {
    let Some(state) = crate::test_support::test_state_from_env().await else {
        eprintln!("skipped (TEST_DATABASE_URL not set)");
        return;
    };
    let prefix = format!("o{}", Uuid::new_v4().simple());
    assert_oversized_page_with_repeats_is_written(&state, &prefix).await;

    for table in ["happyview_record_refs", "happyview_records"] {
        let column = if table == "happyview_records" {
            "uri"
        } else {
            "source_uri"
        };
        let sql = adapt_sql(
            &format!("DELETE FROM {table} WHERE {column} LIKE ?"),
            state.db_backend,
        );
        crate::db::query(&sql)
            .bind(format!("at://did:plc:page/{POST}/{prefix}%"))
            .execute(&state.db)
            .await
            .expect("clean up");
    }
}

fn page_rev(n: i64) -> String {
    crate::tid::tid_from_unix_microseconds(1_700_000_000_000_000 + n)
}

async fn stored_record(state: &AppState, uri: &str) -> Option<(serde_json::Value, Option<String>)> {
    let sql = adapt_sql(
        "SELECT record, rev FROM happyview_records WHERE uri = ?",
        state.db_backend,
    );
    crate::db::query_as::<(String, Option<String>)>(&sql)
        .bind(uri)
        .fetch_optional(&state.db)
        .await
        .expect("row")
        .map(|(body, rev)| (serde_json::from_str(&body).expect("json"), rev))
}

async fn clean_up_page_prefix(state: &AppState, prefix: &str) {
    for (table, column) in [
        ("happyview_record_refs", "source_uri"),
        ("happyview_records", "uri"),
        ("happyview_record_tombstones", "uri"),
    ] {
        let sql = adapt_sql(
            &format!("DELETE FROM {table} WHERE {column} LIKE ?"),
            state.db_backend,
        );
        crate::db::query(&sql)
            .bind(format!("at://did:plc:page/{POST}/{prefix}%"))
            .execute(&state.db)
            .await
            .expect("clean up");
    }
}

/// A crawl row cannot resurrect a record deleted live, and a row with an
/// older rev than the stored one is not written.
async fn assert_page_respects_tombstones_and_revs(state: &AppState, prefix: &str) {
    let dead = prepared(
        &format!("{prefix}dead"),
        serde_json::json!({"text": "deleted live"}),
    );
    {
        let mut conn = state.db.acquire().await.expect("connection");
        crate::tombstones::write_record_tombstone(
            &mut conn,
            state.db_backend,
            &dead.uri(),
            &page_rev(50),
            &now_rfc3339(),
        )
        .await
        .expect("tombstone");
    }
    let fresh = prepared(
        &format!("{prefix}fresh"),
        serde_json::json!({"text": "fresh"}),
    );
    let fresh_uri = fresh.uri();
    let written = write_records_page(state, &[dead, fresh])
        .await
        .expect("write");
    assert_eq!(written, vec![fresh_uri], "the tombstoned row is skipped");

    let mut newer = prepared(
        &format!("{prefix}rev"),
        serde_json::json!({"text": "newer"}),
    );
    newer.rev = Some(page_rev(20));
    let uri = newer.uri();
    write_records_page(state, &[newer]).await.expect("newer");
    let mut older = prepared(
        &format!("{prefix}rev"),
        serde_json::json!({"text": "older"}),
    );
    older.rev = Some(page_rev(10));
    assert!(
        write_records_page(state, &[older])
            .await
            .expect("older")
            .is_empty(),
        "an older rev is not written"
    );
    let sql = adapt_sql(
        "SELECT record FROM happyview_records WHERE uri = ?",
        state.db_backend,
    );
    let (body,): (String,) = crate::db::query_as(&sql)
        .bind(&uri)
        .fetch_one(&state.db)
        .await
        .expect("row");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).expect("json"),
        serde_json::json!({"text": "newer"})
    );
}

#[tokio::test]
async fn a_backfill_page_respects_tombstones_and_revs() {
    let state = migrated_state().await;
    assert_page_respects_tombstones_and_revs(&state, "t").await;
}

#[tokio::test]
async fn a_backfill_page_respects_tombstones_and_revs_on_postgres() {
    let Some(state) = crate::test_support::test_state_from_env().await else {
        eprintln!("skipped (TEST_DATABASE_URL not set)");
        return;
    };
    let prefix = format!("t{}", Uuid::new_v4().simple());
    assert_page_respects_tombstones_and_revs(&state, &prefix).await;
    clean_up_page_prefix(&state, &prefix).await;
}

/// An unversioned crawl row that changes a record keeps the stored rev,
/// so an older versioned write is still refused afterwards.
async fn assert_crawl_row_keeps_the_stored_rev(state: &AppState, prefix: &str) {
    let mut versioned = prepared(&format!("{prefix}kept"), serde_json::json!({"text": "v20"}));
    versioned.rev = Some(page_rev(20));
    let uri = versioned.uri();
    write_records_page(state, &[versioned])
        .await
        .expect("versioned");

    let crawled = prepared(
        &format!("{prefix}kept"),
        serde_json::json!({"text": "crawled"}),
    );
    assert_eq!(
        write_records_page(state, &[crawled])
            .await
            .expect("crawled"),
        vec![uri.clone()],
        "an unversioned row with no tombstone is written"
    );
    assert_eq!(
        stored_record(state, &uri).await,
        Some((serde_json::json!({"text": "crawled"}), Some(page_rev(20)))),
        "the crawl row keeps the stored rev"
    );

    let mut older = prepared(&format!("{prefix}kept"), serde_json::json!({"text": "v10"}));
    older.rev = Some(page_rev(10));
    assert!(
        write_records_page(state, &[older])
            .await
            .expect("older")
            .is_empty(),
        "an older rev is still refused"
    );
}

#[tokio::test]
async fn a_crawl_row_keeps_the_stored_rev() {
    let state = migrated_state().await;
    assert_crawl_row_keeps_the_stored_rev(&state, "k").await;
}

#[tokio::test]
async fn a_crawl_row_keeps_the_stored_rev_on_postgres() {
    let Some(state) = crate::test_support::test_state_from_env().await else {
        eprintln!("skipped (TEST_DATABASE_URL not set)");
        return;
    };
    let prefix = format!("k{}", Uuid::new_v4().simple());
    assert_crawl_row_keeps_the_stored_rev(&state, &prefix).await;
    clean_up_page_prefix(&state, &prefix).await;
}

/// A tombstone being written for a URI in a page, uncommitted, holds the
/// page back until it commits; the page then sees it, rather than
/// checking before it lands and writing after.
#[tokio::test]
async fn a_backfill_page_waits_for_a_concurrent_tombstone_on_postgres() {
    let Some(state) = crate::test_support::test_state_from_env().await else {
        eprintln!("skipped (TEST_DATABASE_URL not set)");
        return;
    };
    let prefix = format!("r{}", Uuid::new_v4().simple());
    let dead = prepared(
        &format!("{prefix}dead"),
        serde_json::json!({"text": "deleted live"}),
    );
    let fresh = prepared(
        &format!("{prefix}fresh"),
        serde_json::json!({"text": "fresh"}),
    );
    let (dead_uri, fresh_uri) = (dead.uri(), fresh.uri());

    let mut tx = crate::db::begin_write(&state.db, state.db_backend)
        .await
        .expect("begin");
    crate::tombstones::lock_uris(&mut tx, state.db_backend, &[&dead_uri])
        .await
        .expect("lock uri");
    crate::tombstones::write_record_tombstone(
        &mut tx,
        state.db_backend,
        &dead_uri,
        &page_rev(50),
        &now_rfc3339(),
    )
    .await
    .expect("write tombstone");

    let racer = {
        let state = state.clone();
        tokio::spawn(async move { write_records_page(&state, &[fresh, dead]).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(
        !racer.is_finished(),
        "the page must wait for the uncommitted tombstone"
    );
    tx.commit().await.expect("commit tombstone");

    let written = racer.await.expect("join").expect("write");
    assert_eq!(written, vec![fresh_uri]);
    assert!(
        stored_record(&state, &dead_uri).await.is_none(),
        "the page must see the tombstone it waited for"
    );
    clean_up_page_prefix(&state, &prefix).await;
}

/// A page that cannot be written rolls back whole and becomes a fetch
/// failure the recorder keeps, rather than a warning nobody reads.
#[tokio::test]
async fn a_page_that_cannot_be_written_is_a_fetch_failure() {
    let mock = MockServer::start().await;
    let state = migrated_state().await;
    // The ingest pipeline stores records for tracked collections only.
    state.lexicons.upsert(record_lexicon(POST).1).await;
    track_collections(&state).await;
    crate::db::query("DROP TABLE happyview_record_refs_v2")
        .execute(&state.db)
        .await
        .expect("drop refs table");
    let value = serde_json::json!({"$type": POST, "subject": "at://did:plc:t/app.test.post/1"});
    let cid = crate::cid_verify::compute_record_cid(&value)
        .expect("cid")
        .to_string();
    Mock::given(method("GET"))
        .and(path("/xrpc/com.atproto.repo.listRecords"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "records": [{"uri": format!("at://did:plc:f/{POST}/1"), "cid": cid, "value": value}]
        })))
        .mount(&mock)
        .await;

    let outcome = fetch_records_page_loop(
        &jobs(&state),
        "job",
        &mock.uri(),
        "did:plc:f",
        POST,
        None,
        &AtomicBool::new(false),
    )
    .await;

    match outcome {
        FetchOutcome::Failed {
            count,
            cursor,
            failure,
        } => {
            assert_eq!(failure.kind, BackfillErrorKind::Other);
            assert!(
                failure.message.contains("database write failed"),
                "{}",
                failure.message
            );
            assert_eq!((count, cursor), (0, None));
        }
        FetchOutcome::Complete { .. } => {
            panic!("a page that cannot be written must not complete")
        }
    }
    let (records,): (i64,) = crate::db::query_as("SELECT COUNT(*) FROM happyview_records")
        .fetch_one(&state.db)
        .await
        .expect("count records");
    assert_eq!(records, 0, "the page rolls back whole");
}

#[tokio::test]
async fn a_bookkeeping_write_that_fails_is_logged_not_dropped() {
    let state = migrated_state().await;
    let result = job_write(&jobs(&state), "job-x", "test_write", || {
        crate::db::query("UPDATE no_such_table SET x = 1").execute(&state.backfill_db)
    })
    .await;
    assert!(result.is_none());

    let (logged,): (i64,) = crate::db::query_as(
        "SELECT COUNT(*) FROM happyview_event_logs WHERE event_type = 'backfill.write_failed' AND subject = 'job-x'",
    )
    .fetch_one(&state.db)
    .await
    .expect("count events");
    assert_eq!(logged, 1);
}

// -----------------------------------------------------------------------
// Running jobs, as `backfill_running` answers from them
// -----------------------------------------------------------------------

/// The relay answers `collection`'s first page only after `delay`, so a job
/// discovering it stays running that long.
async fn mount_slow_relay_page(mock: &MockServer, collection: &str, did: &str, delay: Duration) {
    Mock::given(method("GET"))
        .and(path("/xrpc/com.atproto.sync.listReposByCollection"))
        .and(query_param("collection", collection))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"repos": [{"did": did}]}))
                .set_delay(delay),
        )
        .mount(mock)
        .await;
}

/// A state over `mock` with `POST` tracked, and its Jetstream source.
async fn running_source(
    mock: &MockServer,
) -> (AppState, Arc<happyview_source_jetstream::JetstreamSource>) {
    let mut state = pipeline_state(mock, &[POST]).await;
    let source = crate::host::attach_jetstream(&mut state, Default::default());
    (state, source)
}

async fn until_backfill_running(state: &AppState, collection: &str, running: bool) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while state.source.backfill_running(collection).await != running {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("backfill_running({collection}) never became {running}"));
}

#[tokio::test]
async fn a_job_runs_for_backfill_running_while_its_run_goes_on() {
    let mock = MockServer::start().await;
    let (state, source) = running_source(&mock).await;
    mount_slow_relay_page(&mock, POST, "did:plc:slowrun", Duration::from_millis(800)).await;
    mount_plc(&mock).await;
    mount_records(&mock, "did:plc:slowrun", POST, 1).await;
    let jobs = source.jobs().expect("attached");

    assert!(!state.source.backfill_running(POST).await);
    let job_id = create_backfill_job(&jobs, Some(POST), &[])
        .await
        .expect("create job");
    let run = tokio::spawn(run_backfill_job_with(
        jobs.clone(),
        job_id.clone(),
        DEFAULT_DISCOVERY_WINDOW,
    ));
    until_backfill_running(&state, POST, true).await;
    assert!(
        !state.source.backfill_running(LIKE).await,
        "the job covers its own collection only"
    );
    tokio::time::timeout(Duration::from_secs(60), run)
        .await
        .expect("the job should finish")
        .expect("the job should not panic");
    assert_eq!(job_row(&state, &job_id).await.status, "completed");
    assert!(!state.source.backfill_running(POST).await);
}

#[tokio::test]
async fn a_resumed_job_runs_for_backfill_running_from_the_resume() {
    let mock = MockServer::start().await;
    let (state, source) = running_source(&mock).await;
    mount_slow_relay_page(
        &mock,
        POST,
        "did:plc:slowresume",
        Duration::from_millis(800),
    )
    .await;
    mount_plc(&mock).await;
    mount_records(&mock, "did:plc:slowresume", POST, 1).await;
    seed_job(
        &state,
        "resume-running",
        Some(POST),
        "network",
        "pending",
        2,
        false,
    )
    .await;
    let jobs = source.jobs().expect("attached");

    assert!(!state.source.backfill_running(POST).await);
    resume_backfill_jobs(&jobs).await;
    // Running from the resume itself, before its task has run at all.
    assert!(state.source.backfill_running(POST).await);
    until_backfill_running(&state, POST, false).await;
    assert_eq!(job_row(&state, "resume-running").await.status, "completed");
}

/// A started job is running by the time `start_backfill` returns, before
/// its task has run at all, and stops running once it completes.
#[tokio::test]
async fn a_started_job_runs_for_backfill_running_before_start_returns() {
    let mock = MockServer::start().await;
    let (state, _source) = running_source(&mock).await;

    for _ in 0..20 {
        mount_slow_relay_page(&mock, POST, "did:plc:slowstart", Duration::from_millis(200)).await;
        mount_plc(&mock).await;
        mount_records(&mock, "did:plc:slowstart", POST, 1).await;
        assert!(!state.source.backfill_running(POST).await);
        let job_id = state
            .source
            .start_backfill(happyview_provider::BackfillRequest {
                collection: Some(POST.to_string()),
                dids: Vec::new(),
                source: Some("crawl".to_string()),
                actor: "did:plc:testadmin".to_string(),
                kind: happyview_provider::BackfillKind::Manual,
            })
            .await
            .expect("start a backfill")
            .expect("a job id");
        assert!(state.source.backfill_running(POST).await);
        until_backfill_running(&state, POST, false).await;
        assert_eq!(job_row(&state, &job_id).await.status, "completed");
        mock.verify().await;
        mock.reset().await;
    }

    // A start that is refused leaves nothing running.
    let refused = state
        .source
        .start_backfill(happyview_provider::BackfillRequest {
            collection: Some(POST.to_string()),
            dids: Vec::new(),
            source: Some("nowhere".to_string()),
            actor: "did:plc:testadmin".to_string(),
            kind: happyview_provider::BackfillKind::Manual,
        })
        .await;
    assert!(refused.is_err());
    assert!(!state.source.backfill_running(POST).await);
}
