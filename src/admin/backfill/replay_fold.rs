//! Fold archive events into the record store in seq order (spec B.6),
//! through the ingest pipeline. Creates and updates are buffered and handed
//! to `ingest::apply_batch` (CID check, hook, one `write_network`
//! transaction with the rev guard and tombstones); deletes go through
//! `ingest::apply`; account and sync markers apply as of their own time (P2).
//! A delete of a buffered record, or a marker for a buffered DID, flushes
//! first, so per-record and per-DID order holds.

use std::collections::HashSet;

use crate::AppState;
use crate::ingest::{Action, IngestOutcome, Origin, RecordEvent, RecordKey, SourceEvent};
use crate::jetstream::event::{Event, EventKind};

pub(super) const FOLD_BATCH: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FoldError(pub String);

impl std::fmt::Display for FoldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub(super) struct ReplayFold {
    state: AppState,
    job_id: String,
    tracked: Vec<String>,
    pending: Vec<SourceEvent>,
    pending_uris: HashSet<String>,
    pending_dids: HashSet<String>,
    written: u64,
    /// DIDs with a record written or changed since `take_written_dids`,
    /// kept only once `collect_written_dids` is called: a retry job needs
    /// them, and an ordinary job would hold every DID it wrote.
    written_dids: HashSet<String>,
    collect_dids: bool,
}

impl ReplayFold {
    /// `tracked`: the job's collections, which scope a `#sync` purge.
    pub(super) fn new(state: AppState, job_id: &str, tracked: Vec<String>) -> Self {
        Self {
            state,
            job_id: job_id.to_string(),
            tracked,
            pending: Vec::with_capacity(FOLD_BATCH),
            pending_uris: HashSet::new(),
            pending_dids: HashSet::new(),
            written: 0,
            written_dids: HashSet::new(),
            collect_dids: false,
        }
    }

    /// Records written or changed so far.
    pub(super) fn written(&self) -> u64 {
        self.written
    }

    /// Keep the DIDs of flushed writes for `take_written_dids` (retry runs).
    pub(super) fn collect_written_dids(&mut self) {
        self.collect_dids = true;
    }

    /// The DIDs whose records were written or changed since the last call,
    /// flushed writes only.
    pub(super) fn take_written_dids(&mut self) -> HashSet<String> {
        std::mem::take(&mut self.written_dids)
    }

    /// Whether a record of `did` waits in the buffer, not yet written.
    pub(super) fn buffers(&self, did: &str) -> bool {
        self.pending_dids.contains(did)
    }

    fn origin(&self) -> Origin {
        Origin::Backfill {
            job_id: self.job_id.clone(),
        }
    }

    pub(super) async fn apply(&mut self, event: Event) -> Result<(), FoldError> {
        let Event { did, cursor, kind } = event;
        match kind {
            EventKind::Commit(commit) if commit.operation == "delete" => {
                let uri = format!("at://{did}/{}/{}", commit.collection, commit.rkey);
                if self.pending_uris.contains(&uri) {
                    self.flush().await?;
                }
                let event = SourceEvent::Record(RecordEvent {
                    key: RecordKey {
                        did,
                        collection: commit.collection,
                        rkey: commit.rkey,
                    },
                    action: Action::Delete,
                    cid: None,
                    rev: commit.rev,
                    body: None,
                    time_us: Some(cursor.time_us()),
                    origin: self.origin(),
                    stored: false,
                });
                match crate::ingest::apply(&self.state, event).await {
                    IngestOutcome::Errored | IngestOutcome::Unavailable => Err(FoldError(format!(
                        "deleting {uri} failed; see its record event log"
                    ))),
                    _ => Ok(()),
                }
            }
            EventKind::Commit(commit) => {
                let Some(record) = commit.record else {
                    return Ok(());
                };
                let action = match commit.operation.as_str() {
                    "update" => Action::Update,
                    _ => Action::Create,
                };
                let key = RecordKey {
                    did: did.clone(),
                    collection: commit.collection,
                    rkey: commit.rkey,
                };
                self.pending_uris.insert(key.uri());
                self.pending_dids.insert(did);
                self.pending.push(SourceEvent::Record(RecordEvent {
                    key,
                    action,
                    // Spec B.6: the pipeline verifies each record's CID, as
                    // the crawl does. The CID is the payload's own, so a
                    // mismatch means the JSON form of the payload does not
                    // re-encode to it; the record is skipped.
                    cid: commit.cid,
                    rev: commit.rev,
                    body: Some(record),
                    time_us: Some(cursor.time_us()),
                    origin: self.origin(),
                    stored: false,
                }));
                if self.pending.len() >= FOLD_BATCH {
                    self.flush().await?;
                }
                Ok(())
            }
            kind @ (EventKind::Account { .. } | EventKind::Sync { .. }) => {
                if self.pending_dids.contains(&did) {
                    self.flush().await?;
                }
                let as_of_us = Some(cursor.time_us());
                let event = crate::jetstream::apply::marker_event(
                    did,
                    cursor,
                    kind,
                    as_of_us,
                    &self.tracked,
                );
                let applied = crate::ingest::apply_in(&self.state, event).await;
                match applied.outcome {
                    IngestOutcome::Unavailable
                    | IngestOutcome::Errored
                    | IngestOutcome::Invalid => Err(FoldError(applied.error.unwrap_or_default())),
                    _ => Ok(()),
                }
            }
            EventKind::Identity { .. } => Ok(()),
        }
    }

    pub(super) async fn flush(&mut self) -> Result<(), FoldError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let batch = std::mem::take(&mut self.pending);
        self.pending_uris.clear();
        self.pending_dids.clear();
        let count = batch.len();
        let dids: Vec<String> = batch
            .iter()
            .map(|event| match event {
                SourceEvent::Record(ev) => ev.key.did.clone(),
                _ => String::new(),
            })
            .collect();
        let (outcome, error) = crate::ingest::apply_batch_in(&self.state, batch).await;
        if let Some(e) = error {
            return Err(FoldError(format!(
                "writing {count} replayed records failed: {e}"
            )));
        }
        if outcome
            .outcomes
            .iter()
            .any(|o| matches!(o, IngestOutcome::Errored | IngestOutcome::Unavailable))
        {
            return Err(FoldError(format!(
                "writing {count} replayed records failed; see the record event log"
            )));
        }
        for (changed, did) in outcome.changed.iter().zip(dids) {
            if *changed {
                self.written += 1;
                if self.collect_dids {
                    self.written_dids.insert(did);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::adapt_sql;
    use crate::jetstream::event::{Commit, CursorPoint, Event, EventKind};
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
        ReplayFold::new(state.clone(), "job", vec![NSID.to_string()])
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
                ordinary.written_dids.is_empty(),
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
}
