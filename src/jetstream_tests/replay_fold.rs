//! The archive replay fold against HappyView's ingest pipeline, through
//! `AppHost` (SQLite, and Postgres when `TEST_DATABASE_URL` is set).
use std::collections::HashSet;

use happyview_source_jetstream::backfill::replay_fold::ReplayFold;
use happyview_source_jetstream::consumer::event::{Commit, CursorPoint, Event, EventKind};

use crate::AppState;
use crate::db::adapt_sql;
use crate::host::AppHost;
use crate::lexicon::{ParsedLexicon, ProcedureAction};

const NSID: &str = "app.test.replayed";
const BASE_US: i64 = 1_779_719_000_000_000;

async fn states() -> Vec<AppState> {
    let mut states = vec![crate::test_support::test_state_with_pool(
        crate::test_support::migrated_memory_pool().await,
    )];
    if let Some(state) = crate::test_support::test_state_from_env().await {
        states.push(state);
    }
    for state in &states {
        let parsed = ParsedLexicon::parse(
            serde_json::json!({ "lexicon": 1, "id": NSID, "defs": { "main": { "type": "record", "key": "tid" } } }),
            1,
            Some(NSID.to_string()),
            ProcedureAction::Upsert,
            None,
        )
        .expect("lexicon");
        state.lexicons.upsert(parsed).await;
    }
    states
}

fn did() -> String {
    format!("did:plc:fold{}", uuid::Uuid::new_v4().simple())
}

fn rev(seq: i64) -> String {
    crate::tid::tid_from_unix_microseconds(BASE_US + seq)
}

fn at(seq: i64) -> CursorPoint {
    CursorPoint::V2 {
        seq,
        witnessed_us: BASE_US + seq,
    }
}

fn commit(did: &str, seq: i64, rkey: &str, operation: &str, text: &str) -> Event {
    let record =
        (operation != "delete").then(|| serde_json::json!({ "$type": NSID, "text": text }));
    let cid = record.as_ref().map(|r| {
        crate::cid_verify::compute_record_cid(r)
            .unwrap()
            .to_string()
    });
    Event {
        did: did.into(),
        cursor: at(seq),
        kind: EventKind::Commit(Commit {
            rev: Some(rev(seq)),
            operation: operation.into(),
            collection: NSID.into(),
            rkey: rkey.into(),
            record,
            cid,
        }),
    }
}

fn fresh(state: &AppState) -> ReplayFold {
    ReplayFold::new(AppHost::new(state.clone()), "job", vec![NSID.to_string()])
}

async fn text(state: &AppState, did: &str, rkey: &str) -> Option<String> {
    state
        .records
        .get(&format!("at://{did}/{NSID}/{rkey}"))
        .await
        .expect("get")
        .map(|r| {
            serde_json::from_str::<serde_json::Value>(&r.record).unwrap()["text"]
                .as_str()
                .unwrap()
                .to_string()
        })
}

#[tokio::test]
async fn creates_are_batched_and_keep_their_rev() {
    for state in states().await {
        let did = did();
        let mut fold = fresh(&state);
        for i in 0..450 {
            fold.apply(commit(&did, i + 1, &format!("r{i:04}"), "create", "x"))
                .await
                .unwrap();
        }
        fold.flush().await.unwrap();
        assert_eq!(fold.written(), 450);
        let stored: (Option<String>,) = crate::db::query_as(&adapt_sql(
            "SELECT rev FROM happyview_records WHERE did = ? AND collection = ? AND rkey = 'r0007'",
            state.db_backend,
        ))
        .bind(&did)
        .bind(NSID)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(stored.0, Some(rev(8)));
    }
}

#[tokio::test]
async fn only_a_retry_fold_collects_the_dids_it_wrote() {
    for state in states().await {
        let did = did();
        let mut ordinary = fresh(&state);
        ordinary
            .apply(commit(&did, 1, "r1", "create", "x"))
            .await
            .unwrap();
        ordinary.flush().await.unwrap();
        assert_eq!(ordinary.written(), 1);
        assert!(
            ordinary.written_dids().is_empty(),
            "an ordinary job holds no DIDs"
        );

        let mut retry = fresh(&state);
        retry.collect_written_dids();
        retry
            .apply(commit(&did, 2, "r2", "create", "y"))
            .await
            .unwrap();
        retry.flush().await.unwrap();
        assert_eq!(retry.take_written_dids(), HashSet::from([did.clone()]));
        assert!(retry.take_written_dids().is_empty());
    }
}

#[tokio::test]
async fn a_create_followed_by_its_delete_stays_deleted() {
    for state in states().await {
        let did = did();
        let mut fold = fresh(&state);
        fold.apply(commit(&did, 1, "gone", "create", "x"))
            .await
            .unwrap();
        fold.apply(commit(&did, 2, "gone", "delete", ""))
            .await
            .unwrap();
        fold.flush().await.unwrap();
        assert_eq!(text(&state, &did, "gone").await, None);
        // A resumed job replays the create again: the tombstone holds.
        let mut again = fresh(&state);
        again
            .apply(commit(&did, 1, "gone", "create", "x"))
            .await
            .unwrap();
        again.flush().await.unwrap();
        assert_eq!(text(&state, &did, "gone").await, None);
    }
}

#[tokio::test]
async fn an_update_in_the_same_batch_wins() {
    for state in states().await {
        let did = did();
        let mut fold = fresh(&state);
        fold.apply(commit(&did, 1, "r", "create", "first"))
            .await
            .unwrap();
        fold.apply(commit(&did, 2, "r", "update", "second"))
            .await
            .unwrap();
        fold.flush().await.unwrap();
        assert_eq!(text(&state, &did, "r").await.as_deref(), Some("second"));
    }
}

#[tokio::test]
async fn an_older_replayed_update_never_overwrites_a_newer_live_one() {
    for state in states().await {
        let did = did();
        let mut live = fresh(&state);
        live.apply(commit(&did, 30, "r", "create", "live"))
            .await
            .unwrap();
        live.flush().await.unwrap();
        let mut fold = fresh(&state);
        fold.apply(commit(&did, 20, "r", "update", "replayed"))
            .await
            .unwrap();
        fold.flush().await.unwrap();
        assert_eq!(text(&state, &did, "r").await.as_deref(), Some("live"));
    }
}

#[tokio::test]
async fn a_marker_flushes_its_did_first() {
    // A sync leaves no tombstone, so only flushing before it keeps the
    // buffered older create from landing after the purge.
    for state in states().await {
        let did = did();
        let mut fold = fresh(&state);
        fold.apply(commit(&did, 10, "old", "create", "x"))
            .await
            .unwrap();
        fold.apply(Event {
            did: did.clone(),
            cursor: at(20),
            kind: EventKind::Sync { rev: Some(rev(20)) },
        })
        .await
        .unwrap();
        fold.apply(commit(&did, 30, "new", "create", "y"))
            .await
            .unwrap();
        fold.flush().await.unwrap();
        assert_eq!(text(&state, &did, "old").await, None);
        assert_eq!(text(&state, &did, "new").await.as_deref(), Some("y"));
    }
}

#[tokio::test]
async fn a_record_whose_cid_does_not_match_is_skipped() {
    // Spec B.6: CID verification runs per record, as in the crawl. A
    // payload whose JSON form does not re-encode to its CID is skipped.
    for state in states().await {
        let did = did();
        let mut fold = fresh(&state);
        let mut event = commit(&did, 1, "r", "create", "x");
        let EventKind::Commit(commit) = &mut event.kind else {
            unreachable!()
        };
        commit.cid = Some(
            crate::cid_verify::compute_record_cid(&serde_json::json!({ "other": true }))
                .unwrap()
                .to_string(),
        );
        fold.apply(event).await.unwrap();
        fold.flush().await.unwrap();
        assert_eq!(fold.written(), 0);
        assert_eq!(text(&state, &did, "r").await, None);
    }
}

#[tokio::test]
async fn identity_rows_are_ignored() {
    for state in states().await {
        let mut fold = fresh(&state);
        fold.apply(Event {
            did: did(),
            cursor: at(1),
            kind: EventKind::Identity { handle: None },
        })
        .await
        .unwrap();
        fold.flush().await.unwrap();
        assert_eq!(fold.written(), 0);
    }
}
