//! The Jetstream consumer against a scripted server (`common::jetstream`).
//! Needs `TEST_DATABASE_URL`; serial, because the tests share one database
//! and the process-wide status.

mod common;

use std::time::Duration;

use happyview::db::adapt_sql;
use happyview::jetstream::config::{ProtocolChoice, Settings, parse_endpoint};
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
    }
}

fn start(app: &TestApp, settings: Settings) -> Consumer {
    let (tx, rx) = watch::channel(vec![GAME.to_string()]);
    let mut state = app.state.clone();
    state.config.jetstream_url = settings.endpoint.ws_base.clone();
    Consumer {
        handle: happyview::jetstream::spawn_with(state, rx, settings),
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
/// workers: every one ends deleted, because a DID's events never race.
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
