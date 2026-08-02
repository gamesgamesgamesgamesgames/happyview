mod common;

use happyview::db::DatabaseBackend;
use happyview::maintenance::vacuum;
use serial_test::serial;

/// All tests below share one SQLite file (`TEST_DATABASE_URL`) and mutate the
/// same `happyview_instance_settings` keys, so they must not run concurrently
/// with each other (see `#[serial]` on each).
async fn sqlite_pool() -> Option<sqlx::AnyPool> {
    if common::db::test_backend() != DatabaseBackend::Sqlite {
        eprintln!("skipped (SQLite-only)");
        return None;
    }
    Some(common::db::test_pool().await)
}

/// A database created by this version must have incremental auto-vacuum, which
/// is what makes `PRAGMA incremental_vacuum` able to release pages later.
#[tokio::test]
#[serial]
async fn fresh_sqlite_database_has_incremental_auto_vacuum() {
    common::require_db!();
    let Some(pool) = sqlite_pool().await else {
        return;
    };

    let (mode,): (i64,) = happyview::db::query_as("PRAGMA auto_vacuum")
        .fetch_one(&pool)
        .await
        .expect("failed to read auto_vacuum");

    assert_eq!(mode, 2, "expected auto_vacuum=INCREMENTAL (2), got {mode}");
}

#[tokio::test]
#[serial]
async fn run_if_requested_is_a_noop_when_not_armed() {
    common::require_db!();
    let Some(pool) = sqlite_pool().await else {
        return;
    };
    common::db::truncate_all(&pool).await;

    let out = vacuum::run_if_requested(&pool, DatabaseBackend::Sqlite, &test_url())
        .await
        .expect("run_if_requested failed");
    assert!(out.is_none(), "expected no run when unarmed");
}

#[tokio::test]
#[serial]
async fn requesting_then_running_records_completion_and_disarms() {
    common::require_db!();
    let Some(pool) = sqlite_pool().await else {
        return;
    };
    common::db::truncate_all(&pool).await;

    vacuum::request(&pool, DatabaseBackend::Sqlite)
        .await
        .unwrap();
    let status = vacuum::read_status(&pool, DatabaseBackend::Sqlite)
        .await
        .unwrap();
    assert!(status.requested_at.is_some());
    assert!(status.completed_at.is_none());

    let out = vacuum::run_if_requested(&pool, DatabaseBackend::Sqlite, &test_url())
        .await
        .expect("run_if_requested failed")
        .expect("expected a result");
    assert_eq!(out.status, "ok", "vacuum failed: {:?}", out.error);

    let status = vacuum::read_status(&pool, DatabaseBackend::Sqlite)
        .await
        .unwrap();
    assert!(status.completed_at.is_some(), "completed_at not written");
    assert!(status.requested_at.is_none(), "requested_at not cleared");
    assert!(
        status.attempt_started_at.is_none(),
        "attempt marker not cleared"
    );

    // Second call must not run again.
    let again = vacuum::run_if_requested(&pool, DatabaseBackend::Sqlite, &test_url())
        .await
        .unwrap();
    assert!(again.is_none());
}

#[tokio::test]
#[serial]
async fn a_crashed_previous_attempt_is_detected_and_disarmed() {
    common::require_db!();
    let Some(pool) = sqlite_pool().await else {
        return;
    };
    common::db::truncate_all(&pool).await;

    // Simulate a process that died mid-vacuum: armed, attempt started, no result.
    vacuum::request(&pool, DatabaseBackend::Sqlite)
        .await
        .unwrap();
    set_setting(
        &pool,
        "sqlite_vacuum_attempt_started_at",
        "2026-01-01T00:00:00Z",
    )
    .await;

    let out = vacuum::run_if_requested(&pool, DatabaseBackend::Sqlite, &test_url())
        .await
        .expect("run_if_requested failed")
        .expect("expected a result describing the crash");

    assert_eq!(out.status, "failed");
    let err = out.error.unwrap_or_default();
    assert!(
        err.contains("did not complete"),
        "error should name the crash, got: {err}"
    );

    let status = vacuum::read_status(&pool, DatabaseBackend::Sqlite)
        .await
        .unwrap();
    assert!(
        status.requested_at.is_none(),
        "must disarm, not retry forever"
    );
    assert!(status.completed_at.is_none());
}

#[tokio::test]
#[serial]
async fn mark_not_needed_records_completion_without_running() {
    common::require_db!();
    let Some(pool) = sqlite_pool().await else {
        return;
    };
    common::db::truncate_all(&pool).await;

    vacuum::mark_not_needed(&pool, DatabaseBackend::Sqlite)
        .await
        .unwrap();
    let status = vacuum::read_status(&pool, DatabaseBackend::Sqlite)
        .await
        .unwrap();
    assert!(status.completed_at.is_some());
}

/// `cancel_request` is currently called by nothing in production, but it is
/// the only way to back out of an armed vacuum before it runs.
#[tokio::test]
#[serial]
async fn cancel_request_disarms_without_running() {
    common::require_db!();
    let Some(pool) = sqlite_pool().await else {
        return;
    };
    common::db::truncate_all(&pool).await;

    vacuum::request(&pool, DatabaseBackend::Sqlite)
        .await
        .unwrap();
    vacuum::cancel_request(&pool, DatabaseBackend::Sqlite)
        .await
        .unwrap();

    let status = vacuum::read_status(&pool, DatabaseBackend::Sqlite)
        .await
        .unwrap();
    assert!(
        status.requested_at.is_none(),
        "cancel_request must clear the arm"
    );

    let out = vacuum::run_if_requested(&pool, DatabaseBackend::Sqlite, &test_url())
        .await
        .unwrap();
    assert!(out.is_none(), "a cancelled request must not run");
}

/// The other tests only prove VACUUM returned without error. This proves it
/// actually did something: bloat the file, delete the rows (which, under
/// incremental auto-vacuum, does *not* shrink the file on its own), then
/// vacuum and check the file got smaller.
#[tokio::test]
#[serial]
async fn a_real_vacuum_reclaims_freed_space() {
    common::require_db!();
    let Some(pool) = sqlite_pool().await else {
        return;
    };
    common::db::truncate_all(&pool).await;

    happyview::db::query(
        "CREATE TABLE IF NOT EXISTS hv_vacuum_scratch (id INTEGER PRIMARY KEY, data BLOB)",
    )
    .execute(&pool)
    .await
    .expect("failed to create scratch table");

    let blob = vec![0u8; 300_000];
    for _ in 0..20 {
        happyview::db::query("INSERT INTO hv_vacuum_scratch (data) VALUES (?)")
            .bind(blob.clone())
            .execute(&pool)
            .await
            .expect("failed to insert scratch row");
    }
    happyview::db::query("DELETE FROM hv_vacuum_scratch")
        .execute(&pool)
        .await
        .expect("failed to delete scratch rows");
    // Force the bloat out of the WAL and into the main file before measuring,
    // so `db_bytes_before` actually reflects it.
    happyview::db::query("PRAGMA wal_checkpoint(FULL)")
        .execute(&pool)
        .await
        .expect("failed to checkpoint before measuring");

    vacuum::request(&pool, DatabaseBackend::Sqlite)
        .await
        .unwrap();

    let out = vacuum::run_if_requested(&pool, DatabaseBackend::Sqlite, &test_url())
        .await
        .expect("run_if_requested failed")
        .expect("expected a result");

    assert_eq!(out.status, "ok", "vacuum failed: {:?}", out.error);
    assert!(out.db_bytes_before > 0, "expected a nonzero before size");
    assert!(
        out.db_bytes_after < out.db_bytes_before,
        "expected the file to shrink: before={}, after={}",
        out.db_bytes_before,
        out.db_bytes_after
    );
    assert!(
        out.reclaimed_bytes > 0,
        "expected reclaimed_bytes > 0, got {}",
        out.reclaimed_bytes
    );

    happyview::db::query("DROP TABLE IF EXISTS hv_vacuum_scratch")
        .execute(&pool)
        .await
        .ok();
}

fn test_url() -> String {
    std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL")
}

async fn set_setting(pool: &sqlx::AnyPool, key: &str, value: &str) {
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_instance_settings (key, value, updated_at) VALUES (?, ?, ?)",
        DatabaseBackend::Sqlite,
    );
    happyview::db::query(&sql)
        .bind(key)
        .bind(value)
        .bind(happyview::db::now_rfc3339())
        .execute(pool)
        .await
        .expect("set_setting failed");
}
