//! The Jetstream consumer against a scripted server (`common::jetstream`).
//! Needs `TEST_DATABASE_URL`; serial, because the tests share one database
//! and the process-wide status.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use happyview::db::DatabaseBackend;
use happyview::db::adapt_sql;
use happyview::jetstream::config::{DRAIN_TIMEOUT, ProtocolChoice, Settings, parse_endpoint};
use happyview::jetstream::workers::RetryPolicy;
use happyview::lexicon::{ParsedLexicon, ProcedureAction};
use serde_json::{Value, json};
use serial_test::serial;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use common::app::TestApp;
use common::jetstream::{self as js, MockJetstream};

const GAME: &str = "games.gamesgamesgamesgames.game";

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

/// A running consumer, stopped when dropped.
struct Consumer {
    handle: JoinHandle<()>,
    _collections: watch::Sender<Vec<String>>,
}

impl Drop for Consumer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn app() -> Option<TestApp> {
    if std::env::var("TEST_DATABASE_URL").is_err() {
        eprintln!("skipped (TEST_DATABASE_URL not set)");
        return None;
    }
    let app = TestApp::new().await;
    let parsed = ParsedLexicon::parse(
        common::fixtures::game_record_lexicon(),
        1,
        Some(GAME.to_string()),
        ProcedureAction::Upsert,
        None,
    )
    .expect("parse the game lexicon");
    app.state.lexicons.upsert(parsed).await;
    Some(app)
}

fn settings(mock: &MockJetstream) -> Settings {
    Settings {
        endpoint: parse_endpoint(&mock.url()),
        protocol: ProtocolChoice::Auto,
        compression: true,
        workers: 8,
        invalid_request_retry: Duration::from_millis(100),
        drain_timeout: DRAIN_TIMEOUT,
    }
}

/// Retries in milliseconds, so a test never waits on the production backoff.
fn fast_retries() -> RetryPolicy {
    RetryPolicy {
        delays: vec![Duration::from_millis(10); 3],
        max_unavailable_delay: Duration::from_millis(50),
    }
}

fn start(app: &TestApp, settings: Settings) -> Consumer {
    let (tx, rx) = watch::channel(vec![GAME.to_string()]);
    let mut state = app.state.clone();
    state.config.jetstream_url = settings.endpoint.ws_base.clone();
    Consumer {
        handle: happyview::jetstream::spawn_with(state, rx, settings, Some(fast_retries())),
        _collections: tx,
    }
}

fn game(title: &str) -> Value {
    json!({"$type": GAME, "title": title})
}

async fn setting(app: &TestApp, key: &str) -> Option<String> {
    let sql = adapt_sql(
        "SELECT value FROM happyview_instance_settings WHERE key = ?",
        app.state.db_backend,
    );
    happyview::db::query_as::<(String,)>(&sql)
        .bind(key)
        .fetch_optional(&app.state.db)
        .await
        .expect("read setting")
        .map(|(v,)| v)
}

async fn set_setting(app: &TestApp, key: &str, value: &str) {
    let sql = adapt_sql(
        "INSERT INTO happyview_instance_settings (key, value, updated_at) VALUES (?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(key)
        .bind(value)
        .bind(happyview::db::now_rfc3339())
        .execute(&app.state.db)
        .await
        .expect("seed setting");
}

/// Whether the saved v2 cursor has reached `seq`.
async fn applied_through(app: &TestApp, seq: i64) -> bool {
    setting(app, "jetstream_seq")
        .await
        .and_then(|v| v.parse::<i64>().ok())
        >= Some(seq)
}

async fn rkeys(app: &TestApp, did: &str) -> Vec<String> {
    let sql = adapt_sql(
        "SELECT rkey FROM happyview_records WHERE did = ? ORDER BY rkey",
        app.state.db_backend,
    );
    happyview::db::query_as::<(String,)>(&sql)
        .bind(did)
        .fetch_all(&app.state.db)
        .await
        .expect("read records")
        .into_iter()
        .map(|(r,)| r)
        .collect()
}

async fn count(app: &TestApp, sql: &str) -> i64 {
    happyview::db::query_as::<(i64,)>(&adapt_sql(sql, app.state.db_backend))
        .fetch_one(&app.state.db)
        .await
        .expect("count")
        .0
}

async fn events(app: &TestApp, event_type: &str) -> Vec<Value> {
    let sql = adapt_sql(
        "SELECT detail FROM happyview_event_logs WHERE event_type = ? ORDER BY created_at",
        app.state.db_backend,
    );
    happyview::db::query_as::<(String,)>(&sql)
        .bind(event_type)
        .fetch_all(&app.state.db)
        .await
        .expect("read events")
        .into_iter()
        .map(|(d,)| serde_json::from_str(&d).expect("detail json"))
        .collect()
}

async fn insert_local(app: &TestApp, did: &str, rkey: &str) {
    let sql = adapt_sql(
        "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at) \
         VALUES (?, ?, ?, ?, ?, '', NULL, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(format!("at://{did}/{GAME}/{rkey}"))
        .bind(did)
        .bind(GAME)
        .bind(rkey)
        .bind(game(rkey).to_string())
        .bind(happyview::db::now_rfc3339())
        .execute(&app.state.db)
        .await
        .expect("insert local record");
}

/// 1,000 records, each created then deleted, spread over 100 DIDs and 8
/// workers: every one ends deleted. This alone cannot catch a race (a delete
/// that overtook its create tombstones it, with the same result), so the
/// two tests after it use scripts whose outcome depends on the order.
#[tokio::test]
#[serial]
async fn creates_and_deletes_of_one_uri_never_race() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    let mut seq = 0;
    let mut script = Vec::new();
    for n in 0..1000 {
        let did = format!("did:plc:race{}", n % 100);
        let rkey = format!("r{n:04}");
        seq += 1;
        script.push(js::create(seq, &did, GAME, &rkey, game(&rkey)));
        seq += 1;
        script.push(js::delete(seq, &did, GAME, &rkey));
    }
    for d in 0..100 {
        seq += 1;
        script.push(js::create(
            seq,
            &format!("did:plc:race{d}"),
            GAME,
            "survivor",
            game("survivor"),
        ));
    }
    mock.push(script);
    let _consumer = start(&app, settings(&mock));

    wait_until!("every event applied", applied_through(&app, seq).await);
    assert_eq!(
        count(
            &app,
            "SELECT COUNT(*) FROM happyview_records WHERE rkey <> 'survivor'"
        )
        .await,
        0,
        "a delete raced ahead of its create"
    );
    assert_eq!(
        count(
            &app,
            "SELECT COUNT(*) FROM happyview_records WHERE rkey = 'survivor'"
        )
        .await,
        100
    );
    assert_eq!(
        count(&app, "SELECT COUNT(*) FROM happyview_record_tombstones").await,
        1000
    );

    let connections = mock.connections();
    let first = &connections[0];
    assert_eq!(first.path, "v2");
    assert_eq!(
        first.param("zstdDictionary"),
        Some(js::DICTIONARY_A.to_string()),
        "frames were compressed"
    );
    assert!(first.params("collections").contains(&GAME.to_string()));
    assert_eq!(first.param("kinds"), None);
    assert_eq!(
        first.param("cursor"),
        None,
        "a fresh instance starts at the tip"
    );
}

/// A restart resumes at the saved seq: what came before is not re-applied,
/// and nothing after is skipped.
#[tokio::test]
#[serial]
async fn a_restart_resumes_after_the_saved_seq() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    set_setting(&app, "jetstream_seq", "10").await;
    set_setting(&app, "jetstream_cursor", &js::witnessed_us(10).to_string()).await;
    set_setting(&app, "jetstream_host", &mock.host()).await;
    mock.push(
        (1..=20).map(|s| js::create(s, "did:plc:resume", GAME, &format!("r{s:02}"), game("x"))),
    );
    let _consumer = start(&app, settings(&mock));

    wait_until!("seq 20 applied", applied_through(&app, 20).await);
    assert_eq!(mock.connections()[0].param("cursor").as_deref(), Some("10"));
    let expected: Vec<String> = (11..=20).map(|s| format!("r{s:02}")).collect();
    assert_eq!(
        rkeys(&app, "did:plc:resume").await,
        expected,
        "seq 10 was applied before the restart and is dropped as a duplicate"
    );
}

/// The pre-upgrade cursor is a v1 `time_us`. v2 resumes from it as a
/// timestamp, then saves seqs; `jetstream_cursor` keeps moving so v1 and an
/// older HappyView can still resume.
#[tokio::test]
#[serial]
async fn a_v1_time_cursor_resumes_v2_and_then_switches_to_seq() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    set_setting(&app, "jetstream_cursor", &js::witnessed_us(3).to_string()).await;
    mock.push((1..=6).map(|s| js::create(s, "did:plc:migrate", GAME, &format!("r{s}"), game("x"))));
    let consumer = start(&app, settings(&mock));

    wait_until!("seq 6 applied", applied_through(&app, 6).await);
    assert_eq!(
        mock.connections()[0].param("cursor"),
        Some(js::witnessed_us(3).to_string())
    );
    assert_eq!(
        rkeys(&app, "did:plc:migrate").await,
        vec!["r3", "r4", "r5", "r6"]
    );
    assert_eq!(setting(&app, "jetstream_host").await, Some(mock.host()));
    assert_eq!(
        setting(&app, "jetstream_cursor").await,
        Some(js::witnessed_us(6).to_string())
    );

    mock.kick();
    wait_until!("a reconnect", mock.connections().len() >= 2);
    assert_eq!(mock.connections()[1].param("cursor").as_deref(), Some("6"));
    drop(consumer);
}

/// Seqs belong to one Jetstream instance. On another host the consumer
/// resumes 5 s before the saved witnessed time instead.
#[tokio::test]
#[serial]
async fn a_new_host_resumes_by_witnessed_time() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    set_setting(&app, "jetstream_seq", "4").await;
    set_setting(&app, "jetstream_cursor", &js::witnessed_us(4).to_string()).await;
    set_setting(&app, "jetstream_host", "old-jetstream.example.com").await;
    mock.push((1..=6).map(|s| js::create(s, "did:plc:moved", GAME, &format!("r{s}"), game("x"))));
    let _consumer = start(&app, settings(&mock));

    wait_until!("seq 6 applied", applied_through(&app, 6).await);
    assert_eq!(
        mock.connections()[0].param("cursor"),
        Some((js::witnessed_us(4) - 5_000_000).to_string())
    );
    assert_eq!(setting(&app, "jetstream_host").await, Some(mock.host()));
    assert_eq!(rkeys(&app, "did:plc:moved").await.len(), 6);
}

#[tokio::test]
#[serial]
async fn a_deleted_account_is_purged() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    insert_local(&app, "did:plc:gone", "local").await;
    mock.push([
        js::create(1, "did:plc:gone", GAME, "a", game("a")),
        js::create(2, "did:plc:gone", GAME, "b", game("b")),
        js::create(3, "did:plc:kept", GAME, "c", game("c")),
        js::account(4, "did:plc:gone", false, Some("deleted")),
    ]);
    let _consumer = start(&app, settings(&mock));

    wait_until!("seq 4 applied", applied_through(&app, 4).await);
    assert_eq!(rkeys(&app, "did:plc:gone").await, vec!["local"]);
    assert_eq!(rkeys(&app, "did:plc:kept").await, vec!["c"]);
    assert_eq!(
        count(
            &app,
            "SELECT COUNT(*) FROM happyview_did_tombstones WHERE did = 'did:plc:gone'"
        )
        .await,
        1
    );
    assert_eq!(
        events(&app, "jetstream.account_deleted").await,
        vec![json!({"purged": 2, "seq": 4})]
    );
}

#[tokio::test]
#[serial]
async fn a_sync_replaces_the_dids_records() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    mock.push([
        js::create(1, "did:plc:resync", GAME, "a", game("a")),
        js::create(2, "did:plc:resync", GAME, "b", game("b")),
        js::sync(3, "did:plc:resync"),
        js::create(4, "did:plc:resync", GAME, "c", game("c")),
    ]);
    let _consumer = start(&app, settings(&mock));

    wait_until!("seq 4 applied", applied_through(&app, 4).await);
    assert_eq!(rkeys(&app, "did:plc:resync").await, vec!["c"]);
    assert_eq!(
        events(&app, "jetstream.sync_purged").await,
        vec![json!({"purged": 2, "rev": js::rev_for(3)})]
    );
}

#[tokio::test]
#[serial]
async fn a_takedown_purges_the_records() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    mock.push([
        js::create(1, "did:plc:taken", GAME, "a", game("a")),
        js::account(2, "did:plc:taken", false, Some("takendown")),
        js::identity(3, "did:plc:taken", "taken.example.com"),
    ]);
    let _consumer = start(&app, settings(&mock));

    wait_until!("seq 3 applied", applied_through(&app, 3).await);
    assert!(rkeys(&app, "did:plc:taken").await.is_empty());
    assert_eq!(
        events(&app, "jetstream.account_takendown").await,
        vec![json!({"purged": 1, "seq": 2, "status": "takendown"})]
    );
}

/// One event of a per-DID script, built from its seq and DID.
type Step = dyn Fn(i64, &str) -> Value;

/// Push one script per DID, interleaved in groups of four DIDs so a DID's
/// events sit a few frames apart, each on its own seq. Returns the last seq.
fn interleave(mock: &MockJetstream, dids: usize, steps: &[&Step]) -> i64 {
    let mut seq = 0;
    let mut script = Vec::new();
    for group in (0..dids).step_by(4) {
        for step in steps {
            for d in group..(group + 4).min(dids) {
                seq += 1;
                script.push(step(seq, &format!("did:plc:order{d}")));
            }
        }
    }
    mock.push(script);
    seq
}

/// Per DID, `create a`, `#sync`, `create b`: only b survives, and only when
/// the three apply in order. A sync applied before `a` leaves it, and one
/// applied after `b` purges it.
#[tokio::test]
#[serial]
async fn a_sync_between_creates_applies_in_order() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    let last = interleave(
        &mock,
        1000,
        &[
            &|seq, did| js::create(seq, did, GAME, "a", game("a")),
            &|seq, did| js::sync(seq, did),
            &|seq, did| js::create(seq, did, GAME, "b", game("b")),
        ],
    );
    let _consumer = start(&app, settings(&mock));

    wait_until!("every event applied", applied_through(&app, last).await);
    assert_eq!(
        count(
            &app,
            "SELECT COUNT(*) FROM happyview_records WHERE rkey = 'a'"
        )
        .await,
        0,
        "a sync ran before the create it should have purged"
    );
    assert_eq!(
        count(
            &app,
            "SELECT COUNT(*) FROM happyview_records WHERE rkey = 'b'"
        )
        .await,
        1000,
        "a create ran before the sync that then purged it"
    );
}

/// Per DID, `create a`, account deleted, account reactivated, then `create c`
/// at a later rev: c exists and a does not. A `create c` that overtook the
/// deletion would be purged by it.
#[tokio::test]
#[serial]
async fn a_deleted_then_reactivated_account_applies_in_order() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    let last = interleave(
        &mock,
        1000,
        &[
            &|seq, did| js::create(seq, did, GAME, "a", game("a")),
            &|seq, did| js::account(seq, did, false, Some("deleted")),
            &|seq, did| js::account(seq, did, true, None),
            &|seq, did| js::create(seq, did, GAME, "c", game("c")),
        ],
    );
    let _consumer = start(&app, settings(&mock));

    wait_until!("every event applied", applied_through(&app, last).await);
    assert_eq!(
        count(
            &app,
            "SELECT COUNT(*) FROM happyview_records WHERE rkey = 'a'"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &app,
            "SELECT COUNT(*) FROM happyview_records WHERE rkey = 'c'"
        )
        .await,
        1000,
        "a create ran before the account deletion that then purged it"
    );
    assert_eq!(
        count(&app, "SELECT COUNT(*) FROM happyview_did_tombstones").await,
        0,
        "every reactivation lifted its tombstone"
    );
}

/// The advisory-lock class `tombstones::lock_uris` takes a record URI's lock
/// in (`URI_LOCK_CLASS`).
const URI_LOCK_CLASS: i32 = 0x6876_7201;

/// Holds back writes until released: on Postgres the advisory lock that
/// applying `uri` waits on, on SQLite the database's write lock (so every
/// write waits, retried as unavailable).
struct Blocker {
    conn: sqlx::pool::PoolConnection<sqlx::Any>,
    backend: DatabaseBackend,
}

impl Blocker {
    async fn hold(app: &TestApp, uri: &str) -> Self {
        let backend = app.state.db_backend;
        let mut conn = app.state.db.acquire().await.expect("blocker connection");
        match backend {
            DatabaseBackend::Postgres => {
                happyview::db::query(&format!(
                    "SELECT pg_advisory_lock({URI_LOCK_CLASS}, hashtext($1))::text"
                ))
                .bind(uri)
                .execute(&mut *conn)
                .await
                .expect("take the record's lock");
            }
            DatabaseBackend::Sqlite => {
                happyview::db::query("BEGIN IMMEDIATE")
                    .execute(&mut *conn)
                    .await
                    .expect("take the write lock");
            }
        }
        Self { conn, backend }
    }

    async fn release(mut self) {
        let sql = match self.backend {
            DatabaseBackend::Postgres => "SELECT pg_advisory_unlock_all()::text",
            DatabaseBackend::Sqlite => "COMMIT",
        };
        happyview::db::query(sql)
            .execute(&mut *self.conn)
            .await
            .expect("release the blocker");
    }
}

/// Watches `jetstream_seq` until dropped and records whether it ever went
/// backwards, and every value it held.
struct SeqWatch {
    went_back: Arc<AtomicBool>,
    seen: Arc<std::sync::Mutex<Vec<i64>>>,
    task: JoinHandle<()>,
}

impl SeqWatch {
    fn start(app: &TestApp) -> Self {
        let went_back = Arc::new(AtomicBool::new(false));
        let (db, backend) = (app.state.db.clone(), app.state.db_backend);
        let flag = Arc::clone(&went_back);
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let values = Arc::clone(&seen);
        let task = tokio::spawn(async move {
            let sql = adapt_sql(
                "SELECT value FROM happyview_instance_settings WHERE key = 'jetstream_seq'",
                backend,
            );
            let mut highest = i64::MIN;
            loop {
                let seq = happyview::db::query_as::<(String,)>(&sql)
                    .fetch_optional(&db)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|(v,)| v.parse::<i64>().ok());
                if let Some(seq) = seq {
                    let mut values = values.lock().expect("seen");
                    if values.last() != Some(&seq) {
                        values.push(seq);
                    }
                    drop(values);
                    if seq < highest {
                        flag.store(true, Ordering::SeqCst);
                    }
                    highest = highest.max(seq);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        Self {
            went_back,
            seen,
            task,
        }
    }

    fn went_back(&self) -> bool {
        self.went_back.load(Ordering::SeqCst)
    }

    fn seen(&self) -> Vec<i64> {
        self.seen.lock().expect("seen").clone()
    }
}

impl Drop for SeqWatch {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn received(app: &TestApp) -> u64 {
    app.state
        .telemetry_counters
        .jetstream_received
        .load(Ordering::Relaxed)
}

/// A session whose events are still applying when its drain times out: the
/// reconnect resumes after the last event dispatched, not at the saved
/// cursor, so nothing still queued is dispatched again and the saved seq
/// never moves backwards.
#[tokio::test]
#[serial]
async fn a_drain_timeout_does_not_redispatch_queued_events() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    let did = "did:plc:drain";
    let event = |s: i64| js::create(s, did, GAME, &format!("r{s}"), game("x"));
    mock.push((1..=3).map(event));
    let seqs = SeqWatch::start(&app);
    let base = received(&app);
    let _consumer = start(
        &app,
        Settings {
            drain_timeout: Duration::from_millis(200),
            ..settings(&mock)
        },
    );
    wait_until!("seq 3 applied", applied_through(&app, 3).await);

    let blocker = Blocker::hold(&app, &format!("at://{did}/{GAME}/r4")).await;
    mock.push((4..=8).map(event));
    mock.kick();
    wait_until!("seqs 4-8 dispatched", received(&app) == base + 8);
    assert_eq!(mock.connections()[1].param("cursor").as_deref(), Some("3"));

    mock.kick();
    wait_until!(
        "a reconnect after the drain timeout",
        mock.connections().len() >= 3
    );
    assert_eq!(
        mock.connections()[2].param("cursor").as_deref(),
        Some("8"),
        "resumes after what the pool already holds, not at the saved seq 3"
    );

    blocker.release().await;
    wait_until!("seq 8 applied", applied_through(&app, 8).await);
    let expected: Vec<String> = (1..=8).map(|s| format!("r{s}")).collect();
    assert_eq!(rkeys(&app, did).await, expected);
    assert_eq!(received(&app), base + 8, "an event was dispatched twice");
    assert!(!seqs.went_back(), "jetstream_seq moved backwards");
}

/// A fresh instance (no saved cursor) whose first event is stuck: a
/// reconnect resumes after what it dispatched rather than at the live tip,
/// which would skip whatever arrived in between.
#[tokio::test]
#[serial]
async fn a_fresh_instance_with_a_stuck_event_resumes_past_what_it_dispatched() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    let did = "did:plc:stuck";
    mock.push((1..=4).map(|s| js::create(s, did, GAME, &format!("r{s}"), game("x"))));
    let blocker = Blocker::hold(&app, &format!("at://{did}/{GAME}/r1")).await;
    let base = received(&app);
    let _consumer = start(
        &app,
        Settings {
            drain_timeout: Duration::from_millis(200),
            ..settings(&mock)
        },
    );
    wait_until!("seqs 1-4 dispatched", received(&app) == base + 4);
    assert_eq!(
        setting(&app, "jetstream_seq").await,
        None,
        "nothing applied yet"
    );

    mock.kick();
    wait_until!("a reconnect", mock.connections().len() >= 2);
    assert_eq!(mock.connections()[0].param("cursor"), None);
    assert_eq!(
        mock.connections()[1].param("cursor").as_deref(),
        Some("4"),
        "resumes after the dispatched events, not at the tip"
    );

    blocker.release().await;
    wait_until!("seq 4 applied", applied_through(&app, 4).await);
    assert_eq!(rkeys(&app, did).await, vec!["r1", "r2", "r3", "r4"]);
    assert_eq!(received(&app), base + 4);
}

/// A Jetstream without v2 answers 404; `auto` falls back to v1, which still
/// applies commits and account deletions (v1 never sends `#sync`).
#[tokio::test]
#[serial]
async fn v1_serves_when_v2_answers_404() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    mock.disable_v2();
    mock.push([
        js::create(1, "did:plc:v1", GAME, "a", game("a")),
        js::create(2, "did:plc:v1", GAME, "b", game("b")),
        js::create(3, "did:plc:v1gone", GAME, "c", game("c")),
        js::account(4, "did:plc:v1gone", false, Some("deleted")),
        js::sync(5, "did:plc:v1"),
        js::create(6, "did:plc:v1", GAME, "d", game("d")),
    ]);
    let _consumer = start(&app, settings(&mock));

    wait_until!(
        "the v1 cursor reaches seq 6",
        setting(&app, "jetstream_cursor").await == Some(js::witnessed_us(6).to_string())
    );
    let connections = mock.connections();
    assert_eq!(connections[0].path, "v2");
    assert_eq!(connections[1].path, "v1");
    assert_eq!(connections[1].param("compress").as_deref(), Some("false"));
    assert_eq!(rkeys(&app, "did:plc:v1").await, vec!["a", "b", "d"]);
    assert!(rkeys(&app, "did:plc:v1gone").await.is_empty());
    assert_eq!(
        setting(&app, "jetstream_seq").await,
        None,
        "v1 has no seq to save"
    );
}

/// A seq cursor below the server's retention is refused before the upgrade.
/// The consumer resumes by the witnessed time it saved alongside, which the
/// server clamps up to its floor with an `#info OutdatedCursor`: that notice
/// is the gap.
#[tokio::test]
#[serial]
async fn a_cursor_past_retention_resumes_by_witnessed_time() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    set_setting(&app, "jetstream_seq", "5").await;
    set_setting(&app, "jetstream_cursor", &js::witnessed_us(5).to_string()).await;
    set_setting(&app, "jetstream_host", &mock.host()).await;
    mock.set_retention_floor(90);
    mock.push((100..=102).map(|s| js::create(s, "did:plc:gap", GAME, &format!("r{s}"), game("x"))));
    let _consumer = start(&app, settings(&mock));

    wait_until!("seq 102 applied", applied_through(&app, 102).await);
    let connections = mock.connections();
    assert_eq!(connections[0].param("cursor").as_deref(), Some("5"));
    assert_eq!(
        connections[1].param("cursor"),
        Some(js::witnessed_us(5).to_string()),
        "the retry resumes by witnessed time"
    );
    assert_eq!(connections.len(), 2, "{connections:?}");
    assert_eq!(
        events(&app, "jetstream.gap").await,
        vec![json!({"reason": "outdated_cursor", "message": "cursor clamped to seq 90"})]
    );
    assert_eq!(
        rkeys(&app, "did:plc:gap").await,
        vec!["r100", "r101", "r102"]
    );
}

/// With no witnessed time saved, a refused seq leaves only the live tip; the
/// range lost is logged once the first event shows where the stream resumed.
#[tokio::test]
#[serial]
async fn a_cursor_past_retention_without_a_time_resumes_from_the_tip() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    set_setting(&app, "jetstream_seq", "5").await;
    set_setting(&app, "jetstream_host", &mock.host()).await;
    mock.set_retention_floor(90);
    mock.push((100..=102).map(|s| js::create(s, "did:plc:tip", GAME, &format!("r{s}"), game("x"))));
    let _consumer = start(&app, settings(&mock));

    wait_until!("seq 102 applied", applied_through(&app, 102).await);
    let connections = mock.connections();
    assert_eq!(connections[0].param("cursor").as_deref(), Some("5"));
    assert_eq!(
        connections[1].param("cursor"),
        None,
        "the retry starts at the live tip"
    );
    assert_eq!(
        events(&app, "jetstream.gap").await,
        vec![json!({
            "reason": "cursor_too_old",
            "message": "cursor 5 below lookback floor 90",
            "after_seq": 5,
            "resumed_at_seq": 100,
        })]
    );
    assert_eq!(
        rkeys(&app, "did:plc:tip").await,
        vec!["r100", "r101", "r102"]
    );
}

/// The seq refused is the last one dispatched, ahead of the saved one (an
/// event is still applying). The retry must not bring it back through
/// `last_dispatched`: it resumes by witnessed time, and the mock refuses
/// every seq below its floor, so a retry of the seq would loop forever.
#[tokio::test]
#[serial]
async fn a_refused_dispatched_seq_is_not_retried() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    let did = "did:plc:refused";
    let event = |s: i64| js::create(s, did, GAME, &format!("r{s:02}"), game("x"));
    mock.push((1..=2).map(event));
    let base = received(&app);
    let _consumer = start(
        &app,
        Settings {
            drain_timeout: Duration::from_millis(200),
            ..settings(&mock)
        },
    );
    wait_until!("seq 2 applied", applied_through(&app, 2).await);

    let blocker = Blocker::hold(&app, &format!("at://{did}/{GAME}/r03")).await;
    mock.push([event(3)]);
    mock.kick();
    wait_until!("seq 3 dispatched", received(&app) == base + 3);

    mock.set_retention_floor(10);
    mock.push((10..=12).map(event));
    mock.kick();
    wait_until!(
        "a reconnect past the refusal",
        mock.connections().len() >= 4
    );
    let connections = mock.connections();
    assert_eq!(
        setting(&app, "jetstream_seq").await.as_deref(),
        Some("2"),
        "seq 3 is still applying"
    );
    assert_eq!(
        connections[2].param("cursor").as_deref(),
        Some("3"),
        "resumes past the dispatched seq 3, which is refused"
    );
    assert_eq!(
        connections[3].param("cursor"),
        Some(js::witnessed_us(2).to_string()),
        "then by the saved witnessed time, not the refused seq"
    );

    blocker.release().await;
    wait_until!("seq 12 applied", applied_through(&app, 12).await);
    assert_eq!(
        rkeys(&app, did).await,
        vec!["r01", "r02", "r03", "r10", "r11", "r12"]
    );
    assert!(
        mock.connections()[3..].iter().all(|c| c
            .param("cursor")
            .and_then(|c| c.parse::<i64>().ok())
            .is_none_or(|c| c >= 10)),
        "a refused seq was retried: {:?}",
        mock.connections()
    );
}

/// A timestamp cursor (the pre-upgrade one) older than retention is clamped
/// by the server with an `#info OutdatedCursor` notice, which is a gap too.
#[tokio::test]
#[serial]
async fn a_clamped_time_cursor_logs_the_gap() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    set_setting(&app, "jetstream_cursor", &js::witnessed_us(1).to_string()).await;
    mock.notice_next("OutdatedCursor", "cursor clamped to seq 90");
    mock.push((1..=2).map(|s| js::create(s, "did:plc:clamped", GAME, &format!("r{s}"), game("x"))));
    let _consumer = start(&app, settings(&mock));

    wait_until!("seq 2 applied", applied_through(&app, 2).await);
    assert_eq!(
        events(&app, "jetstream.gap").await,
        vec![json!({"reason": "outdated_cursor", "message": "cursor clamped to seq 90"})]
    );
}

/// The server's seqs started over below the saved one: it answers the
/// resume with `#info FutureCursor` and would stream from its tip. A
/// witnessed time means the same on the new numbering, so the consumer
/// reconnects by the saved one and picks up the renumbered events from
/// there, not just those from the tip. It drops its dedup floor, so the
/// new, lower seqs apply and are saved.
#[tokio::test]
#[serial]
async fn a_future_cursor_resumes_by_witnessed_time() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    mock.push(
        (10..=12).map(|s| js::create(s, "did:plc:before", GAME, &format!("r{s}"), game("x"))),
    );
    let _consumer = start(&app, settings(&mock));
    wait_until!("seq 12 applied", applied_through(&app, 12).await);

    // Renumbered from 1, witnessed after seq 12 was.
    mock.reset_events();
    mock.push((1..=5).map(|s| {
        js::witnessed_later(
            js::create(s, "did:plc:after", GAME, &format!("r{s}"), game("x")),
            js::witnessed_us(20) - js::BASE_US,
        )
    }));
    mock.set_tip(4);
    mock.notice_next("FutureCursor", "cursor 12 is past the next seq 6");
    mock.kick();

    wait_until!(
        "the new seq 5 saved",
        setting(&app, "jetstream_seq").await.as_deref() == Some("5")
    );
    let connections = mock.connections();
    assert_eq!(connections[1].param("cursor").as_deref(), Some("12"));
    assert_eq!(
        connections[2].param("cursor"),
        Some(js::witnessed_us(12).to_string()),
        "reconnects by the saved witnessed time"
    );
    assert_eq!(
        rkeys(&app, "did:plc:after").await,
        vec!["r1", "r2", "r3", "r4", "r5"],
        "the events before the tip were recovered"
    );
    assert_eq!(
        events(&app, "jetstream.gap").await,
        vec![json!({"reason": "future_cursor", "from_seq": 12})]
    );
}

/// `FutureCursor` with no witnessed time saved: the stream goes on from the
/// tip, with the new, lower seqs applied and saved, and the gap logged.
#[tokio::test]
#[serial]
async fn a_future_cursor_without_a_time_resumes_from_the_tip() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    set_setting(&app, "jetstream_seq", "12").await;
    set_setting(&app, "jetstream_host", &mock.host()).await;
    mock.push(
        (1..=5).map(|s| js::create(s, "did:plc:tipafter", GAME, &format!("r{s}"), game("x"))),
    );
    mock.set_tip(4);
    mock.notice_next("FutureCursor", "cursor 12 is past the next seq 6");
    let _consumer = start(&app, settings(&mock));

    wait_until!(
        "the new seq 5 saved",
        setting(&app, "jetstream_seq").await.as_deref() == Some("5")
    );
    assert_eq!(mock.connections()[0].param("cursor").as_deref(), Some("12"));
    assert_eq!(rkeys(&app, "did:plc:tipafter").await, vec!["r4", "r5"]);
    assert_eq!(
        events(&app, "jetstream.gap").await,
        vec![json!({"reason": "future_cursor", "from_seq": 12})]
    );
}

/// The server retrains its dictionary: a reconnect offering the old one is
/// refused, and the consumer refetches and offers the new one.
#[tokio::test]
#[serial]
async fn a_rotated_dictionary_is_refetched() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    mock.push((1..=3).map(|s| js::create(s, "did:plc:dict", GAME, &format!("r{s}"), game("x"))));
    let _consumer = start(&app, settings(&mock));
    wait_until!("seq 3 applied", applied_through(&app, 3).await);

    mock.serve_dictionary(Some(js::DICTIONARY_B));
    mock.accept_dictionaries(&[js::DICTIONARY_B]);
    mock.push((4..=6).map(|s| js::create(s, "did:plc:dict", GAME, &format!("r{s}"), game("x"))));
    mock.kick();

    wait_until!("seq 6 applied", applied_through(&app, 6).await);
    let connections = mock.connections();
    assert!(
        connections.iter().any(|c| {
            c.param("zstdDictionary") == Some(js::DICTIONARY_A.to_string())
                && c.param("cursor").as_deref() == Some("3")
        }),
        "the reconnect first offered the retired dictionary: {connections:?}"
    );
    assert_eq!(
        connections
            .last()
            .expect("connections")
            .param("zstdDictionary"),
        Some(js::DICTIONARY_B.to_string())
    );
    assert_eq!(rkeys(&app, "did:plc:dict").await.len(), 6);
}

/// When refetching gives back the same refused dictionary, compression is
/// dropped rather than the stream.
#[tokio::test]
#[serial]
async fn compression_falls_back_when_the_dictionary_stays_unknown() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    mock.accept_dictionaries(&[]);
    mock.push((1..=3).map(|s| js::create(s, "did:plc:plain", GAME, &format!("r{s}"), game("x"))));
    let _consumer = start(&app, settings(&mock));

    wait_until!("seq 3 applied", applied_through(&app, 3).await);
    let connections = mock.connections();
    assert_eq!(
        connections[0].param("zstdDictionary"),
        Some(js::DICTIONARY_A.to_string())
    );
    assert_eq!(connections[1].param("zstdDictionary"), None);
    assert_eq!(rkeys(&app, "did:plc:plain").await.len(), 3);
}

/// `InvalidRequest` will not fix itself on a quick retry: it is logged as an
/// error and retried after `invalid_request_retry` (an hour in production).
#[tokio::test]
#[serial]
async fn an_invalid_request_is_logged_and_retried_later() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    mock.reject_next(400, "InvalidRequest", "collections: bad nsid");
    mock.push([js::create(1, "did:plc:invalid", GAME, "a", game("a"))]);
    let _consumer = start(&app, settings(&mock));

    wait_until!("seq 1 applied", applied_through(&app, 1).await);
    assert_eq!(
        events(&app, "jetstream.invalid_request").await,
        vec![json!({"message": "collections: bad nsid", "retry_in_secs": 0})]
    );
}

/// A 400 with no body to classify: first drop the dictionary, then the seq
/// cursor (for the witnessed time), and the third time in a row treat it as
/// `InvalidRequest`.
#[tokio::test]
#[serial]
async fn an_unexplained_400_steps_down_then_waits() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    set_setting(&app, "jetstream_seq", "5").await;
    set_setting(&app, "jetstream_cursor", &js::witnessed_us(5).to_string()).await;
    set_setting(&app, "jetstream_host", &mock.host()).await;
    for _ in 0..3 {
        mock.reject_next_bare(400);
    }
    mock.push((6..=8).map(|s| js::create(s, "did:plc:bare", GAME, &format!("r{s}"), game("x"))));
    let _consumer = start(&app, settings(&mock));

    wait_until!("seq 8 applied", applied_through(&app, 8).await);
    let connections = mock.connections();
    let shape: Vec<(Option<String>, Option<String>)> = connections
        .iter()
        .map(|c| (c.param("cursor"), c.param("zstdDictionary")))
        .collect();
    let time = js::witnessed_us(5).to_string();
    assert_eq!(
        shape,
        vec![
            (Some("5".into()), Some(js::DICTIONARY_A.to_string())),
            (Some("5".into()), None),
            (Some(time.clone()), None),
            (Some(time), None),
        ]
    );
    assert_eq!(
        events(&app, "jetstream.invalid_request").await,
        vec![json!({"message": "HTTP 400", "retry_in_secs": 0})]
    );
    assert_eq!(rkeys(&app, "did:plc:bare").await, vec!["r6", "r7", "r8"]);
}

/// An unexplained 400 on a time cursor (v1, or a pre-upgrade instance's
/// first v2 connect) never drops the cursor: dropping it would resume at
/// the tip and skip everything since the saved time. It goes straight to
/// the long wait instead, and keeps the cursor across those retries.
#[tokio::test]
#[serial]
async fn an_unexplained_400_keeps_a_time_cursor() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    let time = js::witnessed_us(5).to_string();
    set_setting(&app, "jetstream_cursor", &time).await;
    for _ in 0..3 {
        mock.reject_next_bare(400);
    }
    mock.push((6..=8).map(|s| js::create(s, "did:plc:keep", GAME, &format!("r{s}"), game("x"))));
    let _consumer = start(&app, settings(&mock));

    wait_until!("seq 8 applied", applied_through(&app, 8).await);
    let connections = mock.connections();
    assert_eq!(connections.len(), 4, "{connections:?}");
    assert!(
        connections
            .iter()
            .all(|c| c.param("cursor") == Some(time.clone())),
        "a connection dropped the cursor: {connections:?}"
    );
    assert_eq!(connections[1].param("zstdDictionary"), None);
    assert_eq!(
        events(&app, "jetstream.invalid_request").await.len(),
        2,
        "the second and third 400s both wait"
    );
    assert!(events(&app, "jetstream.gap").await.is_empty());
    assert_eq!(rkeys(&app, "did:plc:keep").await, vec!["r6", "r7", "r8"]);
}

/// A 503 between two unexplained 400s does not restart the ladder: the
/// second 400 is step 2. (Pins the current behaviour.)
#[tokio::test]
#[serial]
async fn a_503_between_unexplained_400s_does_not_reset_the_ladder() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    set_setting(&app, "jetstream_seq", "5").await;
    set_setting(&app, "jetstream_cursor", &js::witnessed_us(5).to_string()).await;
    set_setting(&app, "jetstream_host", &mock.host()).await;
    mock.reject_next_bare(400);
    mock.reject_next(503, "ServiceUnavailable", "warming up");
    mock.reject_next_bare(400);
    mock.push((6..=8).map(|s| js::create(s, "did:plc:503", GAME, &format!("r{s}"), game("x"))));
    let _consumer = start(&app, settings(&mock));

    wait_until!("seq 8 applied", applied_through(&app, 8).await);
    let shape: Vec<(Option<String>, Option<String>)> = mock
        .connections()
        .iter()
        .map(|c| (c.param("cursor"), c.param("zstdDictionary")))
        .collect();
    assert_eq!(
        shape,
        vec![
            (Some("5".into()), Some(js::DICTIONARY_A.to_string())),
            (Some("5".into()), None),
            (Some("5".into()), None),
            (Some(js::witnessed_us(5).to_string()), None),
        ]
    );
    assert!(events(&app, "jetstream.invalid_request").await.is_empty());
}

/// The dictionary is refused and refetching it fails: the consumer goes on
/// uncompressed.
#[tokio::test]
#[serial]
async fn compression_falls_back_when_the_dictionary_cannot_be_refetched() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    mock.push((1..=3).map(|s| js::create(s, "did:plc:nodict", GAME, &format!("r{s}"), game("x"))));
    let _consumer = start(&app, settings(&mock));
    wait_until!("seq 3 applied", applied_through(&app, 3).await);

    mock.serve_dictionary(None);
    mock.accept_dictionaries(&[]);
    mock.push((4..=6).map(|s| js::create(s, "did:plc:nodict", GAME, &format!("r{s}"), game("x"))));
    mock.kick();

    wait_until!("seq 6 applied", applied_through(&app, 6).await);
    let connections = mock.connections();
    assert_eq!(
        connections[1].param("zstdDictionary"),
        Some(js::DICTIONARY_A.to_string())
    );
    assert_eq!(
        connections
            .last()
            .expect("connections")
            .param("zstdDictionary"),
        None
    );
    assert_eq!(rkeys(&app, "did:plc:nodict").await.len(), 6);
}

/// The same gap met again on a retry with the same cursor is logged once.
#[tokio::test]
#[serial]
async fn a_repeated_gap_is_logged_once() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    let time = js::witnessed_us(1).to_string();
    set_setting(&app, "jetstream_cursor", &time).await;
    mock.notice_next("OutdatedCursor", "cursor clamped to seq 90");
    mock.notice_next("OutdatedCursor", "cursor clamped to seq 90");
    let _consumer = start(&app, settings(&mock));
    wait_until!("a connection", !mock.connections().is_empty());

    mock.push([js::create(2, "did:plc:once", GAME, "a", game("a"))]);
    mock.kick();
    wait_until!("seq 2 applied", applied_through(&app, 2).await);
    assert_eq!(mock.connections()[1].param("cursor"), Some(time));
    assert_eq!(
        events(&app, "jetstream.gap").await,
        vec![json!({"reason": "outdated_cursor", "message": "cursor clamped to seq 90"})]
    );
}

/// `FutureCursor` arrives while an event of the old seq numbering is still
/// applying. When it finishes, its seq (ahead of the new numbering's) must
/// not be saved: after a crash the consumer would resume past every new
/// event up to it. Nothing is saved until a new-numbering event applies,
/// and a restart then resumes in the new numbering.
///
/// Postgres only: it holds two events back independently, and the SQLite
/// blocker holds back every write.
#[tokio::test]
#[serial]
async fn an_old_numbering_seq_is_not_saved_after_a_future_cursor() {
    let Some(app) = app().await else { return };
    if app.state.db_backend == DatabaseBackend::Sqlite {
        eprintln!("skipped (needs Postgres)");
        return;
    }
    let mock = MockJetstream::start().await;
    let (old_did, new_did) = ("did:plc:oldnumbering", "did:plc:newnumbering");
    let old = |s: i64| js::create(s, old_did, GAME, &format!("r{s}"), game("x"));
    // Renumbered from 1, witnessed after the old seqs were.
    let new = |s: i64| {
        js::witnessed_later(
            js::create(s, new_did, GAME, &format!("r{s}"), game("x")),
            js::witnessed_us(20) - js::BASE_US,
        )
    };
    let fast_drain = || Settings {
        drain_timeout: Duration::from_millis(200),
        ..settings(&mock)
    };
    mock.push((10..=11).map(old));
    let base = received(&app);
    let consumer = start(&app, fast_drain());
    wait_until!("seq 11 applied", applied_through(&app, 11).await);

    let old_blocker = Blocker::hold(&app, &format!("at://{old_did}/{GAME}/r12")).await;
    mock.push([old(12)]);
    mock.kick();
    wait_until!("seq 12 dispatched", received(&app) == base + 3);

    let seqs = SeqWatch::start(&app);
    let new_blocker = Blocker::hold(&app, &format!("at://{new_did}/{GAME}/r1")).await;
    mock.reset_events();
    mock.push((1..=3).map(new));
    mock.notice_next("FutureCursor", "cursor 12 is past the next seq 4");
    mock.kick();
    wait_until!("the new seqs dispatched", received(&app) == base + 6);

    old_blocker.release().await;
    wait_until!(
        "old seq 12 applied",
        rkeys(&app, old_did).await.contains(&"r12".to_string())
    );
    let connections = mock.connections().len();
    mock.kick();
    wait_until!(
        "a session end, which saves the cursor",
        mock.connections().len() > connections
    );
    assert_eq!(setting(&app, "jetstream_seq").await.as_deref(), Some("11"));
    assert!(
        !seqs.seen().contains(&12),
        "an old-numbering seq was saved: {:?}",
        seqs.seen()
    );

    // A crash before any new event applied: the restart resumes at the
    // saved 11, the server answers `FutureCursor` again, and nothing of the
    // new numbering is skipped.
    drop(consumer);
    new_blocker.release().await;
    mock.set_tip(4);
    mock.notice_next("FutureCursor", "cursor 11 is past the next seq 4");
    let _consumer = start(&app, fast_drain());
    wait_until!(
        "the new seq 3 saved",
        setting(&app, "jetstream_seq").await.as_deref() == Some("3")
    );
    assert_eq!(rkeys(&app, new_did).await, vec!["r1", "r2", "r3"]);
    assert_eq!(
        mock.connections()
            .last()
            .expect("connections")
            .param("cursor"),
        Some(js::witnessed_us(11).to_string()),
        "the restart resumed by witnessed time, not from the tip"
    );
    assert!(!seqs.seen().contains(&12), "{:?}", seqs.seen());
}

/// A `CursorTooOld` resume from the tip that is refused a few more times
/// (the retries still have no cursor): the gap is logged once the tip
/// connection finally succeeds.
#[tokio::test]
#[serial]
async fn a_cursor_too_old_gap_outlasts_the_refusals_after_it() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    set_setting(&app, "jetstream_seq", "5").await;
    set_setting(&app, "jetstream_host", &mock.host()).await;
    mock.reject_next(400, "CursorTooOld", "cursor 5 below lookback floor 90");
    for _ in 0..3 {
        mock.reject_next_bare(400);
    }
    mock.set_retention_floor(90);
    mock.push(
        (100..=102).map(|s| js::create(s, "did:plc:outlast", GAME, &format!("r{s}"), game("x"))),
    );
    let _consumer = start(&app, settings(&mock));

    wait_until!("seq 102 applied", applied_through(&app, 102).await);
    let connections = mock.connections();
    assert_eq!(connections.len(), 5, "{connections:?}");
    assert!(
        connections[1..].iter().all(|c| c.param("cursor").is_none()),
        "{connections:?}"
    );
    assert_eq!(
        events(&app, "jetstream.invalid_request").await.len(),
        2,
        "the second and third unexplained 400s wait"
    );
    assert_eq!(
        events(&app, "jetstream.gap").await,
        vec![json!({
            "reason": "cursor_too_old",
            "message": "cursor 5 below lookback floor 90",
            "after_seq": 5,
            "resumed_at_seq": 100,
        })]
    );
}

/// `FutureCursor` in answer to a time cursor (which should not happen):
/// reconnecting by that time would only meet it again, so the consumer
/// goes on from the tip instead of looping.
#[tokio::test]
#[serial]
async fn a_future_cursor_on_a_time_cursor_does_not_loop() {
    let Some(app) = app().await else { return };
    let mock = MockJetstream::start().await;
    let time = js::witnessed_us(5);
    set_setting(&app, "jetstream_cursor", &time.to_string()).await;
    for _ in 0..5 {
        mock.notice_next("FutureCursor", "cursor is past the next seq");
    }
    mock.push((6..=7).map(|s| js::create(s, "did:plc:noloop", GAME, &format!("r{s}"), game("x"))));
    let _consumer = start(&app, settings(&mock));

    wait_until!("seq 7 applied", applied_through(&app, 7).await);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let connections = mock.connections();
    assert!(
        connections.len() <= 2,
        "reconnected in a loop: {connections:?}"
    );
    assert_eq!(rkeys(&app, "did:plc:noloop").await, vec!["r6", "r7"]);
    assert_eq!(
        events(&app, "jetstream.gap").await,
        vec![json!({"reason": "future_cursor", "from_seq": time})]
    );
}
