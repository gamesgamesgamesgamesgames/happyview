//! What the backfill admin routes and the job event stream send and take.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Backfill types
// ---------------------------------------------------------------------------

#[derive(Deserialize, Clone)]
pub struct CreateBackfillBody {
    pub collection: Option<String>,
    pub did: Option<String>,
    pub dids: Option<Vec<String>>,
    /// `auto`, `replay` or `crawl`; `BACKFILL_SOURCE` when absent.
    pub source: Option<String>,
}

#[derive(Serialize)]
pub struct BackfillJob {
    pub id: String,
    pub collection: Option<String>,
    pub did: Option<String>,
    pub scope: String,
    pub status: String,
    pub stage: String,
    pub total_repos: Option<i32>,
    pub resolved_repos: Option<i32>,
    pub processed_repos: Option<i32>,
    pub total_records: Option<i32>,
    pub error: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub created_at: String,
    /// False while a network job is still listing repos from the relay; until
    /// then `total_repos` is the count discovered so far.
    pub discovery_complete: bool,
    /// For jobs on the bounded queue, how many of the most recent completions
    /// `repos?phase=fetched` lists. `None` for older jobs, which list every
    /// completed repo.
    pub recent_completions_limit: Option<i64>,
    /// `auto` until an `auto` job has probed the archive, then `replay` or
    /// `crawl`.
    pub source: String,
    /// What an executor's job said about itself ([`crate::JobHandle::note`]).
    pub note: Option<String>,
    /// Replay progress; `None` for a crawl.
    pub replay: Option<ReplayProgress>,
}

/// Where a replay job's running sweep is, and what it has cost.
#[derive(Clone, Debug, Serialize)]
pub struct ReplayProgress {
    /// Where the job started: 0, or the applied seq of a gap replay.
    pub after_seq: i64,
    /// The running sweep's cursor.
    pub through_seq: Option<i64>,
    /// The running sweep's pinned tip.
    pub tip_seq: Option<i64>,
    pub bytes: i64,
    pub blocks_failed: i32,
}

// ---------------------------------------------------------------------------
// Backfill event types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BackfillEvent {
    RepoDiscovered {
        job_id: String,
        did: String,
    },
    RepoResolved {
        job_id: String,
        did: String,
        pds_endpoint: String,
    },
    RepoFetched {
        job_id: String,
        did: String,
        pds_endpoint: String,
        records_fetched: i32,
    },
    JobCounters {
        job_id: String,
        total_repos: Option<i32>,
        resolved_repos: Option<i32>,
        processed_repos: Option<i32>,
        total_records: Option<i32>,
    },
    JobStageChanged {
        job_id: String,
        stage: String,
    },
    JobCompleted {
        job_id: String,
        status: String,
        error: Option<String>,
    },
    JobNote {
        job_id: String,
        note: String,
    },
    ReplayProgress {
        job_id: String,
        through_seq: i64,
        tip_seq: i64,
        bytes: i64,
        blocks_failed: i32,
    },
    /// Current job state, straight from the database.
    ///
    /// The SSE stream is otherwise delta-only over a lossy broadcast channel,
    /// so a client that connects mid-phase or misses an event has no way to
    /// recover. A snapshot is how it resyncs.
    JobSnapshot {
        job_id: String,
        status: String,
        stage: String,
        total_repos: Option<i32>,
        resolved_repos: Option<i32>,
        processed_repos: Option<i32>,
        total_records: Option<i32>,
        error_counts: serde_json::Value,
        discovery_complete: bool,
    },
}

// ---------------------------------------------------------------------------
// Backfill detail response types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct BackfillRepoEntry {
    pub did: String,
    pub pds_endpoint: Option<String>,
    pub status: String,
    pub records_fetched: i32,
}

#[derive(Serialize)]
pub struct BackfillReposResponse {
    pub repos: Vec<BackfillRepoEntry>,
    pub cursor: Option<String>,
}

#[derive(Serialize)]
pub struct PdsSummaryEntry {
    pub pds_endpoint: String,
    pub total_repos: i64,
    pub completed_repos: i64,
    pub total_records: i64,
}

#[derive(Serialize)]
pub struct PdsSummaryResponse {
    pub pds_endpoints: Vec<PdsSummaryEntry>,
}

#[derive(Serialize)]
pub struct BackfillErrorEntry {
    pub did: String,
    pub collection: Option<String>,
    pub phase: String,
    pub kind: String,
    pub message: String,
    pub attempts: i32,
    pub last_at: String,
}

#[derive(Serialize)]
pub struct BackfillErrorsResponse {
    pub errors: Vec<BackfillErrorEntry>,
    pub cursor: Option<String>,
    /// Exact per-kind totals, which stay correct past the detail cap.
    pub counts: Vec<BackfillErrorCount>,
    /// True when detail rows stopped being written.
    pub capped: bool,
    /// `ERROR_DETAIL_CAP`, served so the dashboard's truncation notice never
    /// hardcodes the number and goes stale if the constant is retuned.
    pub cap: i64,
}

/// One kind's total, carrying its own retryability.
///
/// `retryable` is served rather than duplicated in the dashboard: it is policy,
/// and `BackfillErrorKind::is_retryable` is its only definition. A TypeScript
/// copy would drift the first time a kind is added.
#[derive(Serialize)]
pub struct BackfillErrorCount {
    pub kind: String,
    pub count: i64,
    pub retryable: bool,
}
