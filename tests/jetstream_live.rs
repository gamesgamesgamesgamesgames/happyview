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
/// backwards.
struct SeqWatch {
    went_back: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl SeqWatch {
    fn start(app: &TestApp) -> Self {
        let went_back = Arc::new(AtomicBool::new(false));
        let (db, backend) = (app.state.db.clone(), app.state.db_backend);
        let flag = Arc::clone(&went_back);
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
                    if seq < highest {
                        flag.store(true, Ordering::SeqCst);
                    }
                    highest = highest.max(seq);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        Self { went_back, task }
    }

    fn went_back(&self) -> bool {
        self.went_back.load(Ordering::SeqCst)
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
