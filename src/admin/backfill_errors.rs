use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};

use crate::http_retry::parse_retry_after;

/// How a backfill attempt failed.
///
/// The split between retryable and not follows one principle: a definitive
/// answer from a live server is information, and a transport failure is the
/// absence of it. Information is final; absence is worth asking about again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackfillErrorKind {
    DnsFailure,
    ConnectionFailed,
    Timeout,
    PdsServerError,
    RateLimited,
    DidDocNotFound,
    DidDocForbidden,
    DidDocInvalid,
    RepoNotFound,
    RepoDeactivated,
    RepoTakendown,
    Other,
}

impl BackfillErrorKind {
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::DnsFailure
                | Self::ConnectionFailed
                | Self::Timeout
                | Self::PdsServerError
                | Self::RateLimited
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::DnsFailure => "dns_failure",
            Self::ConnectionFailed => "connection_failed",
            Self::Timeout => "timeout",
            Self::PdsServerError => "pds_server_error",
            Self::RateLimited => "rate_limited",
            Self::DidDocNotFound => "did_doc_not_found",
            Self::DidDocForbidden => "did_doc_forbidden",
            Self::DidDocInvalid => "did_doc_invalid",
            Self::RepoNotFound => "repo_not_found",
            Self::RepoDeactivated => "repo_deactivated",
            Self::RepoTakendown => "repo_takendown",
            Self::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "dns_failure" => Self::DnsFailure,
            "connection_failed" => Self::ConnectionFailed,
            "timeout" => Self::Timeout,
            "pds_server_error" => Self::PdsServerError,
            "rate_limited" => Self::RateLimited,
            "did_doc_not_found" => Self::DidDocNotFound,
            "did_doc_forbidden" => Self::DidDocForbidden,
            "did_doc_invalid" => Self::DidDocInvalid,
            "repo_not_found" => Self::RepoNotFound,
            "repo_deactivated" => Self::RepoDeactivated,
            "repo_takendown" => Self::RepoTakendown,
            "other" => Self::Other,
            _ => return None,
        })
    }

    /// Every kind, for zero-filling count maps and driving the dashboard legend.
    pub fn all() -> [Self; 12] {
        [
            Self::DnsFailure,
            Self::ConnectionFailed,
            Self::Timeout,
            Self::PdsServerError,
            Self::RateLimited,
            Self::DidDocNotFound,
            Self::DidDocForbidden,
            Self::DidDocInvalid,
            Self::RepoNotFound,
            Self::RepoDeactivated,
            Self::RepoTakendown,
            Self::Other,
        ]
    }
}

const MAX_MESSAGE_CHARS: usize = 220;

fn truncate(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.chars().count() <= MAX_MESSAGE_CHARS {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(MAX_MESSAGE_CHARS - 1).collect();
    out.push('…');
    out
}

#[derive(Clone, Debug)]
pub struct BackfillFailure {
    pub kind: BackfillErrorKind,
    pub message: String,
    pub retry_after: Option<Duration>,
}

impl BackfillFailure {
    pub fn from_reqwest(e: &reqwest::Error) -> Self {
        let message = crate::error::describe_error_chain(e);
        // Order matters: a DNS failure also reports `is_connect()`, so it must
        // be checked first or every NXDOMAIN is mislabelled as a refused
        // connection.
        let kind = if e.is_timeout() {
            BackfillErrorKind::Timeout
        } else if message.contains("dns error") || message.contains("failed to lookup address") {
            BackfillErrorKind::DnsFailure
        } else if e.is_connect() {
            BackfillErrorKind::ConnectionFailed
        } else {
            BackfillErrorKind::Other
        };
        Self {
            kind,
            message: truncate(&message),
            retry_after: None,
        }
    }

    pub fn from_did_doc_response(status: StatusCode, headers: &HeaderMap) -> Self {
        let (kind, retry_after) = match status {
            StatusCode::TOO_MANY_REQUESTS => (
                BackfillErrorKind::RateLimited,
                Some(Duration::from_secs(parse_retry_after(headers))),
            ),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                (BackfillErrorKind::DidDocForbidden, None)
            }
            StatusCode::NOT_FOUND | StatusCode::GONE => (BackfillErrorKind::DidDocNotFound, None),
            s if s.is_server_error() => (BackfillErrorKind::PdsServerError, None),
            _ => (BackfillErrorKind::Other, None),
        };
        Self {
            kind,
            message: truncate(&format!("DID document request returned {status}")),
            retry_after,
        }
    }

    pub fn from_pds_response(status: StatusCode, body: &str, headers: &HeaderMap) -> Self {
        let error_code = serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or_default();

        let (kind, retry_after) = match status {
            StatusCode::TOO_MANY_REQUESTS => (
                BackfillErrorKind::RateLimited,
                Some(Duration::from_secs(parse_retry_after(headers))),
            ),
            s if s.is_server_error() => (BackfillErrorKind::PdsServerError, None),
            _ => {
                let kind = match error_code.as_str() {
                    "RepoDeactivated" => BackfillErrorKind::RepoDeactivated,
                    "RepoTakendown" => BackfillErrorKind::RepoTakendown,
                    "RepoNotFound" => BackfillErrorKind::RepoNotFound,
                    // atproto PDSes answer a missing repo as a generic
                    // InvalidRequest whose message names the cause.
                    _ if body.contains("Could not find repo") => BackfillErrorKind::RepoNotFound,
                    _ => BackfillErrorKind::Other,
                };
                (kind, None)
            }
        };

        let message = if body.trim().is_empty() {
            format!("PDS returned {status}")
        } else {
            format!("PDS returned {status}: {}", body.trim())
        };
        Self {
            kind,
            message: truncate(&message),
            retry_after,
        }
    }
}

use std::collections::HashMap;

use crate::AppState;
use crate::db::adapt_sql;

/// Detail rows stored per job before the log stops growing.
///
/// Past this point `ErrorCounts` stays exact but individual rows are no longer
/// written: a pathological run against a dead collection must not put hundreds
/// of thousands of rows into SQLite, where the space does not come back easily.
pub const ERROR_DETAIL_CAP: i64 = 10_000;

#[derive(Debug, Default, Clone)]
pub struct ErrorCounts {
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
pub async fn persist_failure(
    state: &AppState,
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

    let now = crate::db::now_rfc3339();
    let result = crate::db::query(&sql)
        .bind(job_id)
        .bind(did)
        .bind(collection)
        .bind(phase)
        .bind(failure.kind.as_str())
        .bind(&failure.message)
        .bind(attempts as i32)
        .bind(&now)
        .execute(&state.backfill_db)
        .await;

    result.is_ok()
}

pub async fn flush_error_counts(state: &AppState, job_id: &str, counts: &ErrorCounts) {
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET error_counts = ? WHERE id = ?",
        state.db_backend,
    );
    let _ = crate::db::query(&sql)
        .bind(counts.to_json().to_string())
        .bind(job_id)
        .execute(&state.backfill_db)
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
    pub async fn new(state: &AppState, job_id: &str) -> Self {
        let sql = adapt_sql(
            "SELECT COUNT(*) FROM happyview_backfill_errors WHERE job_id = ?",
            state.db_backend,
        );
        let stored: i64 = crate::db::query_as::<(i64,)>(&sql)
            .bind(job_id)
            .fetch_one(&state.backfill_db)
            .await
            .map(|(c,)| c)
            .unwrap_or(0);

        let counts_sql = adapt_sql(
            "SELECT error_counts FROM happyview_backfill_jobs WHERE id = ?",
            state.db_backend,
        );
        let counts = crate::db::query_as::<(Option<String>,)>(&counts_sql)
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
    #[allow(clippy::too_many_arguments)]
    pub async fn record(
        &self,
        state: &AppState,
        job_id: &str,
        did: &str,
        collection: Option<&str>,
        phase: &str,
        failure: &BackfillFailure,
        attempts: u32,
    ) {
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
            return;
        }

        if !persist_failure(state, job_id, did, collection, phase, failure, attempts).await {
            self.stored.fetch_sub(1, Ordering::Relaxed);
        }
    }

    pub async fn flush(&self, state: &AppState, job_id: &str) {
        let snapshot = match self.counts.lock() {
            Ok(c) => c.clone(),
            Err(_) => return,
        };
        flush_error_counts(state, job_id, &snapshot).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;
    use reqwest::header::HeaderMap;

    #[test]
    fn retryable_kinds_are_exactly_the_transport_and_server_ones() {
        use BackfillErrorKind::*;
        for k in [
            DnsFailure,
            ConnectionFailed,
            Timeout,
            PdsServerError,
            RateLimited,
        ] {
            assert!(k.is_retryable(), "{k:?} should be retryable");
        }
        for k in [
            DidDocNotFound,
            DidDocForbidden,
            DidDocInvalid,
            RepoNotFound,
            RepoDeactivated,
            RepoTakendown,
            Other,
        ] {
            assert!(!k.is_retryable(), "{k:?} should not be retryable");
        }
    }

    #[test]
    fn did_doc_404_is_not_found() {
        let f = BackfillFailure::from_did_doc_response(StatusCode::NOT_FOUND, &HeaderMap::new());
        assert_eq!(f.kind, BackfillErrorKind::DidDocNotFound);
        assert!(!f.kind.is_retryable());
    }

    #[test]
    fn did_doc_403_is_forbidden_not_missing() {
        // www.viruus.zip answers 403 from a WAF. Reporting that as "not found"
        // is what made the original log unreadable.
        let f = BackfillFailure::from_did_doc_response(StatusCode::FORBIDDEN, &HeaderMap::new());
        assert_eq!(f.kind, BackfillErrorKind::DidDocForbidden);
    }

    #[test]
    fn did_doc_5xx_is_retryable() {
        let f = BackfillFailure::from_did_doc_response(StatusCode::BAD_GATEWAY, &HeaderMap::new());
        assert_eq!(f.kind, BackfillErrorKind::PdsServerError);
        assert!(f.kind.is_retryable());
    }

    #[test]
    fn pds_400_could_not_find_repo_is_repo_not_found() {
        // Verbatim from pioppino.us-west.host.bsky.network.
        let body = r#"{"error":"InvalidRequest","message":"Could not find repo: did:plc:izvzfrxkzrjqhyere4ch4qok"}"#;
        let f =
            BackfillFailure::from_pds_response(StatusCode::BAD_REQUEST, body, &HeaderMap::new());
        assert_eq!(f.kind, BackfillErrorKind::RepoNotFound);
        assert!(!f.kind.is_retryable());
    }

    #[test]
    fn pds_deactivated_and_takendown_are_distinguished() {
        let d = BackfillFailure::from_pds_response(
            StatusCode::BAD_REQUEST,
            r#"{"error":"RepoDeactivated"}"#,
            &HeaderMap::new(),
        );
        assert_eq!(d.kind, BackfillErrorKind::RepoDeactivated);

        let t = BackfillFailure::from_pds_response(
            StatusCode::BAD_REQUEST,
            r#"{"error":"RepoTakendown"}"#,
            &HeaderMap::new(),
        );
        assert_eq!(t.kind, BackfillErrorKind::RepoTakendown);
    }

    #[test]
    fn pds_429_carries_retry_after() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "30".parse().unwrap());
        let f = BackfillFailure::from_pds_response(StatusCode::TOO_MANY_REQUESTS, "", &headers);
        assert_eq!(f.kind, BackfillErrorKind::RateLimited);
        assert_eq!(f.retry_after, Some(Duration::from_secs(30)));
    }

    #[test]
    fn pds_5xx_is_server_error() {
        let f = BackfillFailure::from_pds_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "",
            &HeaderMap::new(),
        );
        assert_eq!(f.kind, BackfillErrorKind::PdsServerError);
    }

    #[test]
    fn message_is_truncated_but_marked() {
        let body = "x".repeat(500);
        let f =
            BackfillFailure::from_pds_response(StatusCode::BAD_REQUEST, &body, &HeaderMap::new());
        assert!(f.message.chars().count() <= 220, "message not truncated");
        assert!(f.message.ends_with('…'));
    }

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

    #[test]
    fn kind_str_round_trips_for_storage() {
        use BackfillErrorKind::*;
        for k in [
            DnsFailure,
            ConnectionFailed,
            Timeout,
            PdsServerError,
            RateLimited,
            DidDocNotFound,
            DidDocForbidden,
            DidDocInvalid,
            RepoNotFound,
            RepoDeactivated,
            RepoTakendown,
            Other,
        ] {
            assert_eq!(BackfillErrorKind::parse(k.as_str()), Some(k));
        }
    }
}
