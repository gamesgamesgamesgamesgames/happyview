//! Fold archive events into the record store in seq order (spec B.6),
//! through the ingest pipeline. Creates and updates are buffered and handed
//! to `Host::ingest_batch` (CID check, hook, one write transaction with the
//! rev guard and tombstones); deletes go through `Host::ingest`; account and
//! sync markers apply as of their own time (P2).
//! A delete of a buffered record, or a marker for a buffered DID, flushes
//! first, so per-record and per-DID order holds.

use std::collections::HashSet;
use std::sync::Arc;

use happyview_provider::{
    Action, Host, IngestOutcome, Origin, RecordEvent, RecordKey, SourceEvent,
};

use crate::consumer::event::{Event, EventKind};

pub const FOLD_BATCH: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoldError(pub String);

impl std::fmt::Display for FoldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub struct ReplayFold {
    host: Arc<dyn Host>,
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
    pub fn new(host: Arc<dyn Host>, job_id: &str, tracked: Vec<String>) -> Self {
        Self {
            host,
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

    /// The DIDs whose records were written or changed since the last
    /// [`Self::take_written_dids`] (retry runs only).
    #[doc(hidden)]
    pub fn written_dids(&self) -> &HashSet<String> {
        &self.written_dids
    }

    /// Records written or changed so far.
    pub fn written(&self) -> u64 {
        self.written
    }

    /// Keep the DIDs of flushed writes for `take_written_dids` (retry runs).
    pub fn collect_written_dids(&mut self) {
        self.collect_dids = true;
    }

    /// The DIDs whose records were written or changed since the last call,
    /// flushed writes only.
    pub fn take_written_dids(&mut self) -> HashSet<String> {
        std::mem::take(&mut self.written_dids)
    }

    /// Whether a record of `did` waits in the buffer, not yet written.
    pub fn buffers(&self, did: &str) -> bool {
        self.pending_dids.contains(did)
    }

    fn origin(&self) -> Origin {
        Origin::Backfill {
            job_id: self.job_id.clone(),
        }
    }

    pub async fn apply(&mut self, event: Event) -> Result<(), FoldError> {
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
                match self.host.ingest(event).await {
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
                let event = crate::consumer::apply::marker_event(
                    did,
                    cursor,
                    kind,
                    as_of_us,
                    &self.tracked,
                );
                let (outcome, error) = self.host.ingest_with_error(event).await;
                match outcome {
                    IngestOutcome::Unavailable
                    | IngestOutcome::Errored
                    | IngestOutcome::Invalid => Err(FoldError(error.unwrap_or_default())),
                    _ => Ok(()),
                }
            }
            EventKind::Identity { .. } => Ok(()),
        }
    }

    pub async fn flush(&mut self) -> Result<(), FoldError> {
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
        let (outcome, error) = self.host.ingest_batch_with_error(batch).await;
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
