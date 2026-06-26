//! Integration tests for plugin logging -> event_logs persistence.
//!
//! Requires TEST_DATABASE_URL to be set (see CLAUDE.md "Testing" section).

mod common;

use common::db::{test_backend, test_pool, truncate_all};
use happyview::db::adapt_sql;
use happyview::plugin::host::{LogLevel, log};
use serde_json::Value;
use serial_test::serial;

/// Wait briefly for detached `tokio::spawn` tasks to flush writes.
/// The log() function spawns fire-and-forget tasks; we need to yield
/// until they complete before querying.
async fn flush_spawned_tasks() {
    common::require_db!();
    for _ in 0..20 {
        tokio::task::yield_now().await;
        tokio::time::sleep(tokio::time::Duration::from_millis(25)).await;
    }
}

#[tokio::test]
#[serial]
async fn plugin_log_writes_all_four_levels_to_event_logs() {
    common::require_db!();
    let pool = test_pool().await;
    let backend = test_backend();
    truncate_all(&pool).await;

    log(
        "my-plugin",
        LogLevel::Debug,
        "dbg msg",
        Some(pool.clone()),
        backend,
    );
    log(
        "my-plugin",
        LogLevel::Info,
        "info msg",
        Some(pool.clone()),
        backend,
    );
    log(
        "my-plugin",
        LogLevel::Warn,
        "warn msg",
        Some(pool.clone()),
        backend,
    );
    log(
        "my-plugin",
        LogLevel::Error,
        "err msg",
        Some(pool.clone()),
        backend,
    );

    flush_spawned_tasks().await;

    let sql = adapt_sql(
        "SELECT severity, subject, detail FROM happyview_event_logs WHERE event_type = ? ORDER BY created_at ASC",
        backend,
    );
    let rows: Vec<(String, Option<String>, String)> = sqlx::query_as(&sql)
        .bind("plugin.log")
        .fetch_all(&pool)
        .await
        .expect("failed to query event_logs");

    assert_eq!(
        rows.len(),
        4,
        "expected 4 plugin.log rows, got {}",
        rows.len()
    );

    // Severity mapping: Debug->info, Info->info, Warn->warn, Error->error
    let severities: Vec<&str> = rows.iter().map(|(s, _, _)| s.as_str()).collect();
    assert_eq!(severities, vec!["info", "info", "warn", "error"]);

    // All rows should have subject = plugin id
    for (_, subject, _) in &rows {
        assert_eq!(subject.as_deref(), Some("my-plugin"));
    }

    // detail.level preserves the original level; detail.message carries the message
    let details: Vec<Value> = rows
        .iter()
        .map(|(_, _, d)| serde_json::from_str(d).expect("detail not valid JSON"))
        .collect();

    assert_eq!(details[0]["level"], "debug");
    assert_eq!(details[0]["message"], "dbg msg");
    assert_eq!(details[1]["level"], "info");
    assert_eq!(details[1]["message"], "info msg");
    assert_eq!(details[2]["level"], "warn");
    assert_eq!(details[2]["message"], "warn msg");
    assert_eq!(details[3]["level"], "error");
    assert_eq!(details[3]["message"], "err msg");
}

#[tokio::test]
#[serial]
async fn plugin_log_with_none_db_does_not_write_event_log() {
    common::require_db!();
    let pool = test_pool().await;
    let backend = test_backend();
    truncate_all(&pool).await;

    // db=None: should only emit to tracing, not persist.
    log(
        "silent-plugin",
        LogLevel::Info,
        "should not persist",
        None,
        backend,
    );

    flush_spawned_tasks().await;

    let sql = adapt_sql(
        "SELECT COUNT(*) FROM happyview_event_logs WHERE event_type = ?",
        backend,
    );
    let count: i64 = sqlx::query_scalar(&sql)
        .bind("plugin.log")
        .fetch_one(&pool)
        .await
        .expect("failed to count event_logs");

    assert_eq!(count, 0);
}
