//! Replay backfill against a mock Jetstream archive. Needs TEST_DATABASE_URL
//! for the database tests; the writer and mock tests run without it.

mod common;

use common::jetstream::{self as js, MockJetstream};
use common::segment_writer::write_segment;
use happyview::jetstream::archive::client::{ArchiveClient, PlanMode, PlanRequest};
use happyview::jetstream::archive::{ArchiveConfig, rows, segment};
use serde_json::json;
use std::io::Cursor;

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
