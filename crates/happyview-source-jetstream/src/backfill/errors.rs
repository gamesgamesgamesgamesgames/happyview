//! A backfill job's failures: exact per-kind counts and capped detail rows.

pub use happyview_provider::failure::{BackfillErrorKind, BackfillFailure};

use std::collections::HashMap;

use happyview_provider::db::adapt_sql;

use super::{JobState, now_rfc3339};

/// Detail rows stored per job before the log stops growing.
///
/// Past this point `ErrorCounts` stays exact but individual rows are no longer
/// written: a pathological run against a dead collection must not put hundreds
/// of thousands of rows into SQLite, where the space does not come back easily.
pub const ERROR_DETAIL_CAP: i64 = 10_000;

#[derive(Debug, Default, Clone)]
pub(crate) struct ErrorCounts {
    counts: HashMap<BackfillErrorKind, i64>,
}

impl ErrorCounts {
    pub fn record(&mut self, kind: BackfillErrorKind) {
        *self.counts.entry(kind).or_insert(0) += 1;
    }

    pub fn get(&self, kind: BackfillErrorKind) -> i64 {
        self.counts.get(&kind).copied().unwrap_or(0)
    }

    /// Raise one kind's count to at least `count`, never lowering it.
    ///
    /// Lets a reader reconcile two partial views of the same job: the counts
    /// flushed to `error_counts` (authoritative above `ERROR_DETAIL_CAP`, where
    /// detail rows stop being written, but only written at phase end) against a
    /// live `COUNT(*)` over the detail table (authoritative below the cap, and
    /// current mid-run). Taking the larger is correct for both.
    pub fn raise_to(&mut self, kind: BackfillErrorKind, count: i64) {
        let entry = self.counts.entry(kind).or_insert(0);
        if count > *entry {
            *entry = count;
        }
    }

    pub fn total(&self) -> i64 {
        self.counts.values().sum()
    }

    pub fn to_json(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        for (kind, count) in &self.counts {
            if *count > 0 {
                map.insert((*kind).as_str().to_string(), (*count).into());
            }
        }
        serde_json::Value::Object(map)
    }

    pub fn from_json(value: &serde_json::Value) -> Self {
        let mut counts = HashMap::new();
        if let Some(map) = value.as_object() {
            for (key, val) in map {
                // Unknown keys are skipped, not fatal: a row written by a newer
                // version must stay readable by an older one.
                if let (Some(kind), Some(n)) = (BackfillErrorKind::parse(key), val.as_i64()) {
                    counts.insert(kind, n);
                }
            }
        }
        Self { counts }
    }
}

/// Record one failed DID, returning whether a row was written.
///
/// The cap is *not* checked here: the caller reserves its slot before calling,
/// so that the reservation and the insert cannot interleave with another
/// worker's across the `.await` below. See `ErrorRecorder::record`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn persist_failure(
    state: &JobState,
    job_id: &str,
    did: &str,
    collection: Option<&str>,
    phase: &str,
    failure: &BackfillFailure,
    attempts: u32,
) -> bool {
    let sql = adapt_sql(
        "INSERT INTO happyview_backfill_errors \
         (job_id, did, collection, phase, kind, message, attempts, last_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (job_id, did, phase) DO UPDATE SET \
         kind = excluded.kind, message = excluded.message, \
         attempts = excluded.attempts, last_at = excluded.last_at",
        state.db_backend,
    );

    let now = now_rfc3339();
    let result = happyview_provider::db::retry_on_busy(|| {
        happyview_provider::db::query(&sql)
            .bind(job_id)
            .bind(did)
            .bind(collection)
            .bind(phase)
            .bind(failure.kind.as_str())
            .bind(&failure.message)
            .bind(attempts as i32)
            .bind(&now)
            .execute(&state.backfill_db)
    })
    .await;

    if let Err(e) = &result {
        // The count is already recorded; only this detail row is missing.
        tracing::warn!(job_id, did, error = %e, "failed to store a backfill error row");
    }
    result.is_ok()
}

pub(crate) async fn flush_error_counts(state: &JobState, job_id: &str, counts: &ErrorCounts) {
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET error_counts = ? WHERE id = ?",
        state.db_backend,
    );
    let json = counts.to_json().to_string();
    super::job_write(state, job_id, "error_counts", || {
        happyview_provider::db::query(&sql)
            .bind(&json)
            .bind(job_id)
            .execute(&state.backfill_db)
    })
    .await;
}

use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};

/// One error sink per backfill job, shared by every worker in every phase.
///
/// The cap and the counts are both per-job invariants, and the fetch phase runs
/// one spawned worker per PDS endpoint — so neither can live in a local
/// variable without silently becoming per-worker. Holding them together behind
/// one handle makes the correct thing the only thing a caller can do.
pub struct ErrorRecorder {
    counts: Mutex<ErrorCounts>,
    stored: AtomicI64,
}

impl ErrorRecorder {
    /// Build a recorder, seeded from what this job has already written.
    ///
    /// Seeding matters because `resume_backfill_jobs` re-spawns jobs orphaned
    /// by a restart. A freshly-zeroed counter would let a resumed job write a
    /// second full `ERROR_DETAIL_CAP` of rows, and would forget every count it
    /// had already accumulated.
    pub async fn new(state: &JobState, job_id: &str) -> Self {
        let sql = adapt_sql(
            "SELECT COUNT(*) FROM happyview_backfill_errors WHERE job_id = ?",
            state.db_backend,
        );
        let stored: i64 = happyview_provider::db::query_as::<(i64,)>(&sql)
            .bind(job_id)
            .fetch_one(&state.backfill_db)
            .await
            .map(|(c,)| c)
            .unwrap_or(0);

        let counts_sql = adapt_sql(
            "SELECT error_counts FROM happyview_backfill_jobs WHERE id = ?",
            state.db_backend,
        );
        let counts = happyview_provider::db::query_as::<(Option<String>,)>(&counts_sql)
            .bind(job_id)
            .fetch_optional(&state.backfill_db)
            .await
            .ok()
            .flatten()
            .and_then(|(json,)| json)
            .and_then(|json| serde_json::from_str(&json).ok())
            .map(|v| ErrorCounts::from_json(&v))
            .unwrap_or_default();

        Self {
            counts: Mutex::new(counts),
            stored: AtomicI64::new(stored),
        }
    }

    /// Record one give-up: count it, then persist detail if under the cap.
    ///
    /// The count is unconditional. That is the whole point of splitting them —
    /// `ErrorCounts` must stay exact past `ERROR_DETAIL_CAP`, so the aggregate
    /// picture survives even when the detail log stops growing.
    ///
    /// Returns whether the detail row was stored: false past the cap or when
    /// the insert failed.
    #[allow(clippy::too_many_arguments)]
    pub async fn record(
        &self,
        state: &JobState,
        job_id: &str,
        did: &str,
        collection: Option<&str>,
        phase: &str,
        failure: &BackfillFailure,
        attempts: u32,
    ) -> bool {
        // Never hold the lock across an await.
        if let Ok(mut counts) = self.counts.lock() {
            counts.record(failure.kind);
        }

        // Reserve a detail-row slot atomically. Loading, awaiting the insert
        // and storing `old + 1` would let the resolver and every PDS worker
        // sharing this recorder read the same value and write the same value,
        // advancing the counter by 1 for N concurrent calls — which would make
        // the cap soft by roughly the concurrency factor. `fetch_add` claims
        // the slot before the await, and it is handed back if unusable.
        let slot = self.stored.fetch_add(1, Ordering::Relaxed);
        if slot >= ERROR_DETAIL_CAP {
            self.stored.fetch_sub(1, Ordering::Relaxed);
            return false;
        }

        let stored =
            persist_failure(state, job_id, did, collection, phase, failure, attempts).await;
        if !stored {
            self.stored.fetch_sub(1, Ordering::Relaxed);
        }
        stored
    }

    pub async fn flush(&self, state: &JobState, job_id: &str) {
        let snapshot = match self.counts.lock() {
            Ok(c) => c.clone(),
            Err(_) => return,
        };
        flush_error_counts(state, job_id, &snapshot).await;
    }
}

// ---------------------------------------------------------------------------
// The failure set
// ---------------------------------------------------------------------------

/// Where a `happyview_backfill_failures` row stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureState {
    /// A fetch retry is due. A resumed job queues the repo again.
    Pending,
    /// The job gave up on the repo.
    Failed,
    /// The repo was fetched since: by this job, or by a retry-failed job of
    /// it. retry-failed no longer selects it.
    Recovered,
}

impl FailureState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Failed => "failed",
            Self::Recovered => "recovered",
        }
    }
}

/// Record that `(did, collection)` is pending a retry or was given up on, in
/// the job's uncapped failure set.
///
/// Unlike `ErrorRecorder::record`, this is per collection and has no cap:
/// it is the list retry-failed reads, and what a resumed job queues again.
/// A failed write is logged as `backfill.write_failed`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn mark_failure(
    state: &JobState,
    job_id: &str,
    did: &str,
    collection: &str,
    phase: &str,
    kind: BackfillErrorKind,
    failure_state: FailureState,
    attempts: u32,
) {
    let sql = adapt_sql(
        "INSERT INTO happyview_backfill_failures \
         (job_id, did, collection, phase, kind, state, attempts, last_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (job_id, did, collection) DO UPDATE SET \
         phase = excluded.phase, kind = excluded.kind, state = excluded.state, \
         attempts = excluded.attempts, last_at = excluded.last_at",
        state.db_backend,
    );
    let now = now_rfc3339();
    let attempts = i32::try_from(attempts).unwrap_or(i32::MAX);
    super::job_write(state, job_id, "failure_set", || {
        happyview_provider::db::query(&sql)
            .bind(job_id)
            .bind(did)
            .bind(collection)
            .bind(phase)
            .bind(kind.as_str())
            .bind(failure_state.as_str())
            .bind(attempts)
            .bind(&now)
            .execute(&state.backfill_db)
    })
    .await;
}

/// Mark `did`'s rows for `collection` recovered: in `job_id`'s failure set,
/// and in that of the job `job_id` retries (`retry_of`), whose resolve
/// failures for `did` are recovered too, since `did` resolved here.
///
/// `job_id IN (…) AND did = ?` leads so Postgres can use the primary key: it
/// runs in every unit's completion, over a table no sweep clears. A `NULL`
/// `retry_of` drops out of the `IN`.
///
/// Binds: now, job_id, job_id, did, job_id, collection, job_id, collection.
pub(crate) const RECOVER_SQL: &str = "UPDATE happyview_backfill_failures SET state = 'recovered', last_at = ? \
     WHERE job_id IN (?, (SELECT retry_of FROM happyview_backfill_jobs WHERE id = ?)) \
       AND did = ? AND state <> 'recovered' \
       AND ((job_id = ? AND collection = ?) \
            OR (job_id <> ? AND (collection = ? OR phase = 'resolve')))";

/// [`RECOVER_SQL`] for one collection fetched whole.
pub(crate) async fn mark_recovered(state: &JobState, job_id: &str, did: &str, collection: &str) {
    let sql = adapt_sql(RECOVER_SQL, state.db_backend);
    let now = now_rfc3339();
    super::job_write(state, job_id, "failure_set", || {
        happyview_provider::db::query(&sql)
            .bind(&now)
            .bind(job_id)
            .bind(job_id)
            .bind(did)
            .bind(job_id)
            .bind(collection)
            .bind(job_id)
            .bind(collection)
            .execute(&state.backfill_db)
    })
    .await;
}

/// Queue a bounded job's pending fetch retries again, one unit per
/// `(did, collection)`, so a job paused or restarted mid-retry fetches them
/// on resume. Each unit added counts toward `total_repos`, as its completion
/// counts toward `processed_repos`. Returns the units added.
pub async fn requeue_pending_failures(state: &JobState, job_id: &str) -> Result<u64, sqlx::Error> {
    let backend = state.db_backend;
    let insert = adapt_sql(
        "INSERT INTO happyview_backfill_queue (job_id, collection, did) \
         SELECT job_id, collection, did FROM happyview_backfill_failures \
         WHERE job_id = ? AND state = 'pending' AND phase = 'fetch' AND collection <> '' \
         ON CONFLICT DO NOTHING",
        backend,
    );
    let total = adapt_sql(
        &format!(
            "UPDATE happyview_backfill_jobs SET {} WHERE id = ?",
            super::saturating_counter("total_repos", "?", backend)
        ),
        backend,
    );
    happyview_provider::db::retry_on_busy(|| async {
        let mut tx = state.backfill_db.begin().await?;
        let added = happyview_provider::db::query(&insert)
            .bind(job_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if added > 0 {
            happyview_provider::db::query(&total)
                .bind(i32::try_from(added).unwrap_or(i32::MAX))
                .bind(job_id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(added)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_counts_round_trip_through_json() {
        let mut counts = ErrorCounts::default();
        counts.record(BackfillErrorKind::DnsFailure);
        counts.record(BackfillErrorKind::DnsFailure);
        counts.record(BackfillErrorKind::RepoNotFound);

        let restored = ErrorCounts::from_json(&counts.to_json());
        assert_eq!(restored.total(), 3);
        assert_eq!(restored.get(BackfillErrorKind::DnsFailure), 2);
        assert_eq!(restored.get(BackfillErrorKind::RepoNotFound), 1);
        assert_eq!(restored.get(BackfillErrorKind::Timeout), 0);
    }

    #[test]
    fn error_counts_json_omits_zero_kinds() {
        let mut counts = ErrorCounts::default();
        counts.record(BackfillErrorKind::Timeout);
        let json = counts.to_json();
        assert_eq!(json["timeout"], 1);
        assert!(
            json.get("dns_failure").is_none(),
            "zero kinds should not be serialised"
        );
    }

    #[test]
    fn error_counts_from_unknown_kind_is_ignored() {
        // A row written by a future version must not panic an older reader.
        let json = serde_json::json!({ "dns_failure": 2, "some_future_kind": 9 });
        let counts = ErrorCounts::from_json(&json);
        assert_eq!(counts.total(), 2);
    }

    #[test]
    fn error_recorder_counts_are_exact_past_the_cap() {
        // The cap bounds detail rows; it must never bound the counts.
        let mut counts = ErrorCounts::default();
        for _ in 0..(ERROR_DETAIL_CAP + 500) {
            counts.record(BackfillErrorKind::DnsFailure);
        }
        assert_eq!(
            counts.get(BackfillErrorKind::DnsFailure),
            ERROR_DETAIL_CAP + 500
        );
    }
}
