//! One-time SQLite VACUUM.
//!
//! `VACUUM` does not clean up in place — it rebuilds the database into a new
//! file and swaps it. That is why it needs the database's size again in free
//! space and exclusive access, and why it runs at startup (before any worker
//! spawns) rather than against a live instance.
//!
//! Existing instances need it because `PRAGMA auto_vacuum = INCREMENTAL` is a
//! no-op on a populated database until a rebuild applies it.

use serde::{Deserialize, Serialize};
use sqlx::AnyPool;
use tracing::info;

use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::maintenance::disk::{self, VacuumFeasibility};

const KEY_REQUESTED: &str = "sqlite_vacuum_requested_at";
const KEY_ATTEMPT: &str = "sqlite_vacuum_attempt_started_at";
const KEY_COMPLETED: &str = "sqlite_vacuum_completed_at";
const KEY_RESULT: &str = "sqlite_vacuum_last_result";

/// The outcome of the most recent attempt, persisted so the dashboard can
/// report it long after the boot that produced it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VacuumResult {
    /// `"ok"` or `"failed"`.
    pub status: String,
    pub at: String,
    pub db_bytes_before: u64,
    pub db_bytes_after: u64,
    pub reclaimed_bytes: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VacuumStatus {
    pub requested_at: Option<String>,
    pub attempt_started_at: Option<String>,
    /// When a vacuum last completed, or when one was marked unnecessary via
    /// `mark_not_needed`. Purely informational — it suppresses the
    /// dashboard's "you should vacuum" prompt but does not gate
    /// `run_if_requested`, which is driven by `requested_at` alone.
    pub completed_at: Option<String>,
    pub last_result: Option<VacuumResult>,
}

async fn get<'e, E>(
    executor: E,
    backend: DatabaseBackend,
    key: &str,
) -> Result<Option<String>, AppError>
where
    E: sqlx::Executor<'e, Database = sqlx::Any>,
{
    let sql = adapt_sql(
        "SELECT value FROM happyview_instance_settings WHERE key = ?",
        backend,
    );
    let row: Option<(String,)> = crate::db::query_as(&sql)
        .bind(key)
        .fetch_optional(executor)
        .await
        .map_err(|e| AppError::Internal(format!("failed to read {key}: {e}")))?;
    Ok(row.map(|r| r.0))
}

async fn put<'e, E>(
    executor: E,
    backend: DatabaseBackend,
    key: &str,
    value: &str,
) -> Result<(), AppError>
where
    E: sqlx::Executor<'e, Database = sqlx::Any>,
{
    let sql = adapt_sql(
        "INSERT INTO happyview_instance_settings (key, value, updated_at) VALUES (?, ?, ?)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        backend,
    );
    crate::db::query(&sql)
        .bind(key)
        .bind(value)
        .bind(now_rfc3339())
        .execute(executor)
        .await
        .map_err(|e| AppError::Internal(format!("failed to write {key}: {e}")))?;
    Ok(())
}

async fn clear<'e, E>(executor: E, backend: DatabaseBackend, key: &str) -> Result<(), AppError>
where
    E: sqlx::Executor<'e, Database = sqlx::Any>,
{
    let sql = adapt_sql(
        "DELETE FROM happyview_instance_settings WHERE key = ?",
        backend,
    );
    crate::db::query(&sql)
        .bind(key)
        .execute(executor)
        .await
        .map_err(|e| AppError::Internal(format!("failed to clear {key}: {e}")))?;
    Ok(())
}

pub async fn read_status(
    pool: &AnyPool,
    backend: DatabaseBackend,
) -> Result<VacuumStatus, AppError> {
    let last_result = get(pool, backend, KEY_RESULT)
        .await?
        .and_then(|raw| serde_json::from_str::<VacuumResult>(&raw).ok());
    Ok(VacuumStatus {
        requested_at: get(pool, backend, KEY_REQUESTED).await?,
        attempt_started_at: get(pool, backend, KEY_ATTEMPT).await?,
        completed_at: get(pool, backend, KEY_COMPLETED).await?,
        last_result,
    })
}

/// Arm the vacuum. It runs on the next startup.
pub async fn request(pool: &AnyPool, backend: DatabaseBackend) -> Result<(), AppError> {
    put(pool, backend, KEY_REQUESTED, &now_rfc3339()).await
}

/// Disarm a scheduled vacuum.
pub async fn cancel_request(pool: &AnyPool, backend: DatabaseBackend) -> Result<(), AppError> {
    clear(pool, backend, KEY_REQUESTED).await
}

/// Record that no vacuum is needed — used at setup, since a database created by
/// this version already has incremental auto-vacuum and nothing stranded.
/// This only suppresses the dashboard's prompt; it does not prevent a later
/// `request()` from arming (and running) a vacuum.
pub async fn mark_not_needed(pool: &AnyPool, backend: DatabaseBackend) -> Result<(), AppError> {
    put(pool, backend, KEY_COMPLETED, &now_rfc3339()).await
}

/// Persist the outcome of an attempt and disarm, all in one transaction — a
/// crash between clearing `requested_at` and writing `completed_at` after a
/// *successful* vacuum would otherwise leave the instance unarmed with no
/// record of success, nagging forever about a vacuum that already worked.
async fn record_result(
    pool: &AnyPool,
    backend: DatabaseBackend,
    result: &VacuumResult,
) -> Result<(), AppError> {
    let json = serde_json::to_string(result)
        .map_err(|e| AppError::Internal(format!("failed to serialize vacuum result: {e}")))?;

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| AppError::Internal(format!("failed to begin transaction: {e}")))?;

    put(&mut *tx, backend, KEY_RESULT, &json).await?;
    clear(&mut *tx, backend, KEY_REQUESTED).await?;
    clear(&mut *tx, backend, KEY_ATTEMPT).await?;
    if result.status == "ok" {
        put(&mut *tx, backend, KEY_COMPLETED, &result.at).await?;
    }

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("failed to commit vacuum result: {e}")))?;
    Ok(())
}

fn failure(at: String, before: u64, error: String) -> VacuumResult {
    VacuumResult {
        status: "failed".into(),
        at,
        db_bytes_before: before,
        db_bytes_after: before,
        reclaimed_bytes: 0,
        error: Some(error),
    }
}

/// Run the vacuum if one was requested.
///
/// Gated on `requested_at` alone — see `VacuumStatus::completed_at`. Returns
/// `Ok(None)` when nothing is armed. Any attempt — successful or not —
/// disarms (clears `requested_at`), so a vacuum that cannot succeed does not
/// retry on every boot.
pub async fn run_if_requested(
    pool: &AnyPool,
    backend: DatabaseBackend,
    db_url: &str,
) -> Result<Option<VacuumResult>, AppError> {
    if backend != DatabaseBackend::Sqlite {
        return Ok(None);
    }
    let status = read_status(pool, backend).await?;
    if status.requested_at.is_none() {
        return Ok(None);
    }
    run_armed(pool, backend, &status, disk::report(db_url), db_url).await
}

/// The decision tree for an armed vacuum, split out from `run_if_requested`
/// so it can be exercised directly against a synthetic `DiskReport` — real
/// free-disk numbers are normally far too large to hit the `Insufficient` or
/// `Unknown` branches in a test.
async fn run_armed(
    pool: &AnyPool,
    backend: DatabaseBackend,
    status: &VacuumStatus,
    report: Option<disk::DiskReport>,
    db_url: &str,
) -> Result<Option<VacuumResult>, AppError> {
    let now = now_rfc3339();
    let before = report.as_ref().map(|r| r.db_bytes).unwrap_or(0);

    // An attempt marker left over from a previous boot means the process died
    // mid-rebuild and never reached the code that records an error.
    if status.attempt_started_at.is_some() {
        let result = failure(
            now,
            before,
            "a previous vacuum attempt did not complete; the process may have run \
             out of memory or disk, or it may have been killed by a container \
             healthcheck or orchestrator timeout while the rebuild was still in \
             progress (VACUUM can take minutes to hours on a large database). \
             Re-schedule it once the cause is addressed."
                .into(),
        );
        record_result(pool, backend, &result).await?;
        return Ok(Some(result));
    }

    if let Some(r) = &report {
        match disk::feasibility(
            r.db_bytes + r.wal_bytes,
            r.db_fs_free,
            r.temp_fs_free,
            &r.db_path,
            &r.temp_path,
            r.same_filesystem,
        ) {
            VacuumFeasibility::Ok => {}
            VacuumFeasibility::Insufficient {
                needed,
                available,
                path,
            } => {
                let result = failure(
                    now,
                    before,
                    format!(
                        "not enough free space: needs ~{} on {path}, {} available",
                        human_bytes(needed),
                        human_bytes(available)
                    ),
                );
                record_result(pool, backend, &result).await?;
                return Ok(Some(result));
            }
            VacuumFeasibility::Unknown { path } => {
                let result = failure(
                    now,
                    before,
                    format!(
                        "could not measure free space on {path}; skipping the vacuum until this is resolved"
                    ),
                );
                record_result(pool, backend, &result).await?;
                return Ok(Some(result));
            }
        }
    }

    put(pool, backend, KEY_ATTEMPT, &now).await?;

    // Log before the rebuild starts, not after: `VACUUM` on a large database
    // blocks the server from binding its port for minutes to hours, and this
    // is the only place that says so — an operator staring at a container
    // that hasn't come up needs this line in the logs to know why, and how
    // big a rebuild they're waiting on.
    info!(
        db_bytes = before,
        db_size = %human_bytes(before),
        "starting scheduled database vacuum; the server will not accept \
         connections until it completes"
    );

    let result = match run(pool, backend).await {
        Ok(()) => {
            let after = disk::report(db_url).map(|r| r.db_bytes).unwrap_or(before);
            VacuumResult {
                status: "ok".into(),
                at: now_rfc3339(),
                db_bytes_before: before,
                db_bytes_after: after,
                reclaimed_bytes: before.saturating_sub(after),
                error: None,
            }
        }
        Err(e) => failure(now_rfc3339(), before, e.to_string()),
    };
    record_result(pool, backend, &result).await?;
    Ok(Some(result))
}

/// Perform the rebuild. All three statements must share one connection: the
/// `auto_vacuum` change only takes effect via the VACUUM that follows it.
/// A no-op on Postgres — `VACUUM`/`auto_vacuum` here are SQLite pragmas.
pub async fn run(pool: &AnyPool, backend: DatabaseBackend) -> Result<(), AppError> {
    if backend != DatabaseBackend::Sqlite {
        return Ok(());
    }

    let mut conn = pool
        .acquire()
        .await
        .map_err(|e| AppError::Internal(format!("failed to acquire connection: {e}")))?;

    crate::db::query("PRAGMA auto_vacuum = INCREMENTAL")
        .execute(&mut *conn)
        .await
        .map_err(|e| AppError::Internal(format!("failed to set auto_vacuum: {e}")))?;
    crate::db::query("VACUUM")
        .execute(&mut *conn)
        .await
        .map_err(|e| AppError::Internal(format!("VACUUM failed: {e}")))?;
    crate::db::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&mut *conn)
        .await
        .map_err(|e| AppError::Internal(format!("WAL checkpoint failed: {e}")))?;
    Ok(())
}

/// Render a byte count for an operator-facing message.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    // The cutoff is slightly below 1024 so a value that would display as
    // "1024.0" after rounding to one decimal place bumps to the next unit
    // instead (e.g. 1_048_575 B is 1 byte short of 1 MiB but rounds to
    // "1024.0 KiB" without this).
    while value >= 1023.95 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_scales_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(8 * 1024 * 1024 * 1024), "8.0 GiB");
    }

    #[test]
    fn human_bytes_rounds_up_to_the_next_unit_at_the_boundary() {
        // 1_048_575 B is 1 byte short of 1 MiB; formatting the KiB value to
        // one decimal place rounds it to "1024.0", so the unit must bump too.
        assert_eq!(human_bytes(1_048_575), "1.0 MiB");
    }

    async fn temp_sqlite_pool() -> (AnyPool, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!("hv-vacuum-unit-{}.db", uuid::Uuid::new_v4()));
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = crate::db::connect(&url, DatabaseBackend::Sqlite).await;
        (pool, path)
    }

    fn cleanup_sqlite_files(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    /// Real free-disk numbers are normally far too large to hit the
    /// `Insufficient` branch, so this drives `run_armed` directly against a
    /// synthetic `DiskReport` to prove the refusal actually disarms — a bug
    /// here means production retries a doomed vacuum on every boot.
    #[tokio::test]
    async fn insufficient_disk_records_failure_and_disarms() {
        let (pool, path) = temp_sqlite_pool().await;

        request(&pool, DatabaseBackend::Sqlite).await.unwrap();
        let status = read_status(&pool, DatabaseBackend::Sqlite).await.unwrap();

        let report = disk::DiskReport {
            db_bytes: 10 * 1024 * 1024 * 1024,
            wal_bytes: 0,
            db_fs_free: Some(1024),
            temp_fs_free: Some(1024),
            same_filesystem: false,
            db_path: "/fake/db".into(),
            temp_path: "/fake/tmp".into(),
        };

        let out = run_armed(
            &pool,
            DatabaseBackend::Sqlite,
            &status,
            Some(report),
            "sqlite://unused",
        )
        .await
        .expect("run_armed failed")
        .expect("expected a result");

        assert_eq!(out.status, "failed");
        assert!(
            out.error
                .unwrap_or_default()
                .contains("not enough free space"),
            "error should name the disk shortfall"
        );

        let status_after = read_status(&pool, DatabaseBackend::Sqlite).await.unwrap();
        assert!(
            status_after.requested_at.is_none(),
            "must disarm on insufficient disk, not retry forever"
        );

        drop(pool);
        cleanup_sqlite_files(&path);
    }

    #[tokio::test]
    async fn unmeasurable_disk_records_failure_and_disarms() {
        let (pool, path) = temp_sqlite_pool().await;

        request(&pool, DatabaseBackend::Sqlite).await.unwrap();
        let status = read_status(&pool, DatabaseBackend::Sqlite).await.unwrap();

        let report = disk::DiskReport {
            db_bytes: 10 * 1024 * 1024 * 1024,
            wal_bytes: 0,
            db_fs_free: None,
            temp_fs_free: Some(50 * 1024 * 1024 * 1024),
            same_filesystem: false,
            db_path: "/fake/db".into(),
            temp_path: "/fake/tmp".into(),
        };

        let out = run_armed(
            &pool,
            DatabaseBackend::Sqlite,
            &status,
            Some(report),
            "sqlite://unused",
        )
        .await
        .expect("run_armed failed")
        .expect("expected a result");

        assert_eq!(out.status, "failed");
        assert!(
            out.error
                .unwrap_or_default()
                .contains("could not measure free space"),
            "error should name the measurement failure, not claim the disk is full"
        );

        let status_after = read_status(&pool, DatabaseBackend::Sqlite).await.unwrap();
        assert!(
            status_after.requested_at.is_none(),
            "must disarm, not retry forever"
        );

        drop(pool);
        cleanup_sqlite_files(&path);
    }
}
