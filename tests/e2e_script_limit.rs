//! The instruction limit ends a spinning script on every runner that serves
//! one, including a script that catches the limit error and loops again, and
//! the runner's worker is released while it does.
//!
//! Each case runs on a one-worker runtime of its own, beside a ticker task.
//! The ticker can only run while the script lets the worker go, so the
//! longest gap between two ticks is the longest the script held the worker.
//! A run that never ends would hang that runtime, so it lives on its own
//! thread and the test waits for it with a deadline that does not depend on
//! the runtime being free.

mod common;

use std::future::Future;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::db::now_rfc3339;
use happyview::lua::{LabelAppliedEvent, LabelHookOutcome, run_label_applied_script};
use happyview::record_handler::{RecordEvent, handle_record_event};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;
use common::fixtures;

/// How long one case may take before it counts as never stopping.
const RUN_DEADLINE: Duration = Duration::from_secs(30);

/// The longest the script may hold the worker at once. One budget of
/// instructions takes milliseconds; this leaves room for a debug build on a
/// busy machine.
const MAX_HOLD: Duration = Duration::from_secs(2);

/// Loops that end only when the limit stops them. Each catches the limit
/// error a different way and carries on.
const SPINNERS: &[(&str, &str)] = &[
    ("a plain loop", "while true do end"),
    (
        "a pcall loop",
        "while true do pcall(function() while true do end end) end",
    ),
    (
        "a loop that catches the limit error by name",
        r#"while true do
             local ok, err = pcall(function() while true do end end)
             if not ok and tostring(err):find("limit") then end
           end"#,
    ),
    (
        "an xpcall loop",
        "while true do xpcall(function() while true do end end, function(e) return e end) end",
    ),
    (
        "a coroutine loop",
        "while true do pcall(coroutine.wrap(function() while true do end end)) end",
    ),
];

struct Report<T> {
    outcome: T,
    elapsed: Duration,
    longest_hold: Duration,
}

/// Runs `test` to the end on a fresh one-worker runtime on its own thread,
/// and fails if it has not finished by `deadline`.
fn on_one_worker<F, Fut>(deadline: Duration, test: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ()>,
{
    let (tx, rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(test());
        let _ = tx.send(());
    });
    match rx.recv_timeout(deadline) {
        Ok(()) => {}
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            if let Err(panic) = thread.join() {
                std::panic::resume_unwind(panic);
            }
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("a script never stopped, and held its worker the whole time")
        }
    }
}

/// Runs `case` beside a 10 ms ticker on the current runtime, which must have
/// one worker, and reports the longest gap between two ticks.
async fn measure<T: Send + 'static>(case: impl Future<Output = T> + Send + 'static) -> Report<T> {
    let last_tick = Arc::new(Mutex::new((Instant::now(), Duration::ZERO)));
    let ticks = Arc::clone(&last_tick);
    let ticker = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let mut t = ticks.lock().unwrap();
            let gap = t.0.elapsed();
            t.0 = Instant::now();
            t.1 = t.1.max(gap);
        }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    *last_tick.lock().unwrap() = (Instant::now(), Duration::ZERO);

    let start = Instant::now();
    let outcome = tokio::spawn(case).await.unwrap();
    let elapsed = start.elapsed();

    ticker.abort();
    let (last, gap) = *last_tick.lock().unwrap();
    Report {
        outcome,
        elapsed,
        longest_hold: gap.max(last.elapsed()),
    }
}

fn assert_released<T>(name: &str, report: &Report<T>) {
    assert!(
        report.longest_hold < MAX_HOLD,
        "{name}: held the worker for {:?} at once",
        report.longest_hold
    );
}

async fn send(app: &TestApp, method: &str, uri: &str, body: Value) {
    let cookie = app.admin_cookie();
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(cookie.0, cookie.1)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "{method} {uri}: {}",
        resp.status()
    );
}

async fn seed_lexicons(app: &TestApp, lexicons: Vec<Value>) {
    for lexicon in lexicons {
        send(
            app,
            "POST",
            "/admin/lexicons",
            json!({ "lexicon_json": lexicon }),
        )
        .await;
    }
}

/// Stores `spinner` as the body of the script `trigger`, creating the script
/// the first time.
async fn set_script(app: &TestApp, trigger: &str, spinner: &str, first: bool) {
    let body = format!("function handle()\n{spinner}\nend");
    if first {
        send(
            app,
            "POST",
            "/admin/scripts",
            json!({ "id": trigger, "body": body }),
        )
        .await;
    } else {
        let uri = format!("/admin/scripts/{}", urlencoding::encode(trigger));
        send(app, "PATCH", &uri, json!({ "body": body })).await;
    }
}

/// The record and label runners retry a failed script three times, waiting
/// 1, 2 and 4 seconds, so each case takes about 7 seconds when every attempt
/// ends promptly.
fn deadline() -> Duration {
    RUN_DEADLINE * SPINNERS.len() as u32
}

#[test]
#[serial]
fn an_anonymous_query_script_is_stopped_by_the_limit() {
    common::require_db!();
    on_one_worker(deadline(), || async {
        let app = Arc::new(TestApp::new().await);
        seed_lexicons(
            &app,
            vec![
                fixtures::game_record_lexicon(),
                fixtures::list_games_query_lexicon(),
            ],
        )
        .await;
        let trigger = "xrpc.query:games.gamesgamesgamesgames.listGames";
        for (i, (name, spinner)) in SPINNERS.iter().enumerate() {
            set_script(&app, trigger, spinner, i == 0).await;
            let router = app.router.clone();
            let report = measure(async move {
                let resp = router
                    .oneshot(
                        Request::builder()
                            .uri("/xrpc/games.gamesgamesgamesgames.listGames")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let status = resp.status();
                let body = resp.into_body().collect().await.unwrap().to_bytes();
                (
                    status,
                    serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null),
                )
            })
            .await;
            let (status, body) = &report.outcome;
            assert_eq!(*status, StatusCode::REQUEST_TIMEOUT, "{name}: {body}");
            assert_eq!(body["errorType"], "timeout", "{name}: {body}");
            assert_released(name, &report);
            println!("{name}: stopped in {:?}", report.elapsed);
        }
    });
}

#[test]
#[serial]
fn a_record_script_is_stopped_by_the_limit() {
    common::require_db!();
    on_one_worker(deadline(), || async {
        let app = Arc::new(TestApp::new().await);
        seed_lexicons(&app, vec![fixtures::game_record_lexicon()]).await;
        let trigger = "record.index:games.gamesgamesgamesgames.game";
        for (i, (name, spinner)) in SPINNERS.iter().enumerate() {
            set_script(&app, trigger, spinner, i == 0).await;
            let state = app.state.clone();
            let report = measure(async move {
                let record = json!({ "title": "spin" });
                let cid = happyview::cid_verify::compute_record_cid(&record).map(|c| c.to_string());
                let event = RecordEvent {
                    did: "did:plc:test".into(),
                    collection: "games.gamesgamesgamesgames.game".into(),
                    rkey: format!("spin{i}"),
                    action: "create".into(),
                    record: Some(record),
                    cid,
                };
                let _ = handle_record_event(&state, &event).await;
            })
            .await;
            assert_released(name, &report);
            println!("{name}: stopped in {:?}", report.elapsed);
        }
    });
}

#[test]
#[serial]
fn a_label_script_is_stopped_by_the_limit() {
    common::require_db!();
    on_one_worker(deadline(), || async {
        let app = Arc::new(TestApp::new().await);
        let trigger = "labeler.apply:app.bsky.feed.post";
        for (i, (name, spinner)) in SPINNERS.iter().enumerate() {
            set_script(&app, trigger, spinner, i == 0).await;
            let state = app.state.clone();
            let report = measure(async move {
                let event = LabelAppliedEvent {
                    src: "did:plc:labeler".into(),
                    uri: "at://did:plc:test/app.bsky.feed.post/spin".into(),
                    val: "spam".into(),
                    neg: false,
                    cts: now_rfc3339(),
                    exp: None,
                };
                run_label_applied_script(&state, event).await
            })
            .await;
            // The label runner fails open: the label passes through.
            assert!(
                matches!(report.outcome, LabelHookOutcome::Continue(_)),
                "{name}: {:?}",
                report.outcome
            );
            assert_released(name, &report);
            println!("{name}: stopped in {:?}", report.elapsed);
        }
    });
}
