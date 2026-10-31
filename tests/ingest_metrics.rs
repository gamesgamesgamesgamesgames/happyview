//! What a record costs on its way through ingest, read back from the admin
//! routes, and a backfill job's hook stats from its first record to the row
//! it ends with.
//!
//! The `interpreter_echo` fixture claims `lua`, so a `record.index:` row runs
//! through the real dispatch. Its source is a directive: `value:other` waves
//! the record through, `value:none` skips it, and `error:timeout` fails as a
//! script out of time would. Needs `TEST_DATABASE_URL`; serial, because the
//! tests share one database.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use happyview::AppState;
use happyview::db::adapt_sql;
use happyview::host::AppHost;
use happyview::lexicon::ProcedureAction;
use happyview_provider::{
    Action, BackfillKind, BackfillRequest, Origin, RecordEvent, RecordKey, RecordSource,
    SourceEvent,
};
use happyview_source_jetstream::{BackfillExecutor, JetstreamSource, JobHandle, JobTotals};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tokio::sync::Notify;
use tower::ServiceExt;

use common::app::TestApp;
use common::echo_interpreter;
use common::fake_storage::FakeStorage;

/// Stored here, hooked to wave records through.
const LOCAL: &str = "app.test.metrics.local";
/// Stored here, hooked by a script that runs out of time.
const FAILING: &str = "app.test.metrics.failing";
/// Claimed by an external storage, hooked to skip; its executor's job is the
/// one under test.
const CLAIMED: &str = "app.test.metrics.claimed";
/// Claimed too; its executor only reports stages of its own.
const OBSERVED: &str = "app.test.metrics.observed";
const DID: &str = "did:plc:ingestmetrics";

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

async fn app() -> Option<TestApp> {
    if std::env::var("TEST_DATABASE_URL").is_err() {
        happyview::test_support::unavailable(happyview::test_support::NO_DATABASE);
        return None;
    }
    if !echo_interpreter::is_built() {
        happyview::test_support::unavailable(echo_interpreter::BUILD);
        return None;
    }
    let mut app = TestApp::new().await;
    let collections = [LOCAL, FAILING, CLAIMED, OBSERVED];
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
                    indexed: true,
                },
            )
            .await
            .expect("store a lexicon");
    }
    let (tx, _) = tokio::sync::watch::channel(collections.iter().map(|c| c.to_string()).collect());
    app.state.collections_tx = tx;
    app.state
        .plugin_registry
        .register(echo_interpreter::plugin("lua"))
        .await;
    for (collection, body) in [
        (LOCAL, "value:other"),
        (FAILING, "error:timeout"),
        (CLAIMED, "value:none"),
    ] {
        seed_hook(&app, collection, body).await;
    }
    let storage = Arc::new(FakeStorage::claiming(&[CLAIMED, OBSERVED]).await);
    app.state.records = app.state.records.clone().with_external(storage);
    Some(app)
}

async fn seed_hook(app: &TestApp, collection: &str, body: &str) {
    let id = format!("record.index:{collection}");
    happyview::db::query(&adapt_sql(
        "DELETE FROM happyview_scripts WHERE id = ?",
        app.state.db_backend,
    ))
    .bind(&id)
    .execute(&app.state.db)
    .await
    .unwrap();
    let now = happyview::db::now_rfc3339();
    happyview::db::query(&adapt_sql(
        "INSERT INTO happyview_scripts (id, body, script_type, created_at, updated_at) \
         VALUES (?, ?, 'lua', ?, ?)",
        app.state.db_backend,
    ))
    .bind(&id)
    .bind(body)
    .bind(&now)
    .bind(&now)
    .execute(&app.state.db)
    .await
    .expect("seed a hook");
}

/// Give `source` a host over `state`, and a router serving both HappyView's
/// admin routes and the source's.
fn attach_routed(state: &AppState, source: Arc<JetstreamSource>) -> axum::Router {
    source.attach(AppHost::new(state.clone()));
    let mut routed = state.clone();
    routed.source = source;
    happyview::server::router(routed)
}

fn request(app: &TestApp, path: &str) -> Request<Body> {
    let (name, value) = app.admin_cookie();
    Request::builder()
        .uri(path)
        .header("host", "127.0.0.1")
        .header(name, value)
        .body(Body::empty())
        .unwrap()
}

async fn get(app: &TestApp, router: &axum::Router, path: &str) -> Value {
    let response = router
        .clone()
        .oneshot(request(app, path))
        .await
        .expect("serve the request");
    assert_eq!(response.status().as_u16(), 200, "{path}");
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
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

async fn job_row(app: &TestApp, id: &str) -> (String, Option<String>) {
    happyview::db::query_as(&adapt_sql(
        "SELECT status, hook_stats FROM happyview_backfill_jobs WHERE id = ?",
        app.state.db_backend,
    ))
    .bind(id)
    .fetch_one(&app.state.db)
    .await
    .expect("read the job")
}

fn event(job: &JobHandle, collection: &str, rkey: &str, stored: bool) -> SourceEvent {
    let body = json!({"$type": collection, "text": rkey});
    SourceEvent::Record(RecordEvent {
        key: RecordKey {
            did: DID.to_string(),
            collection: collection.to_string(),
            rkey: rkey.to_string(),
        },
        action: Action::Create,
        cid: Some(
            happyview::cid_verify::compute_record_cid(&body)
                .unwrap()
                .to_string(),
        ),
        rev: None,
        body: Some(body),
        time_us: None,
        origin: Origin::Backfill {
            job_id: job.id().to_string(),
        },
        stored,
    })
}

/// Hands HappyView records of three collections once `go` is notified: three
/// it stores and waves through, one whose hook times out, and two of its own
/// collection that it holds and the hook skips.
struct Delivering {
    go: Notify,
}

#[async_trait::async_trait]
impl BackfillExecutor for Delivering {
    fn name(&self) -> &'static str {
        "store-pass"
    }

    async fn run(&self, job: JobHandle, collection: &str) -> Result<JobTotals, String> {
        self.go.notified().await;
        let local = (1..=3)
            .map(|n| event(&job, LOCAL, &format!("l{n}"), false))
            .collect();
        job.host().ingest_batch(local).await;
        job.host()
            .ingest_batch(vec![event(&job, FAILING, "f1", false)])
            .await;
        let claimed = (1..=2)
            .map(|n| event(&job, collection, &format!("c{n}"), true))
            .collect();
        job.host().ingest_batch(claimed).await;
        job.progress(6, Some(6)).await;
        Ok(JobTotals { records: 6 })
    }
}

/// Reports a page read and its writes, two real and four skipped, and no
/// records at all.
struct Observing;

#[async_trait::async_trait]
impl BackfillExecutor for Observing {
    fn name(&self) -> &'static str {
        "store-pass"
    }

    async fn run(&self, job: JobHandle, _collection: &str) -> Result<JobTotals, String> {
        job.observe("page_read", Duration::from_millis(6), 6);
        job.observe("write", Duration::from_millis(30), 1);
        job.observe("write", Duration::from_millis(10), 1);
        job.observe("write_skipped", Duration::ZERO, 4);
        job.progress(6, Some(6)).await;
        Ok(JobTotals { records: 2 })
    }
}

/// The `data` of each SSE event in `body` until `done` accepts one.
async fn read_events_until(body: Body, done: impl Fn(&Value) -> bool) -> Vec<Value> {
    let mut body = body;
    let mut buffer = String::new();
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
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

fn row<'a>(listing: &'a Value, collection: &str) -> &'a Value {
    listing["collections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["collection"] == collection && row["action"] == "create")
        .unwrap_or_else(|| panic!("no row for {collection}: {listing:#}"))
}

#[tokio::test]
#[serial]
async fn a_backfills_records_are_measured_by_stage_and_kept_with_the_job() {
    let Some(app) = app().await else {
        return;
    };
    let executor = Arc::new(Delivering { go: Notify::new() });
    let routed = executor.clone();
    let source = JetstreamSource::builder()
        .executor_for(move |c| (c == CLAIMED).then(|| routed.clone() as Arc<dyn BackfillExecutor>))
        .build_detached();
    let router = attach_routed(&app.state, source.clone());

    let id = start_backfill(&source, CLAIMED).await;
    let response = router
        .clone()
        .oneshot(request(&app, &format!("/admin/backfill/{id}/events")))
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let reader = tokio::spawn(read_events_until(response.into_body(), |e| {
        e["type"] == "job_completed"
    }));
    executor.go.notify_one();
    let events = reader.await.unwrap();
    let hook_stats: Vec<&Value> = events
        .iter()
        .filter(|e| e["type"] == "hook_stats")
        .collect();
    let last = hook_stats
        .last()
        .unwrap_or_else(|| panic!("no hook_stats event: {events:#?}"));
    println!("The last hook_stats event:\n{last:#}");
    assert_eq!(last["job_id"], id);
    assert_eq!(last["done"], 6);
    assert_eq!(last["failed"], 1);
    assert_eq!(last["timeouts"], 1);
    assert_eq!(last["dead_lettered"], 1);
    assert_eq!(last["skipped"], 2);
    assert_eq!(last["outcomes"], json!({"proceed": 4, "skip": 2}));
    assert_eq!(last["eta_s"], 0.0);
    for stage in ["resolve", "cid_check", "hook", "write", "delta"] {
        assert!(
            last["stages"][stage]["n"].as_u64().unwrap() > 0,
            "{stage}: {last:#}"
        );
    }
    // The three waved through and the one that failed open were written
    // here; the two the storage holds were applied there.
    assert_eq!(last["stages"]["write"]["n"], 4);
    assert_eq!(last["stages"]["delta"]["n"], 2);
    assert!(last["script_share"].as_f64().unwrap() > 0.0);
    // Its final stats go out ahead of its completion.
    let completed = events.iter().position(|e| e["type"] == "job_completed");
    let final_stats = events.iter().rposition(|e| e["type"] == "hook_stats");
    assert!(final_stats < completed, "{events:#?}");

    wait_until!(
        "the job's hook stats to be kept",
        job_row(&app, &id).await.1.is_some()
    );
    let (status, kept) = job_row(&app, &id).await;
    assert_eq!(status, "completed");
    let kept: Value = serde_json::from_str(&kept.unwrap()).unwrap();
    println!("hook_stats kept with the finished job:\n{kept:#}");
    assert_eq!(kept["done"], 6);
    assert_eq!(kept["dead_lettered"], 1);

    let jobs = get(&app, &router, "/admin/backfill/status").await;
    let job = jobs
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["id"] == id)
        .expect("the job is listed");
    assert_eq!(job["hook_stats"]["done"], 6);
    // Still held in memory for the hour after its last record.
    assert_eq!(job["hooks"]["done"], 6);

    let ingest = get(&app, &router, "/admin/metrics/ingest?window=1h").await;
    println!("GET /admin/metrics/ingest?window=1h:\n{ingest:#}");
    assert_eq!(ingest["window"], "1h");
    assert!(ingest["scripts_table_reads"].as_u64().unwrap() > 0);
    let local = row(&ingest, LOCAL);
    assert_eq!(local["records"], 3);
    assert_eq!(local["outcomes"], json!({"proceed": 3}));
    for stage in ["resolve", "cid_check", "hook", "write"] {
        assert_eq!(local["stages"][stage]["n"], 3, "{stage}: {local:#}");
        assert!(local["stages"][stage]["ms_mean"].is_number());
    }
    assert!(local["record_ms"]["p95"].as_f64().unwrap() > 0.0);
    assert!(local["script_ms_mean"].as_f64().unwrap() > 0.0);
    for stage in ["hook.link", "hook.allocate", "hook.teardown"] {
        assert_eq!(local["stages"][stage]["n"], 3, "{stage}: {local:#}");
    }
    let script_share = local["script_share"].as_f64().unwrap();
    let platform_share = local["platform_share"].as_f64().unwrap();
    assert!((script_share + platform_share - 1.0).abs() < 1e-9);
    let failing = row(&ingest, FAILING);
    assert_eq!(
        (
            &failing["failed"],
            &failing["timeouts"],
            &failing["dead_lettered"]
        ),
        (&json!(1), &json!(1), &json!(1))
    );
    let claimed = row(&ingest, CLAIMED);
    assert_eq!(claimed["outcomes"], json!({"skip": 2}));
    assert_eq!(claimed["stages"]["delta"]["n"], 2);
    assert!(claimed["stages"].get("write").is_none(), "{claimed:#}");

    // A finished job is no longer in the live view.
    let live = get(&app, &router, "/admin/metrics/live").await;
    assert!(
        live["backfill_jobs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|j| j["job_id"] != id),
        "{live:#}"
    );
}

#[tokio::test]
#[serial]
async fn an_executors_own_stages_are_reported_and_skips_stay_out_of_the_mean() {
    let Some(app) = app().await else {
        return;
    };
    let source = JetstreamSource::builder()
        .executor_for(|c| (c == OBSERVED).then(|| Arc::new(Observing) as Arc<dyn BackfillExecutor>))
        .build_detached();
    let router = attach_routed(&app.state, source.clone());

    let id = start_backfill(&source, OBSERVED).await;
    wait_until!(
        "the job's hook stats to be kept",
        job_row(&app, &id).await.1.is_some()
    );
    let jobs = get(&app, &router, "/admin/backfill/status").await;
    let job = jobs
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["id"] == id)
        .expect("the job is listed");
    let stages = &job["hook_stats"]["stages"];
    println!("An observing executor's stages:\n{stages:#}");
    assert_eq!(stages["page_read"], json!({"n": 6, "ms_mean": 1.0}));
    assert_eq!(stages["write"], json!({"n": 2, "ms_mean": 20.0}));
    assert_eq!(stages["write_skipped"], json!({"n": 4, "ms_mean": null}));
    assert_eq!(job["hook_stats"]["done"], 0);
}

#[tokio::test]
#[serial]
async fn nothing_is_measured_while_metrics_are_off() {
    let Some(app) = app().await else {
        return;
    };
    app.state.metrics.set_enabled(false);
    let source = JetstreamSource::builder()
        .executor_for(|c| (c == OBSERVED).then(|| Arc::new(Observing) as Arc<dyn BackfillExecutor>))
        .build_detached();
    let router = attach_routed(&app.state, source.clone());
    let id = start_backfill(&source, OBSERVED).await;
    wait_until!(
        "the job to complete",
        job_row(&app, &id).await.0 == "completed"
    );
    assert_eq!(job_row(&app, &id).await.1, None);
    let ingest = get(&app, &router, "/admin/metrics/ingest").await;
    assert_eq!(ingest["enabled"], false);
    assert_eq!(ingest["collections"], json!([]));
}
