//! A backfill job that replays the Jetstream archive (spec B; plan rulings
//! P1-P4). Commit sweeps first, then account/sync marker sweeps; each plan
//! entry is downloaded, decoded, exactly filtered and folded in seq order, and
//! its sweep's cursor saved once the entry's writes commit.

use std::collections::HashSet;
use std::path::PathBuf;

use futures_util::StreamExt;

use super::replay_fold::ReplayFold;
use super::replay_sweeps::{self as sweeps, Sweep, SweepKind};
use crate::AppState;
use crate::admin::backfill_errors::{BackfillErrorKind, BackfillFailure, ErrorRecorder};
use crate::jetstream::archive::client::{
    ArchiveClient, ArchiveError, PlanEntry, PlanMode, PlanRequest,
};
use crate::jetstream::archive::rows::{self, RowFilter, RowKinds};
use crate::jetstream::archive::segment::{self, Row};

const STALLED_PAGES: u32 = 5;
/// Failed blocks in a row, across entries, after which the job stops rather
/// than run on through an archive outage. Throttling alone doesn't get here:
/// the client paces to the archive's byte quota and waits out its 429s.
const MAX_CONSECUTIVE_FAILURES: u32 = 16;

pub(super) enum ReplayOutcome {
    Completed {
        records: u64,
        bytes: u64,
    },
    /// `auto`, and the Jetstream has no archive: the job is now a crawl.
    FellBack,
    /// Pausing or cancelling; the caller finalises.
    Stopped,
    Failed(String),
}

enum Halt {
    Stopped,
    NoArchive(ArchiveError),
    Failed(String),
}

struct Run<'a> {
    state: &'a AppState,
    job_id: &'a str,
    client: ArchiveClient,
    fold: ReplayFold,
    errors: ErrorRecorder,
    /// Bytes already added to the job's counter.
    counted: u64,
    /// Records of this run's fold already added to `total_records`.
    records_counted: u64,
    probing: bool,
    download_dir: PathBuf,
    /// In a network retry job, the collections a marker's DID must hold
    /// network records in for the marker to apply. `None` otherwise.
    held_only: Option<Vec<String>>,
    /// The current entry's failed blocks (segment, block, message), recorded
    /// once the entry is folded and before its cursor is saved. An entry the
    /// job stops inside is redone on resume, so its failures are not.
    entry_failures: Vec<(String, Option<u32>, String)>,
    /// Blocks that failed since the last one that didn't.
    consecutive_failures: u32,
}

pub(super) async fn run(
    state: &AppState,
    job_id: &str,
    collections: &[String],
    scope_dids: Option<Vec<String>>,
    source: &str,
) -> ReplayOutcome {
    let endpoint = crate::jetstream::config::parse_endpoint(&state.config.jetstream_url);
    let mut run = Run {
        state,
        job_id,
        client: ArchiveClient::new(&endpoint, &state.archive),
        fold: ReplayFold::new(state.clone(), collections.to_vec()),
        errors: ErrorRecorder::new(state, job_id).await,
        counted: 0,
        records_counted: 0,
        probing: source == "auto",
        download_dir: state.archive.download_dir.clone(),
        held_only: None,
        entry_failures: Vec::new(),
        consecutive_failures: 0,
    };
    sweep_downloads(&run.download_dir, job_id).await;
    let result = run.drive(collections, scope_dids).await;
    let flushed = run.fold.flush().await;
    let counted = run.count_bytes(0).await;
    // Persist the error counts (`error_counts`) before the caller completes,
    // fails or pauses the job; `record` only writes the detail rows.
    run.errors.flush(state, job_id).await;
    remove_own_downloads(&run.download_dir, job_id).await;
    match (result, flushed) {
        (Ok(()), Ok(())) => {
            // `total_records` and `replay_bytes` span every run of the job;
            // this run only knows its own.
            let totals = match counted {
                Ok(()) => match (
                    sweeps::records(state, job_id).await,
                    sweeps::counters(state, job_id).await,
                ) {
                    (Ok(records), Ok((bytes, _))) => Ok((records, bytes)),
                    (Err(e), _) | (_, Err(e)) => Err(format!("replay bookkeeping failed: {e}")),
                },
                Err(Halt::Failed(e)) => Err(e),
                Err(_) => Err("replay bookkeeping failed: the record count was not saved".into()),
            };
            match totals {
                Ok((records, bytes)) => ReplayOutcome::Completed {
                    records: u64::try_from(records).unwrap_or(0),
                    bytes: u64::try_from(bytes).unwrap_or(0),
                },
                Err(e) => ReplayOutcome::Failed(e),
            }
        }
        (Err(Halt::Stopped), _) => ReplayOutcome::Stopped,
        (Err(Halt::NoArchive(e)), _) if run.probing => {
            if matches!(e, ArchiveError::Unauthorized(_)) {
                // Bluesky's hosted archive wants a key; say so once, not on
                // every job.
                static KEY_HINT: std::sync::Once = std::sync::Once::new();
                KEY_HINT.call_once(|| {
                    tracing::warn!(
                        host = run.client.host(),
                        error = %e,
                        "the jetstream archive needs a key; set JETSTREAM_API_KEY to replay. Backfilling by crawl"
                    );
                });
            }
            tracing::info!(job_id, error = %e, "no jetstream archive; backfilling by crawl");
            match (
                sweeps::clear(state, job_id).await,
                sweeps::set_source(state, job_id, "crawl").await,
            ) {
                (Ok(()), Ok(())) => ReplayOutcome::FellBack,
                (Err(e), _) | (_, Err(e)) => {
                    ReplayOutcome::Failed(format!("failed to switch the job to crawl: {e}"))
                }
            }
        }
        (Err(Halt::NoArchive(ArchiveError::Unauthorized(status))), _) => {
            ReplayOutcome::Failed(format!(
                "the jetstream at {} needs a key to replay its archive (HTTP {status}); set JETSTREAM_API_KEY",
                run.client.host()
            ))
        }
        (Err(Halt::NoArchive(ArchiveError::NoArchive(status))), _) => {
            ReplayOutcome::Failed(format!(
                "the jetstream at {} has no replay archive (HTTP {status})",
                run.client.host()
            ))
        }
        (Err(Halt::NoArchive(e)), _) => ReplayOutcome::Failed(format!(
            "the jetstream at {} is unreachable: {e}",
            run.client.host()
        )),
        (Err(Halt::Failed(e)), _) | (Ok(()), Err(super::replay_fold::FoldError(e))) => {
            ReplayOutcome::Failed(e)
        }
    }
}

/// Spool files older than this are removed when any replay job starts (P4),
/// whichever job left them.
const STALE_DOWNLOAD_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// At job start (P4): remove `<dir>/<job id>-*`, left by this job's run that
/// died mid-download, and every file in `dir` older than
/// `STALE_DOWNLOAD_AGE`, left by jobs that never came back.
async fn sweep_downloads(dir: &std::path::Path, job_id: &str) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    let prefix = format!("{job_id}-");
    while let Ok(Some(entry)) = entries.next_entry().await {
        let own = entry.file_name().to_string_lossy().starts_with(&prefix);
        let stale = entry
            .metadata()
            .await
            .ok()
            .filter(|meta| meta.is_file())
            .and_then(|meta| meta.modified().ok())
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > STALE_DOWNLOAD_AGE);
        if own || stale {
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
    }
}

/// When a run ends: remove `<dir>/<job id>-*`, anything an error path left.
async fn remove_own_downloads(dir: &std::path::Path, job_id: &str) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    let prefix = format!("{job_id}-");
    while let Ok(Some(entry)) = entries.next_entry().await {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
    }
}

fn db(e: sqlx::Error) -> Halt {
    Halt::Failed(format!("replay bookkeeping failed: {e}"))
}

impl Run<'_> {
    async fn drive(
        &mut self,
        collections: &[String],
        scope_dids: Option<Vec<String>>,
    ) -> Result<(), Halt> {
        if sweeps::has_retries(self.state, self.job_id)
            .await
            .map_err(db)?
        {
            super::set_stage(self.state, self.job_id, "retrying_blocks").await;
            return self.retry_blocks(collections, scope_dids).await;
        }
        let mut all = sweeps::load(self.state, self.job_id).await.map_err(db)?;
        if all.is_empty() {
            let after = sweeps::after_seq(self.state, self.job_id)
                .await
                .map_err(db)?;
            all = sweeps::initial_sweeps(collections, scope_dids.as_deref(), after);
            sweeps::insert(self.state, self.job_id, &all)
                .await
                .map_err(db)?;
        }
        super::set_stage(self.state, self.job_id, "replaying").await;
        for sweep in all
            .iter_mut()
            .filter(|s| s.kind == SweepKind::Commits && !s.done)
        {
            self.sweep(sweep).await?;
        }
        super::set_stage(self.state, self.job_id, "replaying_accounts").await;
        for sweep in all
            .iter_mut()
            .filter(|s| s.kind == SweepKind::Markers && !s.done)
        {
            self.sweep(sweep).await?;
        }
        if scope_dids.is_none() {
            // Network job: replay the markers of every DID left holding records.
            let after = all.first().map_or(0, |s| s.after_seq);
            let mut after_id = all.iter().filter_map(|s| s.last_did_id).max().unwrap_or(0);
            let mut next = all.len() as i32;
            loop {
                let held = sweeps::next_held_dids(
                    self.state,
                    collections,
                    after_id,
                    sweeps::MAX_PLAN_DIDS as i64,
                )
                .await
                .map_err(db)?;
                let Some(&(last, _)) = held.last() else { break };
                let mut sweep = Sweep {
                    sweep: next,
                    kind: SweepKind::Markers,
                    collections: Vec::new(),
                    dids: Some(held.into_iter().map(|(_, did)| did).collect()),
                    after_seq: after,
                    cursor_seq: after,
                    tip_seq: None,
                    last_did_id: Some(last),
                    done: false,
                };
                sweeps::insert(self.state, self.job_id, std::slice::from_ref(&sweep))
                    .await
                    .map_err(db)?;
                self.sweep(&mut sweep).await?;
                after_id = last;
                next += 1;
            }
        }
        Ok(())
    }

    async fn stop_requested(&self) -> bool {
        super::should_stop(self.state, self.job_id).await.is_some()
    }

    /// Add what the client downloaded and the fold wrote since the last call
    /// to the job's counters, plus `failed` blocks. Called once an entry's
    /// writes have committed.
    async fn count_bytes(&mut self, failed: i32) -> Result<(), Halt> {
        let written = self.fold.written();
        let new_records = written - self.records_counted;
        if new_records > 0 {
            sweeps::add_records(self.state, self.job_id, new_records as i64)
                .await
                .map_err(db)?;
            self.records_counted = written;
        }
        let total = self.client.bytes_downloaded();
        let delta = (total - self.counted) as i64;
        self.counted = total;
        if delta == 0 && failed == 0 {
            return Ok(());
        }
        sweeps::add_counters(self.state, self.job_id, delta, failed)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn sweep(&mut self, sweep: &mut Sweep) -> Result<(), Halt> {
        let (kinds, row_kinds) = match sweep.kind {
            SweepKind::Commits => (vec!["commit".to_string()], RowKinds::Commits),
            SweepKind::Markers => (
                vec!["account".to_string(), "sync".to_string()],
                RowKinds::Markers,
            ),
        };
        let collection_set: HashSet<String> = sweep.collections.iter().cloned().collect();
        let did_set: Option<HashSet<String>> =
            sweep.dids.as_ref().map(|d| d.iter().cloned().collect());
        let mut stalled = 0u32;
        loop {
            if self.stop_requested().await {
                return Err(Halt::Stopped);
            }
            let request = PlanRequest {
                kinds: kinds.clone(),
                collections: sweep.collections.clone(),
                dids: sweep.dids.clone().unwrap_or_default(),
                after_seq: sweep.cursor_seq as u64,
                before_seq: sweep.tip_seq.map(|t| t as u64),
            };
            let page = match self.client.plan(&request).await {
                Ok(page) => page,
                Err(e @ (ArchiveError::NoArchive(_) | ArchiveError::Unreachable(_)))
                    if sweep.tip_seq.is_none() =>
                {
                    return Err(Halt::NoArchive(e));
                }
                // A configured key the archive refuses is a mistake to fix,
                // not a reason to crawl.
                Err(ArchiveError::Unauthorized(status)) if self.state.archive.api_key.is_some() => {
                    return Err(Halt::Failed(format!(
                        "the jetstream at {} refused the replay key (HTTP {status})",
                        self.client.host()
                    )));
                }
                // Without a key, a first page that wants one is no archive
                // to this instance: `auto` crawls.
                Err(e @ ArchiveError::Unauthorized(_)) if sweep.tip_seq.is_none() => {
                    return Err(Halt::NoArchive(e));
                }
                Err(e) => return Err(Halt::Failed(format!("planSnapshot failed: {e}"))),
            };
            if self.probing && sweep.tip_seq.is_none() {
                sweeps::set_source(self.state, self.job_id, "replay")
                    .await
                    .map_err(db)?;
                self.probing = false;
            }
            let tip = *sweep.tip_seq.get_or_insert(page.sealed_tip_seq as i64);
            sweeps::save(self.state, self.job_id, sweep)
                .await
                .map_err(db)?;
            let page_start = sweep.cursor_seq;
            let filter = RowFilter {
                kinds: row_kinds,
                collections: collection_set.clone(),
                dids: did_set.clone(),
                after_seq: page_start as u64,
                through_seq: tip as u64,
            };
            for entry in &page.segments {
                if self.stop_requested().await {
                    return Err(Halt::Stopped);
                }
                let failed = self.entry(entry, &filter).await?;
                sweep.cursor_seq = sweep.cursor_seq.max((entry.max_seq as i64).min(tip));
                sweeps::save(self.state, self.job_id, sweep)
                    .await
                    .map_err(db)?;
                self.count_bytes(failed).await?;
                self.publish(sweep).await;
            }
            let through = page.planned_through_seq as i64;
            if through <= page_start && through < tip {
                stalled += 1;
                if stalled >= STALLED_PAGES {
                    return Err(Halt::Failed(format!(
                        "the archive plan stopped advancing at seq {through}"
                    )));
                }
            } else {
                stalled = 0;
            }
            sweep.cursor_seq = sweep.cursor_seq.max(through.min(tip));
            if through >= tip {
                sweep.done = true;
            }
            sweeps::save(self.state, self.job_id, sweep)
                .await
                .map_err(db)?;
            if sweep.done {
                return Ok(());
            }
        }
    }

    async fn publish(&self, sweep: &Sweep) {
        let (bytes, failed) = sweeps::counters(self.state, self.job_id)
            .await
            .unwrap_or((0, 0));
        super::publish_event(
            self.state,
            crate::admin::types::BackfillEvent::ReplayProgress {
                job_id: self.job_id.to_string(),
                through_seq: sweep.cursor_seq,
                tip_seq: sweep.tip_seq.unwrap_or(sweep.cursor_seq),
                bytes,
                blocks_failed: failed,
            },
        );
    }

    /// Download, decode and fold one plan entry; returns the blocks that
    /// failed (recorded as `replay_block` errors). The fold is flushed before
    /// returning, so the caller can save the cursor past the entry.
    async fn entry(&mut self, entry: &PlanEntry, filter: &RowFilter) -> Result<i32, Halt> {
        self.entry_failures.clear();
        match entry.mode {
            PlanMode::Segment => {
                self.whole_segment(&entry.name, filter).await?;
            }
            PlanMode::Blocks => {
                let indexes: Vec<u32> =
                    entry.blocks.iter().flat_map(|r| r.first..=r.last).collect();
                // The stream owns its own handle on the client (a clone shares
                // the byte counter), so the fold can borrow `self` mutably
                // while blocks are still downloading. `buffered` keeps at most
                // `concurrency` frames in flight and yields them in index
                // order, so each block is folded as it arrives and the
                // entry's frames are never all held at once.
                let client = self.client.clone();
                let name = entry.name.clone();
                let mut fetched = futures_util::stream::iter(indexes)
                    .map(move |i| {
                        let (client, name) = (client.clone(), name.clone());
                        async move { (i, client.block(&name, i).await) }
                    })
                    .buffered(self.state.archive.concurrency);
                while let Some((i, result)) = fetched.next().await {
                    match fetched_rows(result).await {
                        Ok(rows) => self.fold_rows(rows, filter, &entry.name).await?,
                        Err((e, outage)) => self.block_failed(&entry.name, Some(i), &e, outage)?,
                    }
                }
            }
        }
        self.fold.flush().await.map_err(|e| Halt::Failed(e.0))?;
        self.record_failures().await
    }

    /// Download and fold a whole segment. Its failures go to
    /// `entry_failures`.
    async fn whole_segment(&mut self, name: &str, filter: &RowFilter) -> Result<(), Halt> {
        if let Err(e) = tokio::fs::create_dir_all(&self.download_dir).await {
            return Err(Halt::Failed(format!(
                "cannot create {}: {e}",
                self.download_dir.display()
            )));
        }
        let path = self.download_dir.join(format!("{}-{name}", self.job_id));
        if let Err(e) = self.client.segment(name, &path).await {
            let _ = tokio::fs::remove_file(&path).await;
            return self.block_failed(name, None, &e.to_string(), is_outage(&e));
        }
        let opened = {
            let path = path.clone();
            tokio::task::spawn_blocking(move || -> Result<_, String> {
                let mut file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
                let len = file.metadata().map_err(|e| e.to_string())?.len();
                let index = segment::read_index(&mut file, len).map_err(|e| e.to_string())?;
                Ok((file, index))
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r)
        };
        let (mut file, index) = match opened {
            Ok(opened) => opened,
            Err(e) => {
                let _ = tokio::fs::remove_file(&path).await;
                return self.block_failed(name, None, &e, false);
            }
        };
        for (i, entry) in index.into_iter().enumerate() {
            let (back, rows) = tokio::task::spawn_blocking(move || {
                let rows = segment::read_block_frame(&mut file, &entry)
                    .and_then(|frame| segment::decode_block(&frame))
                    .map_err(|e| e.to_string());
                (file, rows)
            })
            .await
            .map_err(|e| Halt::Failed(format!("block decode panicked: {e}")))?;
            file = back;
            match rows {
                Ok(rows) => self.fold_rows(rows, filter, name).await?,
                Err(e) => {
                    if let Err(halt) = self.block_failed(name, Some(i as u32), &e, false) {
                        drop(file);
                        let _ = tokio::fs::remove_file(&path).await;
                        return Err(halt);
                    }
                }
            }
        }
        drop(file);
        let _ = tokio::fs::remove_file(&path).await;
        Ok(())
    }

    async fn fold_rows(
        &mut self,
        decoded: Vec<Row>,
        filter: &RowFilter,
        segment: &str,
    ) -> Result<(), Halt> {
        // The block arrived and decoded.
        self.consecutive_failures = 0;
        let mut undecodable = 0u32;
        let mut first_error = None;
        for row in decoded {
            if !filter.keep(&row) {
                continue;
            }
            // A DID whose records still wait in the fold's buffer holds
            // records too; the fold flushes them before the marker.
            if rows::is_marker(row.kind)
                && let Some(collections) = &self.held_only
                && !self.fold.buffers(&row.did)
                && !self
                    .state
                    .records
                    .holds_network_records(&row.did, Some(collections.as_slice()))
                    .await
                    .map_err(db)?
            {
                continue;
            }
            match rows::row_to_event(row) {
                Ok(Some(event)) => self
                    .fold
                    .apply(event)
                    .await
                    .map_err(|e| Halt::Failed(e.0))?,
                Ok(None) => {}
                Err(e) => {
                    undecodable += 1;
                    first_error.get_or_insert(e);
                }
            }
        }
        if let Some(error) = first_error {
            tracing::warn!(
                job_id = self.job_id,
                segment,
                undecodable,
                error,
                "skipped archive rows that do not decode"
            );
        }
        Ok(())
    }

    /// Note a block (or, with no index, a whole segment) that failed, to be
    /// recorded with its entry. An `outage` failure (the archive didn't
    /// answer, or answered an HTTP error) counts towards
    /// `MAX_CONSECUTIVE_FAILURES`; after that many in a row the job stops
    /// inside the entry, so its cursor is not saved and a resume refetches
    /// it. A block that arrived but doesn't decode or is over a cap is only
    /// recorded: it would fail the same way on every resume.
    fn block_failed(
        &mut self,
        segment: &str,
        block: Option<u32>,
        message: &str,
        outage: bool,
    ) -> Result<(), Halt> {
        tracing::warn!(
            job_id = self.job_id,
            segment,
            phase = phase(block),
            message,
            "replay block failed"
        );
        self.entry_failures
            .push((segment.to_string(), block, message.to_string()));
        if !outage {
            return Ok(());
        }
        self.consecutive_failures += 1;
        if self.consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
            return Err(Halt::Failed(format!(
                "the archive failed {MAX_CONSECUTIVE_FAILURES} blocks in a row; stopping (resume to continue)"
            )));
        }
        Ok(())
    }

    /// Record the current entry's failed blocks as `replay_block` errors and
    /// return how many there were. A block whose row can't be stored would
    /// never be retried, so that stops the job before its cursor moves past
    /// the entry.
    async fn record_failures(&mut self) -> Result<i32, Halt> {
        let failures = std::mem::take(&mut self.entry_failures);
        for (segment, block, message) in &failures {
            let phase = phase(*block);
            let failure = BackfillFailure {
                kind: BackfillErrorKind::ReplayBlock,
                message: message.clone(),
                retry_after: None,
            };
            let stored = self
                .errors
                .record(self.state, self.job_id, segment, None, &phase, &failure, 1)
                .await;
            if !stored {
                return Err(Halt::Failed(format!(
                    "could not record the failed block {phase} of {segment} (the job's error log is full or can't be written); stopping so it is not skipped"
                )));
            }
        }
        Ok(failures.len() as i32)
    }

    /// A `retry-failed` job: refetch exactly the recorded blocks (or whole
    /// segments) and fold what they hold for the job's collections and, for
    /// markers, for DIDs this instance holds records for (P2 makes the order
    /// irrelevant). Then replay the account and sync markers of every DID the
    /// refetched blocks wrote, as an ordinary job's marker sweeps do: a
    /// takedown later in the archive must purge what the retry wrote, even
    /// once its DID tombstone has expired.
    async fn retry_blocks(
        &mut self,
        collections: &[String],
        scope_dids: Option<Vec<String>>,
    ) -> Result<(), Halt> {
        if scope_dids.is_none() {
            self.held_only = Some(collections.to_vec());
        }
        self.fold.collect_written_dids();
        let filter = RowFilter {
            kinds: RowKinds::Both,
            collections: collections.iter().cloned().collect(),
            dids: scope_dids.map(|d| d.into_iter().collect()),
            after_seq: 0,
            through_seq: u64::MAX,
        };
        // Marker sweeps an earlier run of this job already recorded.
        let mut markers = sweeps::load(self.state, self.job_id).await.map_err(db)?;
        for (segment, block) in sweeps::retries(self.state, self.job_id).await.map_err(db)? {
            if self.stop_requested().await {
                return Err(Halt::Stopped);
            }
            self.entry_failures.clear();
            if block < 0 {
                self.whole_segment(&segment, &filter).await?;
            } else {
                let fetched = self.client.block(&segment, block as u32).await;
                match fetched_rows(fetched).await {
                    Ok(rows) => self.fold_rows(rows, &filter, &segment).await?,
                    Err((e, outage)) => {
                        self.block_failed(&segment, Some(block as u32), &e, outage)?
                    }
                }
            }
            self.fold.flush().await.map_err(|e| Halt::Failed(e.0))?;
            let failed = self.record_failures().await?;
            // Saved before the block is marked done, so a resumed run still
            // replays the markers of what it wrote.
            let wrote = self.fold.take_written_dids();
            self.add_marker_dids(&mut markers, wrote).await?;
            sweeps::retry_done(self.state, self.job_id, &segment, block)
                .await
                .map_err(db)?;
            self.count_bytes(failed).await?;
        }
        // The DID lists already name only DIDs with records.
        self.held_only = None;
        super::set_stage(self.state, self.job_id, "replaying_accounts").await;
        for sweep in markers.iter_mut().filter(|s| !s.done) {
            self.sweep(sweep).await?;
        }
        Ok(())
    }

    /// Add `dids` to the retry job's marker sweeps, at most `MAX_PLAN_DIDS`
    /// a sweep, persisting each change. Only a sweep that has not planned
    /// yet takes more DIDs.
    async fn add_marker_dids(
        &self,
        markers: &mut Vec<Sweep>,
        dids: HashSet<String>,
    ) -> Result<(), Halt> {
        let known: HashSet<&String> = markers
            .iter()
            .flat_map(|s| s.dids.iter().flatten())
            .collect();
        let mut fresh: Vec<String> = dids.into_iter().filter(|d| !known.contains(d)).collect();
        if fresh.is_empty() {
            return Ok(());
        }
        fresh.sort();
        let after = sweeps::after_seq(self.state, self.job_id)
            .await
            .map_err(db)?;
        let mut fresh = fresh.into_iter().peekable();
        while fresh.peek().is_some() {
            let open = markers.last_mut().filter(|s| {
                s.tip_seq.is_none()
                    && !s.done
                    && s.dids
                        .as_ref()
                        .is_some_and(|d| d.len() < sweeps::MAX_PLAN_DIDS)
            });
            match open {
                Some(sweep) => {
                    let dids = sweep.dids.get_or_insert_with(Vec::new);
                    let room = sweeps::MAX_PLAN_DIDS - dids.len();
                    dids.extend(fresh.by_ref().take(room));
                    sweeps::set_dids(self.state, self.job_id, sweep)
                        .await
                        .map_err(db)?;
                }
                None => {
                    let sweep = Sweep {
                        sweep: markers.iter().map(|s| s.sweep + 1).max().unwrap_or(0),
                        kind: SweepKind::Markers,
                        collections: Vec::new(),
                        dids: Some(fresh.by_ref().take(sweeps::MAX_PLAN_DIDS).collect()),
                        after_seq: after,
                        cursor_seq: after,
                        tip_seq: None,
                        last_did_id: None,
                        done: false,
                    };
                    sweeps::insert(self.state, self.job_id, std::slice::from_ref(&sweep))
                        .await
                        .map_err(db)?;
                    markers.push(sweep);
                }
            }
        }
        Ok(())
    }
}

/// Whether a failed request means the archive is down or refusing, rather
/// than that it answered with something unusable. The client has already
/// retried: a `Status` here is a 5xx that outlasted its backoff, or a 429
/// that kept the request throttled for `MAX_THROTTLED_FOR`, well past the
/// pacing that normally keeps replay within the archive's quota.
fn is_outage(error: &ArchiveError) -> bool {
    matches!(
        error,
        ArchiveError::Unreachable(_) | ArchiveError::Transport(_) | ArchiveError::Status { .. }
    )
}

/// A fetched block's rows, or why not and whether that is an outage.
async fn fetched_rows(fetched: Result<Vec<u8>, ArchiveError>) -> Result<Vec<Row>, (String, bool)> {
    match fetched {
        Ok(frame) => decode(frame).await.map_err(|e| (e, false)),
        Err(e) => Err((e.to_string(), is_outage(&e))),
    }
}

fn phase(block: Option<u32>) -> String {
    match block {
        Some(i) => format!("replay_block:{i}"),
        None => "replay_segment".to_string(),
    }
}

async fn decode(frame: Vec<u8>) -> Result<Vec<Row>, String> {
    tokio::task::spawn_blocking(move || segment::decode_block(&frame).map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r)
}

/// Close a gap in the live stream from the archive (plan ruling P7): start a
/// network replay job for everything after `after_seq`, and return its id and
/// the archive's sealed tip, where the live stream can resume at once.
pub(crate) async fn start_gap_replay(
    state: &AppState,
    after_seq: i64,
) -> Result<(String, i64), String> {
    let collections = super::record_collections(state)
        .await
        .map_err(|e| e.to_string())?;
    if collections.is_empty() {
        return Err("there are no record collections to replay".into());
    }
    let endpoint = crate::jetstream::config::parse_endpoint(&state.config.jetstream_url);
    let client = ArchiveClient::new(&endpoint, &state.archive);
    // The live consumer waits on this call, and the archive's retry policy
    // can stretch a hung or throttled archive into minutes. Bound it, so the
    // consumer resumes without the archive instead.
    let timeout = state.archive.gap_plan_timeout;
    let page = tokio::time::timeout(
        timeout,
        client.plan(&PlanRequest {
            kinds: vec!["commit".into()],
            collections: collections
                .iter()
                .take(sweeps::MAX_PLAN_COLLECTIONS)
                .cloned()
                .collect(),
            after_seq: after_seq.max(0) as u64,
            ..PlanRequest::default()
        }),
    )
    .await
    .map_err(|_| format!("the archive did not plan the gap within {timeout:?}"))?
    .map_err(|e| e.to_string())?;
    let tip = page.sealed_tip_seq as i64;
    if tip <= after_seq {
        return Err(format!(
            "the archive's sealed tip {tip} is not past seq {after_seq}"
        ));
    }
    // A repeated CursorTooOld before anything new is applied asks for the
    // same gap again. Reuse the job already filling it instead of stacking
    // another one.
    let existing: Option<(String,)> = crate::db::query_as(&crate::db::adapt_sql(
        "SELECT id FROM happyview_backfill_jobs \
         WHERE source = 'replay' AND replay_after_seq = ? AND status IN ('running', 'pausing', 'paused') \
         ORDER BY created_at DESC LIMIT 1",
        state.db_backend,
    ))
    .bind(after_seq)
    .fetch_optional(&state.backfill_db)
    .await
    .map_err(|e| e.to_string())?;
    if let Some((job_id,)) = existing {
        tracing::info!(
            job_id,
            after_seq,
            "a gap replay for this seq is already running; reusing it"
        );
        return Ok((job_id, tip));
    }
    let job_id = super::create_gap_replay_job(state, after_seq)
        .await
        .map_err(|e| e.to_string())?;
    super::log_event(
        &state.db,
        super::EventLog {
            event_type: "backfill.started".to_string(),
            severity: super::Severity::Info,
            actor_did: None,
            subject: None,
            detail: serde_json::json!({
                "job_id": job_id,
                "scope": "network",
                "source": "replay",
                "reason": "jetstream_gap",
                "after_seq": after_seq,
                "tip_seq": tip,
            }),
        },
        state.db_backend,
    )
    .await;
    let spawn_state = state.clone();
    let spawn_id = job_id.clone();
    tokio::spawn(async move { super::run_backfill_job(spawn_state, spawn_id).await });
    Ok((job_id, tip))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_unanswered_or_refused_request_is_an_outage() {
        assert!(is_outage(&ArchiveError::Unreachable("refused".into())));
        assert!(is_outage(&ArchiveError::Transport("reset".into())));
        assert!(is_outage(&ArchiveError::Status {
            status: 502,
            message: String::new(),
        }));
        assert!(!is_outage(&ArchiveError::TooLarge("over the cap".into())));
        assert!(!is_outage(&ArchiveError::GenerationChurn("churn".into())));
        assert!(!is_outage(&ArchiveError::Io("disk full".into())));
        assert!(!is_outage(&ArchiveError::NoArchive(404)));
        assert!(!is_outage(&ArchiveError::Unauthorized(401)));
    }

    #[tokio::test]
    async fn a_block_that_arrives_but_does_not_decode_is_no_outage() {
        let (_, outage) = fetched_rows(Ok(b"not a zstd frame".to_vec()))
            .await
            .unwrap_err();
        assert!(!outage);
        let (_, outage) = fetched_rows(Err(ArchiveError::TooLarge("big".into())))
            .await
            .unwrap_err();
        assert!(!outage);
        let (_, outage) = fetched_rows(Err(ArchiveError::Transport("reset".into())))
            .await
            .unwrap_err();
        assert!(outage);
    }
}
