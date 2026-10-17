//! Composing the Jetstream source into another provider: a live filter that
//! keeps collections out of the stream, and backfill executors that run a
//! collection's backfills in place of a crawl or a replay.
//!
//! They need a host and the job tables, which the source crate cannot build,
//! so they live here. Needs `TEST_DATABASE_URL`; serial, because the tests
//! share one database.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use happyview::AppState;
use happyview::db::adapt_sql;
use happyview::host::AppHost;
use happyview::lexicon::ProcedureAction;
use happyview_provider::{
    Action, BackfillKind, BackfillRequest, Delta, Origin, RecordEvent, RecordKey, RecordSource,
    RecordStorage, SourceEvent,
};
use happyview_source_jetstream::archive::ArchiveConfig;
use happyview_source_jetstream::consumer::config::{
    DRAIN_TIMEOUT, ProtocolChoice, Settings, parse_endpoint,
};
use happyview_source_jetstream::{BackfillExecutor, JetstreamSource, JobHandle, JobTotals};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tokio::sync::{Notify, watch};
use tower::ServiceExt;

use common::app::TestApp;
use common::fake_storage::FakeStorage;
use common::jetstream::{self as js, MockJetstream};

const A: &str = "app.test.a";
const B: &str = "app.test.b";
const C: &str = "app.test.c";
const DID: &str = "did:plc:composition";

/// Poll `$done` (an expression, re-evaluated) until it holds or two minutes
/// pass.
macro_rules! wait_until {
    ($what:expr, $done:expr) => {{
        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
        while !$done {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {}",
                $what
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }};
}

fn record_lexicon(id: &str) -> Value {
    json!({
        "lexicon": 1,
        "id": id,
        "defs": {"main": {"type": "record", "key": "tid",
            "record": {"type": "object", "properties": {"text": {"type": "string"}}}}}
    })
}

/// The app with record lexicons for `collections`, and the tracked list a
/// host over the returned state reads, which the test drives.
async fn app(collections: &[&str]) -> Option<(TestApp, watch::Sender<Vec<String>>)> {
    if std::env::var("TEST_DATABASE_URL").is_err() {
        eprintln!("skipped (TEST_DATABASE_URL not set)");
        return None;
    }
    let mut app = TestApp::new().await;
    for collection in collections {
        app.state
            .lexicons
            .put_manual(
                &app.state.db,
                app.state.db_backend,
                happyview::catalog::LexiconWrite {
                    lexicon_json: record_lexicon(collection),
                    backfill: false,
                    target_collection: Some(collection.to_string()),
                    action: ProcedureAction::Upsert,
                    token_cost: None,
                    authority_did: None,
                },
            )
            .await
            .expect("store a lexicon");
    }
    let (tx, _) = watch::channel(collections.iter().map(|c| c.to_string()).collect());
    app.state.collections_tx = tx.clone();
    Some((app, tx))
}

fn settings(mock: &MockJetstream) -> Settings {
    Settings {
        endpoint: parse_endpoint(&mock.url()),
        // v1, so the filter is the `wantedCollections` parameter.
        protocol: ProtocolChoice::V1,
        compression: false,
        workers: 2,
        invalid_request_retry: Duration::from_millis(100),
        drain_timeout: DRAIN_TIMEOUT,
    }
}

/// Give `source` a host over `state`.
fn attach(state: &AppState, source: &JetstreamSource) {
    source.attach(AppHost::new(state.clone()));
}

/// [`attach`], and a router over `state` serving the source's admin routes.
fn attach_routed(state: &AppState, source: Arc<JetstreamSource>) -> axum::Router {
    attach(state, &source);
    let mut routed = state.clone();
    routed.source = source;
    happyview::server::router(routed)
}

fn request(app: &TestApp, method: &str, path: &str) -> Request<Body> {
    let (name, value) = app.admin_cookie();
    Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1")
        .header(name, value)
        .body(Body::empty())
        .unwrap()
}

async fn send(app: &TestApp, router: &axum::Router, method: &str, path: &str) -> (u16, Value) {
    let response = router
        .clone()
        .oneshot(request(app, method, path))
        .await
        .expect("serve the request");
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn start_backfill(source: &JetstreamSource, collection: &str) -> String {
    source
        .start_backfill(BackfillRequest {
            collection: Some(collection.to_string()),
            dids: Vec::new(),
            source: None,
            actor: "did:plc:testadmin".to_string(),
            kind: BackfillKind::Manual,
        })
        .await
        .expect("start a backfill")
        .expect("a job id")
}

#[derive(Debug)]
struct JobRow {
    status: String,
    source: String,
    error: Option<String>,
    processed_repos: Option<i32>,
    total_repos: Option<i32>,
    total_records: Option<i32>,
}

async fn job(app: &TestApp, id: &str) -> JobRow {
    let sql = adapt_sql(
        "SELECT status, source, error, processed_repos, total_repos, total_records FROM happyview_backfill_jobs WHERE id = ?",
        app.state.db_backend,
    );
    #[allow(clippy::type_complexity)]
    let row: (
        String,
        String,
        Option<String>,
        Option<i32>,
        Option<i32>,
        Option<i32>,
    ) = happyview::db::query_as(&sql)
        .bind(id)
        .fetch_one(&app.state.db)
        .await
        .expect("read the job");
    JobRow {
        status: row.0,
        source: row.1,
        error: row.2,
        processed_repos: row.3,
        total_repos: row.4,
        total_records: row.5,
    }
}

async fn local_rows(app: &TestApp) -> i64 {
    happyview::db::query_scalar::<i64>("SELECT COUNT(*) FROM happyview_records_v2")
        .fetch_one(&app.state.db)
        .await
        .expect("count local rows")
}

// ---------------------------------------------------------------------------
// The live filter
// ---------------------------------------------------------------------------

/// A live source over `state` streaming from `mock` by `filter`.
async fn start_live(
    state: &AppState,
    mock: &MockJetstream,
    filter: impl Fn(&str) -> bool + Send + Sync + 'static,
) -> Arc<JetstreamSource> {
    let source = JetstreamSource::builder()
        .settings(settings(mock))
        .live_filter(filter)
        .build();
    source
        .clone()
        .start(AppHost::new(state.clone()))
        .await
        .expect("start the source");
    source
}

#[tokio::test]
#[serial]
async fn live_filter_excludes_collections_from_the_stream() {
    let Some((app, tracked)) = app(&[A, B, C]).await else {
        return;
    };
    tracked.send_replace(vec![A.to_string(), B.to_string()]);
    let mock = MockJetstream::start().await;
    let source = start_live(&app.state, &mock, |c| c != B).await;

    wait_until!("a connection", !mock.connections().is_empty());
    let wanted = mock.connections()[0].params("wantedCollections");
    assert!(wanted.contains(&A.to_string()), "{wanted:?}");
    assert!(!wanted.contains(&B.to_string()), "{wanted:?}");

    // Asked again when the tracked collections change.
    tracked.send_replace(vec![A.to_string(), B.to_string(), C.to_string()]);
    wait_until!("a reconnect", mock.connections().len() >= 2);
    let wanted = mock
        .connections()
        .last()
        .unwrap()
        .params("wantedCollections");
    assert!(wanted.contains(&A.to_string()), "{wanted:?}");
    assert!(wanted.contains(&C.to_string()), "{wanted:?}");
    assert!(!wanted.contains(&B.to_string()), "{wanted:?}");

    source.stop();
}

#[tokio::test]
#[serial]
async fn a_live_filter_keeps_a_claimed_collection_out_of_the_stream() {
    let Some((mut app, _tracked)) = app(&[A, B]).await else {
        return;
    };
    let storage = Arc::new(FakeStorage::claiming(&[B]).await);
    app.state.records = app.state.records.clone().with_external(storage.clone());
    let mock = MockJetstream::start().await;
    let claims = storage.clone();
    let source = start_live(&app.state, &mock, move |c| !claims.claims(c)).await;

    wait_until!("a connection", !mock.connections().is_empty());
    let wanted = mock.connections()[0].params("wantedCollections");
    assert!(wanted.contains(&A.to_string()), "{wanted:?}");
    assert!(!wanted.contains(&B.to_string()), "{wanted:?}");

    source.stop();
}

// ---------------------------------------------------------------------------
// Executors
// ---------------------------------------------------------------------------

/// Counts its runs; reports progress once `go` is notified.
struct Counting {
    calls: AtomicUsize,
    go: Notify,
}

#[async_trait::async_trait]
impl BackfillExecutor for Counting {
    fn name(&self) -> &'static str {
        "store-pass"
    }

    async fn run(&self, job: JobHandle, collection: &str) -> Result<JobTotals, String> {
        assert_eq!(collection, B);
        self.calls.fetch_add(1, Ordering::SeqCst);
        job.stage("storing").await;
        self.go.notified().await;
        job.progress(3, Some(5)).await;
        Ok(JobTotals { records: 5 })
    }
}

/// The `data` of each SSE event in `body` until `done` accepts one.
async fn read_events_until(body: Body, done: impl Fn(&Value) -> bool) -> Vec<Value> {
    let mut body = body;
    let mut buffer = String::new();
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let frame = tokio::time::timeout_at(deadline, body.frame())
            .await
            .expect("an event in time")
            .expect("the stream stays open")
            .expect("a frame");
        let Ok(data) = frame.into_data() else {
            continue;
        };
        buffer.push_str(std::str::from_utf8(&data).unwrap());
        while let Some(end) = buffer.find("\n\n") {
            let message: String = buffer.drain(..end + 2).collect();
            for line in message.lines() {
                if let Some(data) = line.strip_prefix("data:") {
                    let event: Value = serde_json::from_str(data.trim()).unwrap();
                    let finished = done(&event);
                    events.push(event);
                    if finished {
                        return events;
                    }
                }
            }
        }
    }
}

#[tokio::test]
#[serial]
async fn an_executor_runs_backfills_for_its_collections() {
    let Some((app, _tracked)) = app(&[A, B]).await else {
        return;
    };
    let executor = Arc::new(Counting {
        calls: AtomicUsize::new(0),
        go: Notify::new(),
    });
    let routed = executor.clone();
    let source = JetstreamSource::builder()
        .executor_for(move |c| (c == B).then(|| routed.clone() as Arc<dyn BackfillExecutor>))
        .build_detached();
    let router = attach_routed(&app.state, source.clone());

    let id = start_backfill(&source, B).await;
    assert_eq!(job(&app, &id).await.source, "store-pass");

    // Its progress reaches the job's event stream.
    let response = router
        .clone()
        .oneshot(request(
            &app,
            "GET",
            &format!("/admin/backfill/{id}/events"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let body = response.into_body();
    let reader = tokio::spawn(read_events_until(body, |e| e["type"] == "job_counters"));
    // The handler subscribed to the job's events before it answered, and the
    // executor reports only after `go`, so the counters can't be missed.
    executor.go.notify_one();
    let events = reader.await.unwrap();
    assert_eq!(events[0]["type"], "job_snapshot", "{events:?}");
    let counters = events.last().unwrap();
    assert_eq!(counters["job_id"], id);
    assert_eq!(counters["processed_repos"], 3);
    assert_eq!(counters["total_repos"], 5);

    wait_until!(
        "the job to complete",
        job(&app, &id).await.status == "completed"
    );
    let row = job(&app, &id).await;
    assert_eq!(row.processed_repos, Some(3));
    assert_eq!(row.total_repos, Some(5));
    assert_eq!(row.total_records, Some(5));
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);

    // A collection with no executor still crawls.
    let crawl = start_backfill(&source, A).await;
    assert_eq!(job(&app, &crawl).await.source, "crawl");
    wait_until!(
        "the crawl to finish",
        job(&app, &crawl).await.status != "running"
    );
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
}

/// Checkpoints, then waits to be stopped; run again, finishes from its
/// checkpoint.
struct Checkpointing {
    resumed_from: Mutex<Vec<Option<String>>>,
}

#[async_trait::async_trait]
impl BackfillExecutor for Checkpointing {
    fn name(&self) -> &'static str {
        "store-pass"
    }

    async fn run(&self, job: JobHandle, _collection: &str) -> Result<JobTotals, String> {
        let from = job.resume_from().await;
        self.resumed_from.lock().unwrap().push(from.clone());
        if from.is_some() {
            job.progress(2, Some(2)).await;
            return Ok(JobTotals { records: 2 });
        }
        job.checkpoint("page-2").await;
        job.progress(1, Some(2)).await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        while !job.should_stop().await {
            if tokio::time::Instant::now() > deadline {
                return Err("never asked to stop".into());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Ok(JobTotals { records: 1 })
    }
}

#[tokio::test]
#[serial]
async fn a_paused_executor_job_resumes_from_its_checkpoint() {
    let Some((app, _tracked)) = app(&[B]).await else {
        return;
    };
    let executor = Arc::new(Checkpointing {
        resumed_from: Mutex::new(Vec::new()),
    });
    let routed = executor.clone();
    let source = JetstreamSource::builder()
        .executor_for(move |c| (c == B).then(|| routed.clone() as Arc<dyn BackfillExecutor>))
        .build_detached();
    let router = attach_routed(&app.state, source.clone());

    let id = start_backfill(&source, B).await;
    wait_until!(
        "the checkpoint",
        executor.resumed_from.lock().unwrap().len() == 1
            && job(&app, &id).await.processed_repos == Some(1)
    );

    let (status, body) = send(
        &app,
        &router,
        "POST",
        &format!("/admin/backfill/{id}/pause"),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    wait_until!("the pause", job(&app, &id).await.status == "paused");

    let (status, body) = send(
        &app,
        &router,
        "POST",
        &format!("/admin/backfill/{id}/resume"),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    wait_until!(
        "the job to complete",
        job(&app, &id).await.status == "completed"
    );

    assert_eq!(
        *executor.resumed_from.lock().unwrap(),
        vec![None, Some("page-2".to_string())]
    );
    let row = job(&app, &id).await;
    assert_eq!(row.processed_repos, Some(2));
    // Both runs' records: 1 before the pause, 2 after.
    assert_eq!(row.total_records, Some(3));
    assert_eq!(row.error, None);
}

/// Backfills from its own store: hands HappyView events it already holds.
struct StorePass;

#[async_trait::async_trait]
impl BackfillExecutor for StorePass {
    fn name(&self) -> &'static str {
        "store-pass"
    }

    async fn run(&self, job: JobHandle, collection: &str) -> Result<JobTotals, String> {
        let events = (1..=2)
            .map(|n| {
                let body = json!({"$type": collection, "text": format!("stored {n}")});
                SourceEvent::Record(RecordEvent {
                    key: RecordKey {
                        did: DID.to_string(),
                        collection: collection.to_string(),
                        rkey: format!("r{n}"),
                    },
                    action: Action::Create,
                    cid: Some(
                        happyview::cid_verify::compute_record_cid(&body)
                            .unwrap()
                            .to_string(),
                    ),
                    rev: Some(rev(n)),
                    body: Some(body),
                    time_us: None,
                    origin: Origin::Backfill {
                        job_id: job.id().to_string(),
                    },
                    stored: true,
                })
            })
            .collect();
        let batch = job.host().ingest_batch(events).await;
        let records = batch.changed.iter().filter(|changed| **changed).count() as i64;
        job.progress(records, Some(records)).await;
        Ok(JobTotals { records })
    }
}

fn rev(n: i64) -> String {
    happyview::tid::tid_from_unix_microseconds(1_700_000_000_000_000 + n)
}

#[tokio::test]
#[serial]
async fn an_executor_for_a_claimed_collection_lands_its_stored_events_as_deltas() {
    let Some((mut app, _tracked)) = app(&[A, B]).await else {
        return;
    };
    let storage = Arc::new(FakeStorage::claiming(&[B]).await);
    app.state.records = app.state.records.clone().with_external(storage.clone());
    let claims = storage.clone();
    let source = JetstreamSource::builder()
        .live_filter({
            let claims = storage.clone();
            move |c| !claims.claims(c)
        })
        .executor_for(move |c| {
            claims
                .claims(c)
                .then(|| Arc::new(StorePass) as Arc<dyn BackfillExecutor>)
        })
        .build_detached();
    attach(&app.state, &source);

    let id = start_backfill(&source, B).await;
    wait_until!(
        "the job to complete",
        job(&app, &id).await.status == "completed"
    );

    let key = |rkey: &str| RecordKey {
        did: DID.to_string(),
        collection: B.to_string(),
        rkey: rkey.to_string(),
    };
    assert_eq!(
        storage.deltas(),
        vec![
            (key("r1"), Delta::Proceed, Some(rev(1))),
            (key("r2"), Delta::Proceed, Some(rev(2))),
        ]
    );
    assert_eq!(local_rows(&app).await, 0);
    let row = job(&app, &id).await;
    assert_eq!(row.source, "store-pass");
    assert_eq!(row.total_records, Some(2));
}

// ---------------------------------------------------------------------------
// Executor jobs among the others
// ---------------------------------------------------------------------------

async fn seed_executor_job(app: &TestApp, id: &str, status: &str) {
    let sql = adapt_sql(
        "INSERT INTO happyview_backfill_jobs \
         (id, collection, did, scope, status, stage, total_repos, processed_repos, total_records, queue_version, discovery_complete, started_at, created_at, source) \
         VALUES (?, ?, NULL, 'network', ?, 'storing', 5, 3, 7, 2, 1, '2026-10-16T00:00:00Z', '2026-10-16T00:00:00Z', 'store-pass')",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(id)
        .bind(B)
        .bind(status)
        .execute(&app.state.db)
        .await
        .expect("seed an executor job");
}

#[tokio::test]
#[serial]
async fn an_executor_job_lists_in_the_admin_json() {
    let Some((app, _tracked)) = app(&[B]).await else {
        return;
    };
    seed_executor_job(&app, "job-executor", "paused").await;

    // The body as served: the only job, byte for byte.
    let response = app
        .router
        .clone()
        .oneshot(request(&app, "GET", "/admin/backfill/status"))
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        std::str::from_utf8(&bytes).unwrap(),
        concat!(
            "[",
            r#"{"id":"job-executor","collection":"app.test.b","did":null,"scope":"network","#,
            r#""status":"paused","stage":"storing","total_repos":5,"resolved_repos":null,"#,
            r#""processed_repos":3,"total_records":7,"error":null,"#,
            r#""started_at":"2026-10-16T00:00:00Z","completed_at":null,"#,
            r#""created_at":"2026-10-16T00:00:00Z","discovery_complete":true,"#,
            r#""recent_completions_limit":1000,"source":"store-pass","replay":null}"#,
            "]",
        )
    );
}

#[tokio::test]
#[serial]
async fn an_executor_job_without_its_executor_fails_rather_than_crawls() {
    let Some((app, _tracked)) = app(&[B]).await else {
        return;
    };
    seed_executor_job(&app, "job-orphaned", "running").await;
    seed_executor_job(&app, "job-retried", "completed").await;

    // A source with no executors.
    let source = JetstreamSource::detached();
    attach(&app.state, &source);
    let jobs = source.jobs().expect("an attached source");
    happyview_source_jetstream::backfill::resume_backfill_jobs(&jobs).await;
    wait_until!(
        "the job to fail",
        job(&app, "job-orphaned").await.status == "failed"
    );
    let row = job(&app, "job-orphaned").await;
    assert_eq!(
        row.error.as_deref(),
        Some(
            "backfill executor 'store-pass' is not configured for app.test.b; start a new backfill"
        )
    );
    // Nothing was crawled: no units, no counters moved.
    let units: i64 = happyview::db::query_scalar(&adapt_sql(
        "SELECT COUNT(*) FROM happyview_backfill_queue WHERE job_id = ?",
        app.state.db_backend,
    ))
    .bind("job-orphaned")
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(units, 0);
    assert_eq!(row.processed_repos, Some(3));
    assert_eq!(row.source, "store-pass");

    // Its failures are its executor's to retry, never a crawl's.
    let (status, body) = app
        .post_json_status(
            "/admin/backfill/job-retried/retry-failed",
            json!({"kinds": ["other"]}),
        )
        .await;
    assert_eq!(status, 400, "{body}");
}

#[tokio::test]
#[serial]
async fn a_gap_is_replayed_only_for_the_collections_streamed_live() {
    let Some((app, _tracked)) = app(&[A, B]).await else {
        return;
    };
    let mock = MockJetstream::start().await;
    mock.seal(
        vec![
            js::archive_create(1, DID, A, "a1", json!({"$type": A, "text": "a"})),
            js::archive_create(2, DID, B, "b1", json!({"$type": B, "text": "b"})),
            js::archive_create(3, DID, A, "a2", json!({"$type": A, "text": "a"})),
        ],
        3,
    );
    let source = JetstreamSource::builder()
        .settings(settings(&mock))
        .archive(ArchiveConfig {
            download_dir: std::env::temp_dir().join(format!("hv-compose-{}", uuid::Uuid::new_v4())),
            retry_unit: Duration::from_millis(5),
            ..ArchiveConfig::default()
        })
        .live_filter(|c| c != B)
        .build_detached();
    attach(&app.state, &source);

    let id = source
        .start_backfill(BackfillRequest {
            collection: None,
            dids: Vec::new(),
            source: None,
            actor: "did:plc:testadmin".to_string(),
            kind: BackfillKind::Gap { after_seq: 1 },
        })
        .await
        .expect("start a gap replay")
        .expect("a job id");
    wait_until!(
        "the gap job to finish",
        job(&app, &id).await.status != "running"
    );

    // Commit plans name collections; marker plans don't.
    let plans: Vec<Value> = mock
        .plan_requests()
        .into_iter()
        .filter(|p| p.get("collections").is_some())
        .collect();
    assert!(!plans.is_empty());
    for plan in &plans {
        let collections = plan["collections"].as_array().unwrap();
        assert!(collections.contains(&json!(A)), "{plan}");
        assert!(!collections.contains(&json!(B)), "{plan}");
    }
}

/// Never expected to run.
struct Unused {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl BackfillExecutor for Unused {
    fn name(&self) -> &'static str {
        "store-pass"
    }

    async fn run(&self, _job: JobHandle, _collection: &str) -> Result<JobTotals, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(JobTotals::default())
    }
}

#[tokio::test]
#[serial]
async fn a_gap_replays_a_streamed_collection_that_has_an_executor() {
    let Some((app, _tracked)) = app(&[A, B]).await else {
        return;
    };
    let mock = MockJetstream::start().await;
    mock.seal(
        vec![
            js::archive_create(1, DID, A, "a1", json!({"$type": A, "text": "a"})),
            js::archive_create(2, DID, B, "b1", json!({"$type": B, "text": "b"})),
            js::archive_create(3, DID, A, "a2", json!({"$type": A, "text": "a"})),
        ],
        3,
    );
    let executor = Arc::new(Unused {
        calls: AtomicUsize::new(0),
    });
    let routed = executor.clone();
    // `b` is streamed live and has an executor.
    let source = JetstreamSource::builder()
        .settings(settings(&mock))
        .archive(ArchiveConfig {
            download_dir: std::env::temp_dir().join(format!("hv-compose-{}", uuid::Uuid::new_v4())),
            retry_unit: Duration::from_millis(5),
            ..ArchiveConfig::default()
        })
        .executor_for(move |c| (c == B).then(|| routed.clone() as Arc<dyn BackfillExecutor>))
        .build_detached();
    attach(&app.state, &source);

    let id = source
        .start_backfill(BackfillRequest {
            collection: None,
            dids: Vec::new(),
            source: None,
            actor: "did:plc:testadmin".to_string(),
            kind: BackfillKind::Gap { after_seq: 1 },
        })
        .await
        .expect("start a gap replay")
        .expect("a job id");
    wait_until!(
        "the gap job to finish",
        job(&app, &id).await.status != "running"
    );
    assert_eq!(job(&app, &id).await.status, "completed");

    let plans: Vec<Value> = mock
        .plan_requests()
        .into_iter()
        .filter(|p| p.get("collections").is_some())
        .collect();
    assert!(!plans.is_empty());
    for plan in &plans {
        let collections = plan["collections"].as_array().unwrap();
        assert!(collections.contains(&json!(A)), "{plan}");
        assert!(collections.contains(&json!(B)), "{plan}");
    }
    // Seqs 2 (`b`) and 3 (`a`), both replayed.
    assert_eq!(local_rows(&app).await, 2);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
#[serial]
async fn an_account_backfill_leaves_executor_collections_out() {
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, ResponseTemplate};

    let Some((mut app, _tracked)) = app(&[A, B]).await else {
        return;
    };
    let storage = Arc::new(FakeStorage::claiming(&[B]).await);
    app.state.records = app.state.records.clone().with_external(storage.clone());
    let executor = Arc::new(Unused {
        calls: AtomicUsize::new(0),
    });
    let routed = executor.clone();
    let claims = storage.clone();
    let source = JetstreamSource::builder()
        .executor_for(move |c| {
            claims
                .claims(c)
                .then(|| routed.clone() as Arc<dyn BackfillExecutor>)
        })
        .build_detached();
    attach(&app.state, &source);

    let pds = app.mock_server.uri();
    Mock::given(method("GET"))
        .and(path(format!("/{DID}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(common::fixtures::did_document(DID, &pds)),
        )
        .mount(&app.mock_server)
        .await;
    let value = json!({"$type": A, "text": "crawled"});
    let cid = happyview::cid_verify::compute_record_cid(&value)
        .unwrap()
        .to_string();
    Mock::given(method("GET"))
        .and(path("/xrpc/com.atproto.repo.listRecords"))
        .and(query_param("repo", DID))
        .and(query_param("collection", A))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "records": [{"uri": format!("at://{DID}/{A}/one"), "cid": cid, "value": value}]
        })))
        .mount(&app.mock_server)
        .await;
    // The executor's collection is never fetched.
    Mock::given(method("GET"))
        .and(path("/xrpc/com.atproto.repo.listRecords"))
        .and(query_param("collection", B))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&app.mock_server)
        .await;

    let id = source
        .start_backfill(BackfillRequest {
            collection: None,
            dids: vec![DID.to_string()],
            source: None,
            actor: "did:plc:testadmin".to_string(),
            kind: BackfillKind::Manual,
        })
        .await
        .expect("start an account backfill")
        .expect("a job id");
    wait_until!(
        "the job to finish",
        job(&app, &id).await.status != "running"
    );
    let row = job(&app, &id).await;
    assert_eq!(row.status, "completed", "{row:?}");
    assert_eq!(row.source, "crawl");
    assert_eq!(local_rows(&app).await, 1);

    // Nothing reached the claiming storage, refused or otherwise.
    assert!(storage.deltas().is_empty(), "{:?}", storage.deltas());
    let refused: i64 = happyview::db::query_scalar(&adapt_sql(
        "SELECT COUNT(*) FROM happyview_event_logs WHERE event_type = 'record.unstored_claimed'",
        app.state.db_backend,
    ))
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(refused, 0);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
    let jobs: i64 = happyview::db::query_scalar("SELECT COUNT(*) FROM happyview_backfill_jobs")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(jobs, 1, "no executor job beside an account backfill");
}

struct Panicking;

#[async_trait::async_trait]
impl BackfillExecutor for Panicking {
    fn name(&self) -> &'static str {
        "store-pass"
    }

    async fn run(&self, _job: JobHandle, _collection: &str) -> Result<JobTotals, String> {
        panic!("the executor fell over");
    }
}

#[tokio::test]
#[serial]
async fn a_panicking_executor_fails_its_job() {
    let Some((app, _tracked)) = app(&[B]).await else {
        return;
    };
    let source = JetstreamSource::builder()
        .executor_for(|c| (c == B).then(|| Arc::new(Panicking) as Arc<dyn BackfillExecutor>))
        .build_detached();
    attach(&app.state, &source);

    let id = start_backfill(&source, B).await;
    wait_until!("the job to fail", job(&app, &id).await.status != "running");
    let row = job(&app, &id).await;
    assert_eq!(row.status, "failed");
    assert_eq!(
        row.error.as_deref(),
        Some("backfill executor 'store-pass' panicked")
    );
    assert!(!source.backfill_running(B).await);
}
