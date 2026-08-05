//! `happyview.delete-collection` — delete every record in a collection.
//!
//! Batched deliberately. One unbounded `DELETE` is a single transaction whose
//! every dirtied page accumulates in the WAL until it commits, which is how a
//! large delete inflates disk usage instead of reducing it. Committing per batch
//! releases the write lock (so ingest can interleave) and lets the WAL
//! checkpoint between batches.

use crate::AppState;
use crate::db::{DatabaseBackend, adapt_sql};

use super::super::{Job, db};
use super::NativeOutcome;

/// Rows per transaction.
const BATCH_SIZE: i64 = 5000;

pub async fn run(state: &AppState, job: &Job) -> NativeOutcome {
    let collection = match job.input.get("collection").and_then(|v| v.as_str()) {
        Some(c) if !c.is_empty() => c.to_string(),
        _ => return NativeOutcome::Failed("input.collection is required".into()),
    };
    let backend = state.db_backend;

    let count_sql = adapt_sql(
        "SELECT COUNT(*) FROM happyview_records WHERE collection = ?",
        backend,
    );
    let total: i64 = match crate::db::query_as::<(i64,)>(&count_sql)
        .bind(&collection)
        .fetch_one(&state.db)
        .await
    {
        Ok((n,)) => n,
        Err(e) => return NativeOutcome::Failed(format!("failed to count records: {e}")),
    };

    let delete_sql = adapt_sql(
        "DELETE FROM happyview_records WHERE uri IN \
         (SELECT uri FROM happyview_records WHERE collection = ? LIMIT ?)",
        backend,
    );

    let mut deleted: i64 = 0;
    let mut stopped: Option<&'static str> = None;

    loop {
        if let Some(reason) = db::should_stop(state, &job.id).await {
            tracing::info!(job_id = %job.id, reason, deleted, "delete-collection stopping");
            stopped = Some(reason);
            break;
        }

        let affected = match crate::db::query(&delete_sql)
            .bind(&collection)
            .bind(BATCH_SIZE)
            .execute(&state.db)
            .await
        {
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

        // A batch smaller than BATCH_SIZE means the collection, as it stood
        // when this statement ran, has been drained. Waiting for a batch of
        // exactly zero instead would never happen on a collection still
        // receiving inserts (e.g. from Jetstream) — the loop would spin
        // indefinitely, holding the write lock and starving every other
        // queued job behind this one. Records that arrive after a job was
        // asked to delete a collection are out of scope for that job.
        if (affected as i64) < BATCH_SIZE {
            break;
        }
    }

    if let Some(reason) = stopped {
        // finalize()'s pause/cancel arms only set status — they don't persist
        // a result, so this is the only place the partial count and the
        // reason it stopped are recorded.
        let _ = db::update_progress(
            state,
            &job.id,
            &serde_json::json!({ "deleted": deleted, "total": total, "stopped": reason }),
        )
        .await;
    }

    // Incremental auto-vacuum makes reclamation possible but never automatic;
    // this is what actually returns the freed pages to the filesystem. It is a
    // no-op on instances that have not yet run the one-time vacuum. Runs
    // whether the loop drained the collection or was interrupted — a cancel is
    // the operator's escape hatch from a slow delete, and it should reclaim
    // whatever was freed up to that point rather than reclaiming nothing.
    if backend == DatabaseBackend::Sqlite
        && let Err(e) = crate::db::query("PRAGMA incremental_vacuum")
            .execute(&state.db)
            .await
    {
        tracing::warn!(error = %e, "incremental_vacuum after delete failed");
    }

    match stopped {
        Some(reason) => NativeOutcome::Completed(serde_json::json!({
            "deleted": deleted,
            "stopped": reason,
        })),
        None => NativeOutcome::Completed(serde_json::json!({ "deleted": deleted })),
    }
}
