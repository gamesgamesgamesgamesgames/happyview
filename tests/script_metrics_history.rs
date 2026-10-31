//! Stored script metrics: closed minutes written to the table, folded into
//! hours, trimmed by the event log's retention, read back over 7 and 30 days
//! through the admin routes, and read back into memory by a restart.
//!
//! Runs are recorded at injected past times with `record_at`, and the writer
//! is driven with an injected "now", so days pass in a few calls. One run
//! goes through the real dispatch, through the `interpreter_echo` fixture,
//! to show a run still in memory is counted beside the stored ones.

mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use common::app::TestApp;
use common::echo_interpreter;
use happyview::metrics::ingest::{RecordDecision, RecordSample};
use happyview::metrics::persist::{self, Persister};
use happyview::metrics::run::RunTimings;
use happyview::metrics::{Layer, Outcome, RunSample, ScriptMetrics, Window, unix_now};
use happyview::plugin::ScriptKind;

const QUERY: &str = "games.gamesgamesgamesgames.listGames";
const DAY: u64 = 86_400;

macro_rules! require_fixture {
    () => {
        if !echo_interpreter::is_built() {
            happyview::test_support::unavailable(echo_interpreter::BUILD);
            return;
        }
    };
}

fn trigger() -> String {
    format!("xrpc.query:{QUERY}")
}

async fn request(
    app: &TestApp,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let cookie = app.admin_cookie();
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(cookie.0, cookie.1);
    let body = match body {
        Some(body) => {
            builder = builder.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&body).unwrap())
        }
        None => Body::empty(),
    };
    let resp = app
        .router
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn get(app: &TestApp, uri: &str) -> (StatusCode, Value) {
    request(app, Method::GET, uri, None).await
}

async fn app_with_query() -> TestApp {
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .register(echo_interpreter::plugin("lua"))
        .await;
    let (status, body) = request(
        &app,
        Method::POST,
        "/admin/lexicons",
        Some(json!({
            "lexicon_json": common::fixtures::list_games_query_lexicon(),
            "target_collection": "games.gamesgamesgamesgames.game",
        })),
    )
    .await;
    assert!(status.is_success(), "{status}: {body}");
    let now = happyview::db::now_rfc3339();
    happyview::db::query(&happyview::db::adapt_sql(
        "INSERT INTO happyview_scripts (id, body, script_type, created_at, updated_at) \
         VALUES (?, 'host:script_log', 'lua', ?, ?)",
        app.state.db_backend,
    ))
    .bind(trigger())
    .bind(&now)
    .bind(&now)
    .execute(&app.state.db)
    .await
    .expect("seed a script row");
    app
}

async fn call_query(app: &TestApp) -> StatusCode {
    app.router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/xrpc/{QUERY}"))
                .header("x-client-key", "hvc_test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

fn sample(wall_ms: u64) -> RunSample {
    RunSample {
        outcome: Outcome::Ok,
        wall: Duration::from_millis(wall_ms),
        prepare: Duration::from_micros(200),
        timings: RunTimings {
            guest_call: Some(Duration::from_millis(wall_ms / 2)),
            ..Default::default()
        },
    }
}

fn run_at(metrics: &ScriptMetrics, at: u64, wall_ms: u64) {
    metrics.record_at(
        at,
        &trigger(),
        ScriptKind::XrpcQuery,
        Layer::Dashboard,
        &sample(wall_ms),
    );
}

fn record_at(metrics: &ScriptMetrics, at: u64) {
    metrics.record_ingest_at(
        at * 1_000,
        "games.gamesgamesgamesgames.game",
        "create",
        None,
        &RecordSample {
            decision: RecordDecision::Proceed,
            record: Duration::from_millis(3),
            script: Some(Duration::from_millis(1)),
            stages: vec![("write", Duration::from_millis(2))],
            failed: false,
            timed_out: false,
            dead_lettered: false,
        },
    );
}

fn runs(listing: &Value) -> u64 {
    let scripts = listing["scripts"].as_array().expect("a scripts list");
    assert_eq!(scripts.len(), 1, "{listing:#}");
    scripts[0]["runs"].as_u64().unwrap()
}

fn point_runs(series: &Value) -> (usize, u64) {
    let points = series["points"].as_array().expect("points");
    (
        points.len(),
        points.iter().map(|p| p["runs"].as_u64().unwrap()).sum(),
    )
}

async fn rows(app: &TestApp, resolution: &str) -> i64 {
    happyview::db::query_scalar(&happyview::db::adapt_sql(
        "SELECT COUNT(*) FROM happyview_script_metrics WHERE resolution = ?",
        app.state.db_backend,
    ))
    .bind(resolution)
    .fetch_one(&app.state.db)
    .await
    .unwrap()
}

#[tokio::test]
async fn stored_metrics_are_written_folded_trimmed_and_read_back() {
    common::require_db!();
    require_fixture!();
    let app = app_with_query().await;
    let state = &app.state;
    let metrics = &state.metrics;
    let (db, backend) = (&state.db, state.db_backend);
    let base = unix_now();

    // Forty, ten and three days ago, each written once its minute closed,
    // in the order the writer would have met them.
    for (ago, wall_ms) in [(40 * DAY, 4), (10 * DAY, 6), (3 * DAY, 8)] {
        let at = base - ago;
        run_at(metrics, at, wall_ms);
        record_at(metrics, at);
        let written = persist::write_closed(metrics, db, backend, at + 60)
            .await
            .unwrap();
        assert_eq!(written, 2, "{ago}s ago");
    }
    assert_eq!(rows(&app, "minute").await, 6);

    // Ten minutes ago, not yet written, and one run now, through the real
    // dispatch, still in its open minute.
    run_at(metrics, base - 600, 12);
    assert_eq!(call_query(&app).await, StatusCode::OK);

    // The hourly pass: the forty-day-old minutes are past the event log's
    // thirty days, the other two are past two days and fold into hours.
    let tick = Persister::default().tick(metrics, db, backend, base).await;
    assert_eq!(tick.written, 1, "ten minutes ago, but not the open minute");
    assert_eq!(tick.swept, 2);
    assert_eq!(tick.folded, 4);
    assert_eq!(rows(&app, "minute").await, 1);
    assert_eq!(rows(&app, "hour").await, 4);

    let (status, week) = get(&app, "/admin/metrics/scripts?window=7d").await;
    assert_eq!(status, StatusCode::OK, "{week}");
    println!("GET /admin/metrics/scripts?window=7d:\n{week:#}");
    assert_eq!(week["window"], "7d");
    assert_eq!(week["retention_days"], 30);
    assert_eq!(week["enabled"], true);
    assert_eq!(runs(&week), 3);
    let row = &week["scripts"][0];
    assert_eq!(row["trigger"], trigger());
    assert_eq!(row["kind"], "xrpc_query");
    assert_eq!(row["layer"], "dashboard");
    assert_eq!(row["outcomes"], json!({"ok": 3}));
    assert!(row["wall_ms"]["max"].as_f64().unwrap() >= 8.0, "{row:#}");
    // The stored row's shape is the in-memory one's.
    let (_, hour) = get(&app, "/admin/metrics/scripts?window=1h").await;
    let keys = |v: &Value| {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    assert_eq!(keys(row), keys(&hour["scripts"][0]));

    let (_, month) = get(&app, "/admin/metrics/scripts?window=30d").await;
    assert_eq!(runs(&month), 4);
    assert_eq!(month["retention_days"], 30);

    let encoded = trigger().replace(':', "%3A");
    let (status, series) = get(
        &app,
        &format!("/admin/metrics/scripts/{encoded}?window=7d&step=1h"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{series}");
    assert_eq!(series["step"], "1h");
    assert_eq!(series["retention_days"], 30);
    assert_eq!(series["layers"], json!(["dashboard"]));
    assert_eq!(point_runs(&series), (168, 3));
    let (status, series) = get(
        &app,
        &format!("/admin/metrics/scripts/{encoded}?window=30d"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{series}");
    assert_eq!(series["step"], "1d");
    assert_eq!(point_runs(&series), (30, 4));
    let (status, _) = get(
        &app,
        &format!("/admin/metrics/scripts/{encoded}?window=7d&step=1m"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = get(
        &app,
        &format!("/admin/metrics/scripts/{encoded}?window=1h&step=1d"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, ingest) = get(&app, "/admin/metrics/ingest?window=30d").await;
    assert_eq!(status, StatusCode::OK, "{ingest}");
    assert_eq!(ingest["retention_days"], 30);
    let collections = ingest["collections"].as_array().unwrap();
    assert_eq!(collections.len(), 1, "{ingest:#}");
    assert_eq!(collections[0]["records"], 2);
    assert_eq!(collections[0]["stages"]["write"]["n"], 2);

    // Retention follows the event log's setting, re-read on the next pass.
    let (status, body) = request(
        &app,
        Method::PUT,
        "/admin/settings/event_log_retention_days",
        Some(json!({"value": "5"})),
    )
    .await;
    assert!(status.is_success(), "{status}: {body}");
    // A new writer stands for the next hour's pass: moving the injected
    // clock an hour on would close the real run's minute in the past of
    // the routes, which read the real clock.
    let tick = Persister::default().tick(metrics, db, backend, base).await;
    assert_eq!(tick.swept, 2, "the ten-day-old hours");
    let (_, month) = get(&app, "/admin/metrics/scripts?window=30d").await;
    assert_eq!(month["retention_days"], 5);
    assert_eq!(runs(&month), 3, "a window past retention is what exists");

    // A restart: the run just made is written once its minute closes, and
    // a new registry reads it back with the minute before it.
    let now = base + 120;
    let written = persist::write_closed(metrics, db, backend, now)
        .await
        .unwrap();
    assert_eq!(written, 1);
    let restarted = ScriptMetrics::new();
    assert_eq!(
        persist::reload(&restarted, db, backend, now).await.unwrap(),
        2
    );
    let restored = restarted.scripts_at(now, Window::Hour);
    assert_eq!(restored, metrics.scripts_at(now, Window::Hour));
    assert_eq!(restored[0].stats.runs, 2);
    assert_eq!(restarted.scripts_at(now, Window::Day)[0].stats.runs, 2);
}
