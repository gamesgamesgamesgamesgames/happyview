//! Replay backfill against a mock Jetstream archive. Needs TEST_DATABASE_URL
//! for the database tests; the writer and mock tests run without it.

mod common;

use common::app::TestApp;
use common::jetstream::{self as js, MockJetstream};
use common::segment_writer::write_segment;
use happyview::jetstream::archive::SourceChoice;
use happyview::jetstream::archive::client::{ArchiveClient, PlanMode, PlanRequest};
use happyview::jetstream::archive::{ArchiveConfig, rows, segment};
use happyview::jetstream::config::{DRAIN_TIMEOUT, ProtocolChoice, Settings, parse_endpoint};
use happyview::jetstream::workers::RetryPolicy;
use serde_json::json;
use serial_test::serial;
use std::io::Cursor;
use std::time::Duration;

const GAME: &str = "games.gamesgamesgamesgames.game";

fn game(title: &str) -> serde_json::Value {
    json!({ "$type": GAME, "title": title, "createdAt": "2026-10-12T00:00:00.000Z" })
}

#[test]
fn the_test_writer_round_trips_through_the_reader() {
    let events = vec![
        js::archive_create(1, "did:plc:a", GAME, "r1", game("one")),
        js::archive_update(2, "did:plc:a", GAME, "r1", game("two")),
        js::archive_delete(3, "did:plc:a", GAME, "r1"),
        js::archive_account(4, "did:plc:a", false, Some("takendown")),
        js::archive_sync(5, "did:plc:b"),
        js::archive_identity(6, "did:plc:c"),
        js::archive_create(
            7,
            "did:plc:b",
            "app.other.thing",
            "r2",
            json!({ "$type": "app.other.thing" }),
        ),
    ];
    let written = write_segment(&events, 3);
    assert_eq!(written.blocks.len(), 3);
    let mut file = Cursor::new(&written.bytes);
    let index = segment::read_index(&mut file, written.bytes.len() as u64).unwrap();
    let mut rows_read = Vec::new();
    for entry in &index {
        rows_read.extend(
            segment::decode_block(&segment::read_block_frame(&mut file, entry).unwrap()).unwrap(),
        );
    }
    assert_eq!(rows_read.len(), events.len());
    for (row, event) in rows_read.iter().zip(&events) {
        assert_eq!(
            (row.seq, row.kind, &row.did, &row.payload),
            (event.seq, event.kind, &event.did, &event.payload)
        );
        assert_eq!(row.witnessed_at, event.witnessed_at);
    }
    // And the conversion agrees with the live mock's events of the same seq.
    let created = rows::row_to_event(rows_read[0].clone()).unwrap().unwrap();
    let live = js::create(1, "did:plc:a", GAME, "r1", game("one"));
    let happyview::jetstream::event::EventKind::Commit(commit) = created.kind else {
        panic!()
    };
    assert_eq!(commit.cid.as_deref(), live["cid"].as_str());
    assert_eq!(commit.rev.as_deref(), live["rev"].as_str());
    assert_eq!(commit.record.as_ref(), Some(&live["record"]));
}

#[tokio::test]
async fn the_mock_archive_plans_pages_and_serves_what_it_planned() {
    let mock = MockJetstream::start().await;
    let a = mock.seal(
        (1..=8)
            .map(|s| js::archive_create(s, "did:plc:a", GAME, &format!("r{s}"), game("x")))
            .collect(),
        4,
    );
    let b = mock.seal(
        (9..=16)
            .map(|s| {
                if s == 12 {
                    js::archive_create(s, "did:plc:a", GAME, "r12", game("x"))
                } else {
                    js::archive_create(
                        s,
                        "did:plc:b",
                        "app.other.thing",
                        &format!("o{s}"),
                        json!({ "$type": "app.other.thing" }),
                    )
                }
            })
            .collect(),
        4,
    );
    mock.set_plan_page_entries(1);
    let endpoint = happyview::jetstream::config::parse_endpoint(&mock.url());
    let client = ArchiveClient::new(reqwest::Client::new(), &endpoint, &ArchiveConfig::default());
    let request = |after, before| PlanRequest {
        kinds: vec!["commit".into()],
        collections: vec![GAME.into()],
        after_seq: after,
        before_seq: before,
        ..PlanRequest::default()
    };
    let first = client.plan(&request(0, None)).await.unwrap();
    assert_eq!((first.planned_through_seq, first.sealed_tip_seq), (8, 16));
    assert_eq!(first.segments[0].name, a);
    assert_eq!(first.segments[0].mode, PlanMode::Segment);
    let second = client.plan(&request(8, Some(16))).await.unwrap();
    assert_eq!(second.planned_through_seq, 16);
    assert_eq!(second.segments[0].name, b);
    assert_eq!(
        second.segments[0].mode,
        PlanMode::Blocks,
        "1 of 2 blocks matches"
    );
    assert_eq!(
        (
            second.segments[0].blocks[0].first,
            second.segments[0].blocks[0].last
        ),
        (0, 0)
    );
    let frame = client.block(&b, 0).await.unwrap();
    assert_eq!(segment::decode_block(&frame).unwrap().len(), 4);

    let path = std::env::temp_dir().join(format!("hv-mock-{}", uuid::Uuid::new_v4()));
    client.segment(&a, &path).await.unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(
        segment::read_index(&mut Cursor::new(&bytes), bytes.len() as u64)
            .unwrap()
            .len(),
        2
    );
    std::fs::remove_file(&path).ok();
    assert_eq!(mock.segment_downloads(), vec![a]);
    assert_eq!(mock.plan_requests().len(), 2);
}

macro_rules! wait_until {
    ($what:expr, $cond:expr) => {{
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        loop {
            if $cond {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {}",
                $what
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }};
}

/// A TestApp tracking GAME, pointed at `mock`, whose replay settings use a
/// fresh download dir, fast retries and `key`.
async fn app(mock: &MockJetstream, key: Option<&str>) -> Option<TestApp> {
    std::env::var("TEST_DATABASE_URL").ok()?;
    let mut app = TestApp::new().await;
    // Upload through the API so the lexicon is in both `happyview_lexicons`
    // (a job without a collection, like a gap replay, lists record lexicons
    // from the table) and the in-memory registry. `backfill: false`, or the
    // upload starts a job of its own.
    let (status, body) = app
        .post_json_status(
            "/admin/lexicons",
            json!({ "lexicon_json": common::fixtures::game_record_lexicon(), "backfill": false }),
        )
        .await;
    assert!(status == 200 || status == 201, "{body}");
    app.state.config.jetstream_url = mock.url();
    app.state.archive = ArchiveConfig {
        api_key: key.map(str::to_string),
        download_dir: std::env::temp_dir().join(format!("hv-replay-{}", uuid::Uuid::new_v4())),
        retry_unit: Duration::from_millis(5),
        ..ArchiveConfig::default()
    };
    app.rebuild_router();
    Some(app)
}

async fn start_job(app: &TestApp, body: serde_json::Value) -> String {
    let (status, json) = app.post_json_status("/admin/backfill", body).await;
    assert_eq!(status, 201, "{json}");
    json["id"].as_str().expect("job id").to_string()
}

async fn job(app: &TestApp, id: &str) -> serde_json::Value {
    let jobs = app.get_json("/admin/backfill/status").await;
    jobs.as_array()
        .unwrap()
        .iter()
        .find(|j| j["id"] == id)
        .cloned()
        .expect("job listed")
}

async fn finished(app: &TestApp, id: &str) -> serde_json::Value {
    wait_until!("the job to finish", {
        let status = job(app, id).await["status"].as_str().unwrap().to_string();
        matches!(
            status.as_str(),
            "completed" | "failed" | "cancelled" | "paused"
        )
    });
    job(app, id).await
}

/// `uri, cid, record, rev` of every network record, sorted.
async fn dump(app: &TestApp) -> Vec<(String, String, String, Option<String>)> {
    happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT uri, cid, record, rev FROM happyview_records WHERE indexed_at IS NOT NULL ORDER BY uri",
        app.state.db_backend,
    ))
    .fetch_all(&app.state.db)
    .await
    .expect("dump")
}

/// The TestApp's relay (wiremock) lists no repos, so a crawl completes at
/// once instead of failing discovery on an unmounted route.
async fn empty_relay(app: &TestApp) {
    wiremock::Mock::given(wiremock::matchers::path(
        "/xrpc/com.atproto.sync.listReposByCollection",
    ))
    .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({ "repos": [] })))
    .mount(&app.mock_server)
    .await;
}

fn games(
    range: std::ops::RangeInclusive<i64>,
    did: &str,
) -> Vec<common::segment_writer::ArchiveEvent> {
    range
        .map(|s| js::archive_create(s, did, GAME, &format!("g{s:04}"), game(&format!("t{s}"))))
        .collect()
}

#[tokio::test]
#[serial]
async fn replay_matches_applying_the_same_events_live() {
    let mock = MockJetstream::start().await;
    let Some(app) = app(&mock, None).await else {
        return;
    };
    let (a, b) = ("did:plc:replaya", "did:plc:replayb");
    let mut events = games(1..=30, a);
    events.extend(games(31..=40, b));
    events.push(js::archive_update(41, a, GAME, "g0003", game("edited")));
    events.push(js::archive_delete(42, a, GAME, "g0004"));
    events.push(js::archive_create(
        43,
        b,
        "app.other.thing",
        "o1",
        json!({ "$type": "app.other.thing" }),
    ));
    events.push(js::archive_sync(44, b));
    events.push(js::archive_create(45, b, GAME, "g0045", game("after sync")));
    events.push(js::archive_identity(46, a));
    let all = events.clone();
    mock.seal(events.drain(..20).collect(), 4);
    mock.seal(events, 4);

    let id = start_job(&app, json!({ "collection": GAME, "source": "replay" })).await;
    let job = finished(&app, &id).await;
    assert_eq!(job["status"], "completed", "{job}");
    assert_eq!(job["source"], "replay");
    let replayed = dump(&app).await;

    common::db::truncate_all(&app.state.db).await;
    for event in all {
        let row = happyview::jetstream::archive::segment::Row {
            seq: event.seq,
            witnessed_at: event.witnessed_at,
            indexed_at: 0,
            kind: event.kind,
            collection: event.collection,
            did: event.did,
            rkey: event.rkey,
            rev: event.rev,
            payload: event.payload,
        };
        if let Some(event) = rows::row_to_event(row).unwrap() {
            happyview::jetstream::apply::apply_event(&app.state, &[GAME.to_string()], event).await;
        }
    }
    assert_eq!(dump(&app).await, replayed);
    assert!(replayed.iter().all(|(uri, ..)| uri.contains(GAME)));
    assert!(
        !replayed.iter().any(|(uri, ..)| uri.ends_with("/g0004")),
        "deleted"
    );
    assert!(
        !replayed
            .iter()
            .any(|(uri, ..)| uri.starts_with(&format!("at://{b}/{GAME}/g003"))),
        "purged by sync"
    );
}

#[tokio::test]
#[serial]
async fn commit_sweeps_ask_for_commits_only() {
    let mock = MockJetstream::start().await;
    let Some(app) = app(&mock, None).await else {
        return;
    };
    mock.seal(games(1..=5, "did:plc:kinds"), 4);
    let id = start_job(&app, json!({ "collection": GAME, "source": "replay" })).await;
    assert_eq!(finished(&app, &id).await["status"], "completed");
    let plans = mock.plan_requests();
    let commits: Vec<_> = plans
        .iter()
        .filter(|p| p.get("collections").is_some())
        .collect();
    assert!(!commits.is_empty());
    for plan in &commits {
        assert_eq!(plan["kinds"], json!(["commit"]), "{plan}");
        assert_eq!(plan["collections"], json!([GAME]));
    }
    for plan in plans.iter().filter(|p| p.get("collections").is_none()) {
        assert_eq!(plan["kinds"], json!(["account", "sync"]), "{plan}");
        assert!(
            plan["dids"]
                .as_array()
                .is_some_and(|d| !d.is_empty() && d.len() <= 10_000)
        );
    }
}

#[tokio::test]
#[serial]
async fn a_network_job_replays_account_markers_for_the_dids_it_holds() {
    let mock = MockJetstream::start().await;
    let Some(app) = app(&mock, None).await else {
        return;
    };
    let (kept, taken) = ("did:plc:kept", "did:plc:taken");
    let mut events = games(1..=4, kept);
    events.extend(games(5..=8, taken));
    events.push(js::archive_account(9, taken, false, Some("takendown")));
    events.push(js::archive_account(
        10,
        "did:plc:stranger",
        false,
        Some("deleted"),
    ));
    mock.seal(events, 4);
    let id = start_job(&app, json!({ "collection": GAME, "source": "replay" })).await;
    assert_eq!(finished(&app, &id).await["status"], "completed");
    let uris: Vec<String> = dump(&app).await.into_iter().map(|r| r.0).collect();
    assert_eq!(uris.len(), 4);
    assert!(uris.iter().all(|u| u.contains(kept)));
    let marker_plan = mock
        .plan_requests()
        .into_iter()
        .find(|p| p["kinds"] == json!(["account", "sync"]))
        .expect("a marker sweep");
    let mut dids: Vec<String> = serde_json::from_value(marker_plan["dids"].clone()).unwrap();
    dids.sort();
    assert_eq!(
        dids,
        vec![kept.to_string(), taken.to_string()],
        "only DIDs holding records"
    );
}

#[tokio::test]
#[serial]
async fn replay_never_resurrects_a_record_deleted_live() {
    let mock = MockJetstream::start().await;
    let Some(app) = app(&mock, None).await else {
        return;
    };
    let did = "did:plc:livedelete";
    mock.seal(games(1..=4, did), 4);
    // Live ingest saw the create at seq 2, then its delete at seq 50, before
    // the backfill started.
    for event in [
        js::create(2, did, GAME, "g0002", game("t2")),
        js::delete(50, did, GAME, "g0002"),
    ] {
        let happyview::jetstream::event::Frame::Event(event) =
            happyview::jetstream::event::parse_v2(
                &json!({ "$type": "message", "payload": event }).to_string(),
            )
            .unwrap()
        else {
            panic!("an event")
        };
        happyview::jetstream::apply::apply_event(&app.state, &[GAME.to_string()], event).await;
    }
    let id = start_job(&app, json!({ "collection": GAME, "source": "replay" })).await;
    assert_eq!(finished(&app, &id).await["status"], "completed");
    let uris: Vec<String> = dump(&app).await.into_iter().map(|r| r.0).collect();
    assert_eq!(uris.len(), 3);
    assert!(!uris.iter().any(|u| u.ends_with("/g0002")));
}

#[tokio::test]
#[serial]
async fn auto_falls_back_to_crawl_when_the_archive_answers_404() {
    let mock = MockJetstream::start().await;
    mock.disable_archive(404);
    let Some(app) = app(&mock, None).await else {
        return;
    };
    // The crawl finds nothing: the relay mock lists no repos. The point is
    // that the job runs as a crawl and completes.
    empty_relay(&app).await;
    let id = start_job(&app, json!({ "collection": GAME, "source": "auto" })).await;
    let job = finished(&app, &id).await;
    assert_eq!(job["source"], "crawl", "{job}");
    assert_eq!(job["status"], "completed", "{job}");
}

#[tokio::test]
#[serial]
async fn auto_falls_back_to_crawl_when_nothing_listens() {
    let mock = MockJetstream::start().await;
    let Some(mut app) = app(&mock, None).await else {
        return;
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let closed = listener.local_addr().unwrap();
    drop(listener);
    app.state.config.jetstream_url = format!("ws://{closed}");
    app.rebuild_router();
    empty_relay(&app).await;
    let id = start_job(&app, json!({ "collection": GAME })).await;
    // No `source`: the TestApp default is crawl, so ask for auto explicitly too.
    let job = finished(&app, &id).await;
    assert_eq!(job["source"], "crawl");
    let id = start_job(&app, json!({ "collection": GAME, "source": "auto" })).await;
    let job = finished(&app, &id).await;
    assert_eq!(
        (job["source"].as_str(), job["status"].as_str()),
        (Some("crawl"), Some("completed")),
        "{job}"
    );
}

#[tokio::test]
#[serial]
async fn a_replay_job_without_an_archive_fails_with_a_reason() {
    let mock = MockJetstream::start().await;
    mock.disable_archive(501);
    let Some(app) = app(&mock, None).await else {
        return;
    };
    let id = start_job(&app, json!({ "collection": GAME, "source": "replay" })).await;
    let job = finished(&app, &id).await;
    assert_eq!(job["status"], "failed");
    assert_eq!(
        job["error"],
        format!(
            "the jetstream at {} has no replay archive (HTTP 501)",
            mock.host()
        )
    );
}

#[tokio::test]
#[serial]
async fn an_unknown_source_is_refused() {
    let mock = MockJetstream::start().await;
    let Some(app) = app(&mock, None).await else {
        return;
    };
    let (status, body) = app
        .post_json_status(
            "/admin/backfill",
            json!({ "collection": GAME, "source": "relay" }),
        )
        .await;
    assert_eq!(status, 400);
    assert!(
        body.to_string()
            .contains("source must be one of auto, replay, crawl"),
        "{body}"
    );
}

#[tokio::test]
#[serial]
async fn an_account_job_filters_by_did_and_replays_its_markers() {
    let mock = MockJetstream::start().await;
    let Some(app) = app(&mock, None).await else {
        return;
    };
    let (wanted, other) = ("did:plc:wanted", "did:plc:other");
    let mut events = games(1..=4, wanted);
    events.extend(games(5..=8, other));
    events.push(js::archive_sync(9, wanted));
    events.push(js::archive_create(
        10,
        wanted,
        GAME,
        "g0010",
        game("replacement"),
    ));
    mock.seal(events, 4);
    // `dids` must already be DIDs: the TestApp's identity resolver accepts them.
    let id = start_job(
        &app,
        json!({ "collection": GAME, "source": "replay", "dids": [wanted] }),
    )
    .await;
    assert_eq!(finished(&app, &id).await["status"], "completed");
    let uris: Vec<String> = dump(&app).await.into_iter().map(|r| r.0).collect();
    assert_eq!(
        uris,
        vec![format!("at://{wanted}/{GAME}/g0010")],
        "the sync superseded g1-g4"
    );
    for plan in mock.plan_requests() {
        assert_eq!(plan["dids"], json!([wanted]), "{plan}");
    }
}

#[tokio::test]
#[serial]
async fn paging_pins_the_tip_and_ignores_segments_sealed_later() {
    let mock = MockJetstream::start().await;
    let Some(app) = app(&mock, None).await else {
        return;
    };
    mock.set_plan_page_entries(1);
    mock.seal(games(1..=4, "did:plc:page"), 4);
    mock.seal(games(5..=8, "did:plc:page"), 4);
    let id = start_job(&app, json!({ "collection": GAME, "source": "replay" })).await;
    // `#[tokio::test]` runs on one thread, so the job has not planned yet.
    // Wait until the first page has been answered (the mock computes a page
    // in the same poll that records its request), then seal a third
    // segment: it lies past the tip that page pinned.
    wait_until!("the first plan request", !mock.plan_requests().is_empty());
    mock.seal(games(9..=12, "did:plc:page"), 4);
    let job = finished(&app, &id).await;
    assert_eq!(job["status"], "completed", "{job}");
    let plans: Vec<_> = mock
        .plan_requests()
        .into_iter()
        .filter(|p| p.get("collections").is_some())
        .collect();
    assert!(
        plans.len() >= 2,
        "one entry a page, two segments: {plans:?}"
    );
    assert!(plans[0].get("beforeSeq").is_none());
    assert!(
        plans[1..].iter().all(|p| p["beforeSeq"] == json!(8)),
        "{plans:?}"
    );
    // Only the two segments under the pinned tip were replayed. The job's
    // `replay.tip_seq` is not checked: it mirrors the sweep running last, the
    // marker sweep, which pins its own tip after the third seal.
    assert_eq!(dump(&app).await.len(), 8);
}

#[tokio::test]
#[serial]
async fn bytes_downloaded_are_counted_on_the_job() {
    let mock = MockJetstream::start().await;
    let Some(app) = app(&mock, None).await else {
        return;
    };
    mock.seal(games(1..=8, "did:plc:bytes"), 4);
    let id = start_job(&app, json!({ "collection": GAME, "source": "replay" })).await;
    let job = finished(&app, &id).await;
    assert!(job["replay"]["bytes"].as_i64().unwrap() > 256, "{job}");
    assert_eq!(job["replay"]["blocks_failed"], 0);
    assert_eq!(job["total_records"], 8);
}

#[tokio::test]
#[serial]
async fn the_live_stream_never_sees_the_api_key() {
    let mock = MockJetstream::start().await;
    let Some(app) = app(&mock, Some("s3cret")).await else {
        return;
    };
    mock.seal(games(1..=4, "did:plc:key"), 4);
    let id = start_job(&app, json!({ "collection": GAME, "source": "replay" })).await;
    assert_eq!(finished(&app, &id).await["status"], "completed");
    let (tx, rx) = tokio::sync::watch::channel(vec![GAME.to_string()]);
    let consumer = happyview::jetstream::spawn(app.state.clone(), rx);
    wait_until!("a live connection", !mock.connections().is_empty());
    consumer.abort();
    drop(tx);
    let authorizations = mock.authorizations();
    for route in ["plan", "segment", "v2"] {
        assert!(
            authorizations.iter().any(|(r, _)| r == route),
            "no {route} request: {authorizations:?}"
        );
    }
    for (route, auth) in authorizations {
        if matches!(route.as_str(), "plan" | "segment" | "block") {
            assert_eq!(auth.as_deref(), Some("Bearer s3cret"), "{route}");
        } else {
            assert_eq!(auth, None, "{route}");
        }
    }
}

#[tokio::test]
#[serial]
async fn a_resumed_job_does_not_refetch_finished_entries() {
    let mock = MockJetstream::start().await;
    let Some(app) = app(&mock, None).await else {
        return;
    };
    mock.set_plan_page_entries(1);
    let first = mock.seal(games(1..=4, "did:plc:resume"), 4);
    let second = mock.seal(games(5..=8, "did:plc:resume"), 4);
    // Hold the second segment's download, so the job is known to be inside
    // the second entry, with the first one finished, when it is paused.
    mock.hold_segment(&second);
    let id = start_job(&app, json!({ "collection": GAME, "source": "replay" })).await;
    wait_until!(
        "the second download to start, after the first entry",
        mock.segment_downloads().contains(&second) && dump(&app).await.len() == 4
    );
    let (status, body) = app
        .post_json_status(&format!("/admin/backfill/{id}/pause"), json!({}))
        .await;
    assert_eq!(status, 200, "{body}");
    // The entry in flight finishes, and the job stops at its next check.
    mock.release_segment(&second);
    assert_eq!(finished(&app, &id).await["status"], "paused");

    let (status, body) = app
        .post_json_status(&format!("/admin/backfill/{id}/resume"), json!({}))
        .await;
    assert_eq!(status, 200, "{body}");
    let job = finished(&app, &id).await;
    assert_eq!(job["status"], "completed");
    assert_eq!(
        job["total_records"], 8,
        "records from before the pause still count: {job}"
    );
    let downloads = mock.segment_downloads();
    assert_eq!(
        downloads.iter().filter(|n| **n == first).count(),
        1,
        "{downloads:?}"
    );
    assert_eq!(
        downloads.iter().filter(|n| **n == second).count(),
        1,
        "{downloads:?}"
    );
    assert_eq!(dump(&app).await.len(), 8);
}

/// 16 events in 4 blocks of which only block 2 holds GAME records, so the
/// plan asks for that one block (`mode: "blocks"`).
fn one_game_block(did: &str) -> Vec<common::segment_writer::ArchiveEvent> {
    (1..=16)
        .map(|s| {
            if (9..=12).contains(&s) {
                js::archive_create(s, did, GAME, &format!("g{s:04}"), game("t"))
            } else {
                js::archive_create(
                    s,
                    "did:plc:noise",
                    "app.other.thing",
                    &format!("o{s}"),
                    json!({ "$type": "app.other.thing" }),
                )
            }
        })
        .collect()
}

async fn errors(app: &TestApp, job: &str) -> Vec<(String, String, String)> {
    happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT did, phase, kind FROM happyview_backfill_errors WHERE job_id = ? ORDER BY did, phase",
        app.state.db_backend,
    ))
    .bind(job)
    .fetch_all(&app.state.db)
    .await
    .expect("errors")
}

#[tokio::test]
#[serial]
async fn a_failed_block_is_recorded_and_retry_failed_refetches_just_it() {
    let mock = MockJetstream::start().await;
    let Some(app) = app(&mock, None).await else {
        return;
    };
    let segment = mock.seal(one_game_block("did:plc:retry"), 4);
    mock.fail_block(&segment, 2, usize::MAX);
    let id = start_job(&app, json!({ "collection": GAME, "source": "replay" })).await;
    let job = finished(&app, &id).await;
    assert_eq!(
        job["status"], "completed",
        "a failed block doesn't fail the job"
    );
    assert_eq!(job["replay"]["blocks_failed"], 1);
    assert_eq!(
        errors(&app, &id).await,
        vec![(
            segment.clone(),
            "replay_block:2".into(),
            "replay_block".into()
        )]
    );
    assert!(dump(&app).await.is_empty());

    mock.fail_block(&segment, 2, 0);
    let plans_before = mock.plan_requests().len();
    let block_2 = |mock: &MockJetstream| {
        mock.block_fetches()
            .iter()
            .filter(|(s, i)| *s == segment && *i == 2)
            .count()
    };
    let fetches_before = block_2(&mock);
    let (status, body) = app
        .post_json_status(&format!("/admin/backfill/{id}/retry-failed"), json!({}))
        .await;
    assert_eq!(status, 201, "{body}");
    let retry = body["id"].as_str().unwrap().to_string();
    let job = finished(&app, &retry).await;
    assert_eq!(job["status"], "completed", "{job}");
    assert_eq!(job["source"], "replay");
    assert_eq!(dump(&app).await.len(), 4);
    assert_eq!(
        mock.plan_requests().len(),
        plans_before,
        "a retry plans nothing"
    );
    assert_eq!(
        block_2(&mock),
        fetches_before + 1,
        "exactly one more fetch of block 2"
    );
}

#[tokio::test]
#[serial]
async fn a_corrupt_block_is_recorded_as_a_failed_block() {
    let mock = MockJetstream::start().await;
    let Some(app) = app(&mock, None).await else {
        return;
    };
    let segment = mock.seal(one_game_block("did:plc:corrupt"), 4);
    mock.corrupt_block(&segment, 2);
    let id = start_job(&app, json!({ "collection": GAME, "source": "replay" })).await;
    assert_eq!(finished(&app, &id).await["replay"]["blocks_failed"], 1);
    assert_eq!(errors(&app, &id).await[0].1, "replay_block:2");
}

#[tokio::test]
#[serial]
async fn a_segment_that_fails_to_download_is_retried_whole() {
    let mock = MockJetstream::start().await;
    let Some(app) = app(&mock, None).await else {
        return;
    };
    let segment = mock.seal(games(1..=8, "did:plc:wholeseg"), 4);
    mock.fail_segment(&segment);
    let id = start_job(&app, json!({ "collection": GAME, "source": "replay" })).await;
    assert_eq!(finished(&app, &id).await["status"], "completed");
    assert_eq!(
        errors(&app, &id).await,
        vec![(
            segment.clone(),
            "replay_segment".into(),
            "replay_block".into()
        )]
    );
    mock.fail_segment(&segment);
    let (status, body) = app
        .post_json_status(&format!("/admin/backfill/{id}/retry-failed"), json!({}))
        .await;
    assert_eq!(status, 201, "{body}");
    assert_eq!(
        finished(&app, body["id"].as_str().unwrap()).await["status"],
        "completed"
    );
    assert_eq!(dump(&app).await.len(), 8);
}

#[tokio::test]
#[serial]
async fn retry_failed_without_block_failures_is_refused() {
    let mock = MockJetstream::start().await;
    let Some(app) = app(&mock, None).await else {
        return;
    };
    mock.seal(games(1..=4, "did:plc:clean"), 4);
    let id = start_job(&app, json!({ "collection": GAME, "source": "replay" })).await;
    assert_eq!(finished(&app, &id).await["status"], "completed");
    let (status, body) = app
        .post_json_status(&format!("/admin/backfill/{id}/retry-failed"), json!({}))
        .await;
    assert_eq!(status, 400, "{body}");
}

#[tokio::test]
#[serial]
async fn retrying_an_account_job_whose_dids_are_gone_is_refused() {
    let mock = MockJetstream::start().await;
    let Some(app) = app(&mock, None).await else {
        return;
    };
    let did = "did:plc:gonedids";
    let segment = mock.seal(one_game_block(did), 4);
    mock.fail_block(&segment, 2, usize::MAX);
    let id = start_job(
        &app,
        json!({ "collection": GAME, "source": "replay", "dids": [did] }),
    )
    .await;
    assert_eq!(finished(&app, &id).await["replay"]["blocks_failed"], 1);
    // What a detail flush or the retention sweep does to a finished job.
    happyview::db::query(&happyview::db::adapt_sql(
        "DELETE FROM happyview_backfill_queue WHERE job_id = ?",
        app.state.db_backend,
    ))
    .bind(&id)
    .execute(&app.state.db)
    .await
    .expect("clear the job's queue rows");
    let jobs_before = app
        .get_json("/admin/backfill/status")
        .await
        .as_array()
        .unwrap()
        .len();
    let (status, body) = app
        .post_json_status(&format!("/admin/backfill/{id}/retry-failed"), json!({}))
        .await;
    assert_eq!(status, 400, "{body}");
    assert!(
        body.to_string()
            .contains("the job's DID list is no longer available"),
        "{body}"
    );
    let jobs_after = app
        .get_json("/admin/backfill/status")
        .await
        .as_array()
        .unwrap()
        .len();
    assert_eq!(jobs_after, jobs_before, "no network job in its place");
}

#[tokio::test]
#[serial]
async fn a_retried_block_applies_markers_only_for_dids_with_records() {
    let mock = MockJetstream::start().await;
    let Some(app) = app(&mock, None).await else {
        return;
    };
    let (held, stranger) = ("did:plc:heldretry", "did:plc:strangerretry");
    let mut events = games(1..=4, held);
    events.extend([
        js::archive_create(
            5,
            "did:plc:noise",
            "app.other.thing",
            "o5",
            json!({ "$type": "app.other.thing" }),
        ),
        js::archive_account(6, held, false, Some("takendown")),
        js::archive_account(7, stranger, false, Some("deleted")),
        js::archive_create(
            8,
            "did:plc:noise",
            "app.other.thing",
            "o8",
            json!({ "$type": "app.other.thing" }),
        ),
    ]);
    let segment = mock.seal(events, 4);
    // Block 1 (seqs 5-8) holds both markers; fail it in the marker sweep.
    mock.fail_block(&segment, 1, usize::MAX);
    let id = start_job(&app, json!({ "collection": GAME, "source": "replay" })).await;
    assert_eq!(finished(&app, &id).await["status"], "completed");
    assert_eq!(
        dump(&app).await.len(),
        4,
        "the takedown was in the failed block"
    );
    mock.fail_block(&segment, 1, 0);
    let (_, body) = app
        .post_json_status(&format!("/admin/backfill/{id}/retry-failed"), json!({}))
        .await;
    assert_eq!(
        finished(&app, body["id"].as_str().unwrap()).await["status"],
        "completed"
    );
    assert!(dump(&app).await.is_empty(), "the takedown applied on retry");
    let tombstones: Vec<(String,)> = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT did FROM happyview_did_tombstones WHERE did = ?",
        app.state.db_backend,
    ))
    .bind(stranger)
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    assert!(
        tombstones.is_empty(),
        "no tombstone for a DID this instance holds nothing for"
    );
}

#[tokio::test]
#[serial]
async fn a_plan_that_stops_advancing_fails_the_job() {
    let mock = MockJetstream::start().await;
    let Some(app) = app(&mock, None).await else {
        return;
    };
    mock.seal(games(1..=4, "did:plc:stall"), 4);
    mock.stall_plan(5);
    let id = start_job(&app, json!({ "collection": GAME, "source": "replay" })).await;
    let job = finished(&app, &id).await;
    assert_eq!(job["status"], "failed");
    assert_eq!(job["error"], "the archive plan stopped advancing at seq 0");
}

#[tokio::test]
#[serial]
async fn a_throttled_archive_is_waited_out() {
    let mock = MockJetstream::start().await;
    let Some(app) = app(&mock, None).await else {
        return;
    };
    mock.seal(games(1..=4, "did:plc:throttle"), 4);
    mock.throttle_archive(3);
    let id = start_job(&app, json!({ "collection": GAME, "source": "replay" })).await;
    assert_eq!(finished(&app, &id).await["status"], "completed");
    assert_eq!(dump(&app).await.len(), 4);
}

#[tokio::test]
#[serial]
async fn cancelling_a_replay_stops_it() {
    let mock = MockJetstream::start().await;
    let Some(app) = app(&mock, None).await else {
        return;
    };
    let segment = mock.seal(games(1..=4, "did:plc:cancel"), 4);
    mock.hold_segment(&segment);
    let id = start_job(&app, json!({ "collection": GAME, "source": "replay" })).await;
    wait_until!(
        "the download to start",
        mock.segment_downloads().contains(&segment)
    );
    let (status, _) = app
        .post_json_status(&format!("/admin/backfill/{id}/cancel"), json!({}))
        .await;
    assert_eq!(status, 200);
    mock.release_segment(&segment);
    assert_eq!(finished(&app, &id).await["status"], "cancelled");
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

/// Seed a setting. Every seeded row shares one `updated_at`, as `cursor::save`
/// writes the seq and the time together (copied from tests/jetstream_live.rs).
async fn save_setting(app: &TestApp, key: &str, value: &str) {
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_instance_settings (key, value, updated_at) VALUES (?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(key)
        .bind(value)
        .bind("2026-01-01T00:00:00+00:00")
        .execute(&app.state.db)
        .await
        .expect("seed setting");
}

/// Run the live consumer on GAME against `mock`; abort it by dropping the
/// handle's task (`.abort()`), keeping the sender alive until then.
fn consume(
    app: &TestApp,
    mock: &MockJetstream,
) -> (
    tokio::task::JoinHandle<()>,
    tokio::sync::watch::Sender<Vec<String>>,
) {
    let (tx, rx) = tokio::sync::watch::channel(vec![GAME.to_string()]);
    let handle = happyview::jetstream::spawn_with(
        app.state.clone(),
        rx,
        settings(mock),
        Some(fast_retries()),
    );
    (handle, tx)
}

/// `app` with `BACKFILL_SOURCE=auto`, the production default. The TestApp
/// default crawls, and under `crawl` gap replay is off (P8).
async fn auto_app(mock: &MockJetstream) -> Option<TestApp> {
    let mut app = app(mock, None).await?;
    app.state.archive.default_source = SourceChoice::Auto;
    app.rebuild_router();
    Some(app)
}

/// The details of every `jetstream.gap` event, oldest first.
async fn gap_events(app: &TestApp) -> Vec<serde_json::Value> {
    happyview::db::query_as::<(String,)>(&happyview::db::adapt_sql(
        "SELECT detail FROM happyview_event_logs WHERE event_type = 'jetstream.gap' ORDER BY created_at",
        app.state.db_backend,
    ))
    .fetch_all(&app.state.db)
    .await
    .expect("gap events")
    .into_iter()
    .map(|(detail,)| serde_json::from_str(&detail).expect("detail is JSON"))
    .collect()
}

fn replay_jobs(jobs: &serde_json::Value) -> Vec<serde_json::Value> {
    jobs.as_array()
        .unwrap()
        .iter()
        .filter(|j| j["source"] == "replay")
        .cloned()
        .collect()
}

#[tokio::test]
#[serial]
async fn cursor_too_old_replays_the_gap_and_resumes_live_at_the_sealed_tip() {
    let mock = MockJetstream::start().await;
    let Some(app) = auto_app(&mock).await else {
        return;
    };
    let did = "did:plc:gap";
    mock.seal(games(1..=50, did), 10);
    // Live events after the sealed tip.
    mock.push(
        (51..=55).map(|s| js::create(s, did, GAME, &format!("g{s:04}"), game(&format!("t{s}")))),
    );
    // The consumer last applied seq 20 on this host, long enough ago that the
    // server refuses it.
    save_setting(&app, "jetstream_seq", "20").await;
    save_setting(&app, "jetstream_host", &mock.host()).await;
    mock.reject_next(
        400,
        "CursorTooOld",
        "cursor is older than the retention floor",
    );

    let (consumer, _collections) = consume(&app, &mock);

    wait_until!(
        "seq 51-55 applied live",
        dump(&app)
            .await
            .iter()
            .filter(|r| r.0.ends_with("g0055"))
            .count()
            == 1
    );
    let jobs = app.get_json("/admin/backfill/status").await;
    let gap = jobs
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["source"] == "replay")
        .expect("a gap replay job")
        .clone();
    let gap = finished(&app, gap["id"].as_str().unwrap()).await;
    assert_eq!(gap["status"], "completed", "{gap}");
    assert_eq!(gap["replay"]["after_seq"], 20);
    consumer.abort();

    let uris: Vec<String> = dump(&app).await.into_iter().map(|r| r.0).collect();
    assert_eq!(
        uris.len(),
        35,
        "21-50 replayed, 51-55 live, 1-20 not refetched: {uris:?}"
    );
    let connections = mock.connections();
    let resumed = connections.last().unwrap();
    assert_eq!(
        resumed.param("cursor").as_deref(),
        Some("50"),
        "resumed at the sealed tip"
    );
    let gaps = gap_events(&app).await;
    let detail = &gaps[0];
    assert_eq!(detail["reason"], "cursor_too_old");
    assert_eq!(detail["after_seq"], 20);
    assert_eq!(detail["resumed_at_seq"], 50);
    assert_eq!(detail["replay_job_id"], gap["id"]);
}

#[tokio::test]
#[serial]
async fn cursor_too_old_without_an_archive_resumes_as_before() {
    let mock = MockJetstream::start().await;
    mock.disable_archive(404);
    let Some(app) = auto_app(&mock).await else {
        return;
    };
    mock.push(
        (51..=55).map(|s| js::create(s, "did:plc:noarchive", GAME, &format!("g{s:04}"), game("t"))),
    );
    save_setting(&app, "jetstream_seq", "20").await;
    save_setting(&app, "jetstream_host", &mock.host()).await;
    mock.reject_next(400, "CursorTooOld", "too old");
    let (consumer, _collections) = consume(&app, &mock);
    wait_until!("live events applied", dump(&app).await.len() == 5);
    consumer.abort();
    let jobs = app.get_json("/admin/backfill/status").await;
    assert!(
        jobs.as_array().unwrap().is_empty(),
        "no replay job without an archive"
    );
}

#[tokio::test]
#[serial]
async fn cursor_too_old_under_crawl_never_asks_the_archive() {
    // P8: with BACKFILL_SOURCE=crawl (the TestApp default), CursorTooOld is
    // handled exactly as before, even though this host has an archive.
    let mock = MockJetstream::start().await;
    let Some(app) = app(&mock, None).await else {
        return;
    };
    mock.seal(games(1..=50, "did:plc:crawlgap"), 10);
    mock.push(
        (51..=55).map(|s| js::create(s, "did:plc:crawlgap", GAME, &format!("g{s:04}"), game("t"))),
    );
    save_setting(&app, "jetstream_seq", "20").await;
    save_setting(&app, "jetstream_host", &mock.host()).await;
    mock.reject_next(400, "CursorTooOld", "too old");
    let (consumer, _collections) = consume(&app, &mock);
    wait_until!("live events applied", dump(&app).await.len() == 5);
    consumer.abort();
    assert!(
        mock.plan_requests().is_empty(),
        "the archive was never asked"
    );
    let jobs = app.get_json("/admin/backfill/status").await;
    assert!(
        jobs.as_array().unwrap().is_empty(),
        "no replay job under crawl"
    );
}

#[tokio::test]
#[serial]
async fn a_repeated_cursor_too_old_reuses_the_running_gap_job() {
    let mock = MockJetstream::start().await;
    let Some(app) = auto_app(&mock).await else {
        return;
    };
    let did = "did:plc:gaptwice";
    // 25 events a block: (20, 50] touches both blocks, so the plan answers
    // in segment mode and `hold_segment` can keep the job running.
    let segment = mock.seal(games(1..=50, did), 25);
    mock.hold_segment(&segment);
    mock.push((51..=55).map(|s| js::create(s, did, GAME, &format!("g{s:04}"), game("t"))));
    save_setting(&app, "jetstream_seq", "20").await;
    save_setting(&app, "jetstream_host", &mock.host()).await;
    // Refused twice: at the saved seq 20, then again at the sealed tip 50,
    // before anything is applied, so the saved seq is still 20.
    mock.reject_next(400, "CursorTooOld", "too old");
    mock.reject_next(400, "CursorTooOld", "still too old");
    let (consumer, _collections) = consume(&app, &mock);
    wait_until!(
        "the live stream to resume",
        dump(&app).await.iter().any(|r| r.0.ends_with("g0055"))
    );
    let jobs = replay_jobs(&app.get_json("/admin/backfill/status").await);
    assert_eq!(
        jobs.len(),
        1,
        "one gap job for seq 20, however often it is refused: {jobs:?}"
    );
    mock.release_segment(&segment);
    assert_eq!(
        finished(&app, jobs[0]["id"].as_str().unwrap()).await["status"],
        "completed"
    );
    consumer.abort();
    let gaps = gap_events(&app).await;
    assert_eq!(gaps.len(), 2, "{gaps:?}");
    assert!(
        gaps.iter().all(|g| g["replay_job_id"] == jobs[0]["id"]),
        "{gaps:?}"
    );
    assert_eq!(dump(&app).await.len(), 35);
}
