//! `happyview.purge-event-logs` — delete event log rows matching a filter.
//!
//! Batched for the same reason as `delete_collection`: one unbounded `DELETE`
//! is a single transaction whose every dirtied page accumulates in the WAL
//! until it commits, which inflates disk usage instead of reducing it.

use crate::AppState;
use crate::db::{DatabaseBackend, adapt_sql};
use crate::event_log::EventFilter;

use super::super::{Job, db};
use super::NativeOutcome;

/// Rows per transaction.
const BATCH_SIZE: i64 = 5000;

/// Read the filter out of the job's input. Absent fields are unset, so an empty
/// input selects every row — the endpoint is what decides whether that is
/// allowed, not this.
///
/// Timestamps are trusted to already be `+00:00`-normalised. That holds today
/// because `purge_events` (`src/admin/events.rs`) is this job's sole enqueue
/// path, and it normalises via `filter_from_query` before building the job
/// input. A `Z`-suffixed `before`/`after` reaching this function would
/// silently mis-select against the TEXT `created_at` column — if a second
/// enqueue path is ever added, it must normalise first too.
fn filter_from_input(input: &serde_json::Value) -> EventFilter {
    let field = |key: &str| {
        input
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    };
    EventFilter {
        event_type: field("event_type"),
        category: field("category"),
        severity: field("severity"),
        subject: field("subject"),
        after: field("after"),
        before: field("before"),
    }
}

pub async fn run(state: &AppState, job: &Job) -> NativeOutcome {
    let backend = state.db_backend;
    let (frag, binds) = filter_from_input(&job.input).build();

    let count_sql = adapt_sql(
        &format!("SELECT COUNT(*) FROM happyview_event_logs WHERE 1=1{frag}"),
        backend,
    );
    let mut count_q = crate::db::query_as::<(i64,)>(&count_sql);
    for bind in &binds {
        count_q = count_q.bind(bind);
    }
    let total: i64 = match count_q.fetch_one(&state.db).await {
        Ok((n,)) => n,
        Err(e) => return NativeOutcome::Failed(format!("failed to count events: {e}")),
    };

    let delete_sql = adapt_sql(
        &format!(
            "DELETE FROM happyview_event_logs WHERE id IN \
             (SELECT id FROM happyview_event_logs WHERE 1=1{frag} LIMIT ?)"
        ),
        backend,
    );

    let mut deleted: i64 = 0;
    let mut stopped: Option<&'static str> = None;

    loop {
        if let Some(reason) = db::should_stop(state, &job.id).await {
            tracing::info!(job_id = %job.id, reason, deleted, "purge-event-logs stopping");
            stopped = Some(reason);
            break;
        }

        let mut q = crate::db::query(&delete_sql);
        for bind in &binds {
            q = q.bind(bind);
        }
        q = q.bind(BATCH_SIZE);

        let affected = match q.execute(&state.db).await {
            Ok(r) => r.rows_affected(),
            Err(e) => return NativeOutcome::Failed(format!("failed to delete batch: {e}")),
        };
        deleted += affected as i64;

        let _ = db::update_progress(
            state,
            &job.id,
            &serde_json::json!({ "deleted": deleted, "total": total }),
        )
        .await;

        // A short batch means the selection has been drained. Waiting for
        // exactly zero would never arrive on a live instance: without a
        // `before` bound, new events keep matching, and the loop would spin
        // holding the write lock and starving the single-threaded job worker.
        // Events written after a purge was requested are out of that purge's scope.
        if (affected as i64) < BATCH_SIZE {
            break;
        }
    }

    if let Some(reason) = stopped {
        let _ = db::update_progress(
            state,
            &job.id,
            &serde_json::json!({ "deleted": deleted, "total": total, "stopped": reason }),
        )
        .await;
    }

    // Runs on both the drained and the interrupted path — a cancel is the
    // operator's escape hatch from a slow purge, and it should reclaim whatever
    // was freed up to that point rather than reclaiming nothing.
    if backend == DatabaseBackend::Sqlite
        && let Err(e) = crate::db::query("PRAGMA incremental_vacuum")
            .execute(&state.db)
            .await
    {
        tracing::warn!(error = %e, "incremental_vacuum after purge failed");
    }

    match stopped {
        Some(reason) => NativeOutcome::Completed(serde_json::json!({
            "deleted": deleted,
            "stopped": reason,
        })),
        None => NativeOutcome::Completed(serde_json::json!({ "deleted": deleted })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_from_input_reads_every_field() {
        let input = serde_json::json!({
            "event_type": "record.skipped",
            "category": "record",
            "severity": "info",
            "subject": "did:plc:abc",
            "after": "2026-07-01T00:00:00+00:00",
            "before": "2026-08-01T00:00:00+00:00",
        });
        let f = filter_from_input(&input);
        assert_eq!(f.event_type.as_deref(), Some("record.skipped"));
        assert_eq!(f.category.as_deref(), Some("record"));
        assert_eq!(f.severity.as_deref(), Some("info"));
        assert_eq!(f.subject.as_deref(), Some("did:plc:abc"));
        assert_eq!(f.after.as_deref(), Some("2026-07-01T00:00:00+00:00"));
        assert_eq!(f.before.as_deref(), Some("2026-08-01T00:00:00+00:00"));
    }

    #[test]
    fn filter_from_input_treats_missing_fields_as_unset() {
        let f = filter_from_input(&serde_json::json!({}));
        assert!(f.is_empty());
    }
}
