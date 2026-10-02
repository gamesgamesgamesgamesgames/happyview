use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::AnyPool;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
    Error,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Info => write!(f, "info"),
            Severity::Warn => write!(f, "warn"),
            Severity::Error => write!(f, "error"),
        }
    }
}

pub struct EventLog {
    pub event_type: String,
    pub severity: Severity,
    pub actor_did: Option<String>,
    pub subject: Option<String>,
    pub detail: Value,
}

/// A filter over `happyview_event_logs`, shared by the admin list and count
/// endpoints, the purge job, and the retention sweep.
///
/// Sharing one builder is what keeps the purge's preview count truthful: a
/// count that re-implemented these clauses could disagree with what the purge
/// actually deletes, and the whole confirm step rests on that number.
#[derive(Debug, Default, Clone)]
pub struct EventFilter {
    pub event_type: Option<String>,
    /// Comma-separated list of `event_type` prefixes, e.g. `record,script`.
    pub category: Option<String>,
    /// Comma-separated list of severities, e.g. `warn,error`.
    pub severity: Option<String>,
    /// Substring match against `subject`.
    pub subject: Option<String>,
    /// Inclusive lower bound on `created_at`.
    pub after: Option<String>,
    /// Exclusive upper bound on `created_at`.
    pub before: Option<String>,
}

impl EventFilter {
    /// True when no field is set, meaning the filter selects every row.
    pub fn is_empty(&self) -> bool {
        self.event_type.is_none()
            && self.category.is_none()
            && self.severity.is_none()
            && self.subject.is_none()
            && self.after.is_none()
            && self.before.is_none()
    }

    /// A SQL fragment to append after `WHERE 1=1`, plus its binds in order.
    pub fn build(&self) -> (String, Vec<String>) {
        let mut sql = String::new();
        let mut binds: Vec<String> = Vec::new();

        if let Some(event_type) = &self.event_type {
            sql.push_str(" AND event_type = ?");
            binds.push(event_type.clone());
        }
        if let Some(category) = &self.category {
            let parts: Vec<&str> = category.split(',').collect();
            let clauses: Vec<&str> = parts.iter().map(|_| "event_type LIKE ?").collect();
            sql.push_str(&format!(" AND ({})", clauses.join(" OR ")));
            for part in parts {
                binds.push(format!("{part}.%"));
            }
        }
        if let Some(severity) = &self.severity {
            let parts: Vec<&str> = severity.split(',').collect();
            let placeholders: Vec<&str> = parts.iter().map(|_| "?").collect();
            sql.push_str(&format!(" AND severity IN ({})", placeholders.join(",")));
            for part in parts {
                binds.push(part.to_string());
            }
        }
        if let Some(subject) = &self.subject {
            sql.push_str(" AND subject LIKE ?");
            binds.push(format!("%{subject}%"));
        }
        if let Some(after) = &self.after {
            sql.push_str(" AND created_at >= ?");
            binds.push(after.clone());
        }
        if let Some(before) = &self.before {
            sql.push_str(" AND created_at < ?");
            binds.push(before.clone());
        }

        (sql, binds)
    }
}

/// Normalise an RFC3339 timestamp to the `+00:00` form this codebase stores.
///
/// `created_at` is TEXT on SQLite and compared lexicographically, so a
/// `Z`-suffixed input would sort against `+00:00`-suffixed rows incorrectly.
pub fn normalize_rfc3339(s: &str) -> Result<String, String> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&chrono::Utc).to_rfc3339())
        .map_err(|e| format!("invalid timestamp '{s}': {e}"))
}

/// What one retention sweep did.
pub struct SweepOutcome {
    pub deleted: u64,
    pub vacuumed: bool,
}

/// Rows deleted per transaction, matching the purge job's batch size and for
/// the same reason (see `jobs::native::purge_event_logs`): one unbounded
/// `DELETE` accumulates every dirtied page in the WAL until it commits, which
/// inflates disk instead of reducing it, and holds the SQLite write lock
/// against Jetstream ingest for the whole transaction. Retention is now a
/// runtime setting rather than a boot-time constant, so an operator can point
/// this at an arbitrarily large backlog (e.g. set retention to `1` on a
/// multi-million-row instance) — batching is what keeps that safe.
const SWEEP_BATCH_SIZE: i64 = 5000;

/// Delete every event log row strictly older than `cutoff`, then reclaim.
///
/// `cutoff` must be RFC3339 with a `+00:00` offset — the form `now_rfc3339()`
/// writes. On SQLite `created_at` is TEXT and this is a string comparison, so a
/// differently-formatted cutoff silently mis-selects rows.
pub async fn sweep_with_cutoff(
    db: &AnyPool,
    cutoff: &str,
    backend: DatabaseBackend,
) -> SweepOutcome {
    let filter = EventFilter {
        before: Some(cutoff.to_string()),
        ..Default::default()
    };
    let (frag, binds) = filter.build();
    let sql = adapt_sql(
        &format!(
            "DELETE FROM happyview_event_logs WHERE id IN \
             (SELECT id FROM happyview_event_logs WHERE 1=1{frag} LIMIT ?)"
        ),
        backend,
    );

    let mut deleted: u64 = 0;
    loop {
        let mut q = crate::db::query(&sql);
        for bind in &binds {
            q = q.bind(bind);
        }
        q = q.bind(SWEEP_BATCH_SIZE);

        let affected = match q.execute(db).await {
            Ok(result) => result.rows_affected(),
            Err(e) => {
                tracing::warn!("failed to clean up event logs: {e}");
                break;
            }
        };
        deleted += affected;

        // A short batch means the selection is drained. Waiting for exactly
        // zero would spin forever on a live instance without a lower bound.
        if (affected as i64) < SWEEP_BATCH_SIZE {
            break;
        }
    }

    // Incremental auto-vacuum makes reclamation possible but never automatic;
    // this is what returns the freed pages to the filesystem. Guarded on having
    // deleted something, so an idle instance does not issue a pointless pragma
    // every hour forever, and it runs once after the whole sweep rather than
    // once per batch.
    let mut vacuumed = false;
    if deleted > 0 && backend == DatabaseBackend::Sqlite {
        match crate::db::query("PRAGMA incremental_vacuum")
            .execute(db)
            .await
        {
            Ok(_) => vacuumed = true,
            Err(e) => {
                tracing::warn!(error = %e, "incremental_vacuum after event log cleanup failed")
            }
        }
    }

    SweepOutcome { deleted, vacuumed }
}

/// Sweep rows older than `retention_days`. A retention of `0` is a no-op.
pub async fn run_retention_sweep(
    db: &AnyPool,
    retention_days: u32,
    backend: DatabaseBackend,
) -> SweepOutcome {
    if retention_days == 0 {
        return SweepOutcome {
            deleted: 0,
            vacuumed: false,
        };
    }
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(retention_days as i64)).to_rfc3339();
    sweep_with_cutoff(db, &cutoff, backend).await
}

/// Hourly retention sweep.
///
/// The retention value is re-read every iteration rather than captured at
/// spawn, so an operator can change it without a restart. A value of `0`
/// therefore skips the sweep and leaves this task running — it must not return,
/// or disabling retention once would be permanent until the process restarts.
pub async fn spawn_retention_cleanup(db: AnyPool, backend: DatabaseBackend) {
    let interval = tokio::time::Duration::from_secs(3600);

    loop {
        tokio::time::sleep(interval).await;

        let retention_days = match crate::admin::settings::get_setting(
            &db,
            "event_log_retention_days",
            backend,
        )
        .await
        {
            Some(v) => v.parse::<u32>().unwrap_or_else(|e| {
                tracing::warn!(
                    value = %v,
                    error = %e,
                    "event_log_retention_days is not a valid non-negative integer, defaulting to 30"
                );
                30
            }),
            None => 30,
        };

        let outcome = run_retention_sweep(&db, retention_days, backend).await;
        if outcome.deleted > 0 {
            tracing::info!(
                count = outcome.deleted,
                vacuumed = outcome.vacuumed,
                "cleaned up old event logs"
            );
        }
    }
}

pub async fn log_event(db: &AnyPool, event: EventLog, backend: DatabaseBackend) {
    let severity = event.severity.to_string();
    let detail_str = serde_json::to_string(&event.detail).unwrap_or_else(|_| "{}".to_string());
    let id = Uuid::new_v4().to_string();
    let created_at = now_rfc3339();

    let sql = adapt_sql(
        "INSERT INTO happyview_event_logs (id, event_type, severity, actor_did, subject, detail, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        backend,
    );

    let result = crate::db::query(&sql)
        .bind(&id)
        .bind(&event.event_type)
        .bind(&severity)
        .bind(&event.actor_did)
        .bind(&event.subject)
        .bind(&detail_str)
        .bind(&created_at)
        .execute(db)
        .await;

    if let Err(e) = result {
        tracing::warn!(event_type = %event.event_type, "failed to log event: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_display() {
        assert_eq!(Severity::Info.to_string(), "info");
        assert_eq!(Severity::Warn.to_string(), "warn");
        assert_eq!(Severity::Error.to_string(), "error");
    }

    #[test]
    fn severity_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Severity::Info).unwrap(), "\"info\"");
        assert_eq!(
            serde_json::to_string(&Severity::Error).unwrap(),
            "\"error\""
        );
    }

    #[test]
    fn event_log_construction() {
        let event = EventLog {
            event_type: "lexicon.created".to_string(),
            severity: Severity::Info,
            actor_did: Some("did:plc:test".to_string()),
            subject: Some("com.example.test".to_string()),
            detail: serde_json::json!({"revision": 1}),
        };
        assert_eq!(event.event_type, "lexicon.created");
        assert_eq!(event.severity, Severity::Info);
        assert_eq!(event.actor_did.unwrap(), "did:plc:test");
    }

    #[test]
    fn empty_filter_produces_no_clauses() {
        let (frag, binds) = EventFilter::default().build();
        assert_eq!(frag, "");
        assert!(binds.is_empty());
        assert!(EventFilter::default().is_empty());
    }

    #[test]
    fn event_type_is_an_exact_match() {
        let f = EventFilter {
            event_type: Some("record.skipped".into()),
            ..Default::default()
        };
        let (frag, binds) = f.build();
        assert_eq!(frag, " AND event_type = ?");
        assert_eq!(binds, vec!["record.skipped".to_string()]);
        assert!(!f.is_empty());
    }

    #[test]
    fn category_expands_to_prefix_matches() {
        let f = EventFilter {
            category: Some("record,script".into()),
            ..Default::default()
        };
        let (frag, binds) = f.build();
        assert_eq!(frag, " AND (event_type LIKE ? OR event_type LIKE ?)");
        assert_eq!(binds, vec!["record.%".to_string(), "script.%".to_string()]);
    }

    #[test]
    fn severity_expands_to_an_in_list() {
        let f = EventFilter {
            severity: Some("warn,error".into()),
            ..Default::default()
        };
        let (frag, binds) = f.build();
        assert_eq!(frag, " AND severity IN (?,?)");
        assert_eq!(binds, vec!["warn".to_string(), "error".to_string()]);
    }

    #[test]
    fn subject_is_a_substring_match() {
        let f = EventFilter {
            subject: Some("did:plc:abc".into()),
            ..Default::default()
        };
        let (frag, binds) = f.build();
        assert_eq!(frag, " AND subject LIKE ?");
        assert_eq!(binds, vec!["%did:plc:abc%".to_string()]);
    }

    #[test]
    fn after_is_inclusive_and_before_is_exclusive() {
        let f = EventFilter {
            after: Some("2026-07-01T00:00:00+00:00".into()),
            before: Some("2026-08-01T00:00:00+00:00".into()),
            ..Default::default()
        };
        let (frag, binds) = f.build();
        assert_eq!(frag, " AND created_at >= ? AND created_at < ?");
        assert_eq!(
            binds,
            vec![
                "2026-07-01T00:00:00+00:00".to_string(),
                "2026-08-01T00:00:00+00:00".to_string()
            ]
        );
    }

    #[test]
    fn bind_order_follows_fragment_order() {
        let f = EventFilter {
            event_type: Some("record.skipped".into()),
            severity: Some("info".into()),
            before: Some("2026-08-01T00:00:00+00:00".into()),
            ..Default::default()
        };
        let (frag, binds) = f.build();
        assert_eq!(
            frag,
            " AND event_type = ? AND severity IN (?) AND created_at < ?"
        );
        assert_eq!(
            binds,
            vec![
                "record.skipped".to_string(),
                "info".to_string(),
                "2026-08-01T00:00:00+00:00".to_string()
            ]
        );
    }

    #[test]
    fn normalize_rfc3339_rewrites_a_z_suffix_to_an_offset() {
        assert_eq!(
            normalize_rfc3339("2026-08-01T00:00:00Z").unwrap(),
            "2026-08-01T00:00:00+00:00"
        );
    }

    #[test]
    fn normalize_rfc3339_passes_through_an_offset_form() {
        assert_eq!(
            normalize_rfc3339("2026-08-01T00:00:00+00:00").unwrap(),
            "2026-08-01T00:00:00+00:00"
        );
    }

    #[test]
    fn normalize_rfc3339_rejects_garbage() {
        assert!(normalize_rfc3339("not-a-timestamp").is_err());
    }

    async fn pool_with_events() -> AnyPool {
        let pool = crate::test_support::memory_pool().await;
        crate::db::query(
            "CREATE TABLE happyview_event_logs (
                id TEXT PRIMARY KEY,
                event_type TEXT NOT NULL,
                severity TEXT NOT NULL DEFAULT 'info',
                actor_did TEXT,
                subject TEXT,
                detail TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("create event_logs");
        pool
    }

    async fn insert_at(pool: &AnyPool, id: &str, created_at: &str) {
        crate::db::query(
            "INSERT INTO happyview_event_logs (id, event_type, severity, detail, created_at)
             VALUES (?, 'record.skipped', 'info', '{}', ?)",
        )
        .bind(id)
        .bind(created_at)
        .execute(pool)
        .await
        .expect("insert event");
    }

    async fn remaining_ids(pool: &AnyPool) -> Vec<String> {
        let rows: Vec<(String,)> =
            crate::db::query_as("SELECT id FROM happyview_event_logs ORDER BY id")
                .fetch_all(pool)
                .await
                .expect("select ids");
        rows.into_iter().map(|r| r.0).collect()
    }

    /// The regression guard for the format mismatch. The old sweep compared
    /// RFC3339 `created_at` values against SQLite's `datetime('now', …)`, whose
    /// space separator sorts before `T`, so a row written earlier on the
    /// cutoff's own calendar day was never swept.
    #[tokio::test]
    async fn sweep_deletes_rows_earlier_in_the_day_than_the_cutoff() {
        let pool = pool_with_events().await;
        insert_at(&pool, "older", "2026-07-08T08:00:00+00:00").await;
        insert_at(&pool, "boundary", "2026-07-08T16:00:00+00:00").await;
        insert_at(&pool, "newer", "2026-07-08T20:00:00+00:00").await;

        let outcome =
            sweep_with_cutoff(&pool, "2026-07-08T16:00:00+00:00", DatabaseBackend::Sqlite).await;

        assert_eq!(outcome.deleted, 1, "only the row before the cutoff goes");
        assert_eq!(
            remaining_ids(&pool).await,
            vec!["boundary".to_string(), "newer".to_string()],
            "the cutoff itself is exclusive, so `boundary` survives"
        );
    }

    #[tokio::test]
    async fn sweep_reports_vacuumed_only_when_it_deleted_something() {
        let pool = pool_with_events().await;
        insert_at(&pool, "old", "2026-01-01T00:00:00+00:00").await;

        let hit =
            sweep_with_cutoff(&pool, "2026-07-08T16:00:00+00:00", DatabaseBackend::Sqlite).await;
        assert_eq!(hit.deleted, 1);
        assert!(hit.vacuumed, "a sweep that freed pages reclaims them");

        let miss =
            sweep_with_cutoff(&pool, "2026-07-08T16:00:00+00:00", DatabaseBackend::Sqlite).await;
        assert_eq!(miss.deleted, 0);
        assert!(
            !miss.vacuumed,
            "an idle sweep must not issue an hourly no-op pragma"
        );
    }

    #[tokio::test]
    async fn retention_of_zero_deletes_nothing() {
        let pool = pool_with_events().await;
        insert_at(&pool, "ancient", "2020-01-01T00:00:00+00:00").await;

        let outcome = run_retention_sweep(&pool, 0, DatabaseBackend::Sqlite).await;

        assert_eq!(outcome.deleted, 0);
        assert!(!outcome.vacuumed);
        assert_eq!(remaining_ids(&pool).await, vec!["ancient".to_string()]);
    }

    #[tokio::test]
    async fn retention_deletes_past_the_cutoff_and_keeps_recent_rows() {
        let pool = pool_with_events().await;
        let now = chrono::Utc::now();
        let ancient = (now - chrono::Duration::days(90)).to_rfc3339();
        let recent = (now - chrono::Duration::days(1)).to_rfc3339();
        insert_at(&pool, "ancient", &ancient).await;
        insert_at(&pool, "recent", &recent).await;

        let outcome = run_retention_sweep(&pool, 30, DatabaseBackend::Sqlite).await;

        assert_eq!(outcome.deleted, 1);
        assert_eq!(remaining_ids(&pool).await, vec!["recent".to_string()]);
    }
}
