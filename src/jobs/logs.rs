use serde::Serialize;
use uuid::Uuid;

use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::error::AppError;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobLog {
    pub id: String,
    pub job_id: String,
    pub level: String,
    pub message: String,
    pub created_at: String,
}

pub async fn insert_log(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    job_id: &str,
    level: &str,
    message: &str,
) -> Result<(), AppError> {
    let id = Uuid::new_v4().to_string();
    let now = now_rfc3339();
    let sql = adapt_sql(
        "INSERT INTO happyview_job_logs (id, job_id, level, message, created_at) VALUES (?, ?, ?, ?, ?)",
        backend,
    );
    crate::db::query(&sql)
        .bind(&id)
        .bind(job_id)
        .bind(level)
        .bind(message)
        .bind(&now)
        .execute(db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to insert job log: {e}")))?;
    Ok(())
}

pub async fn list_logs(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    job_id: &str,
    limit: i64,
    cursor: Option<&str>,
) -> Result<(Vec<JobLog>, Option<String>), AppError> {
    let limit = limit.clamp(1, 500);
    let fetch_limit = limit + 1;

    let rows: Vec<(String, String, String, String, String)> = if let Some(cursor_id) = cursor {
        let sql = adapt_sql(
            "SELECT l.id, l.job_id, l.level, l.message, l.created_at FROM happyview_job_logs l INNER JOIN happyview_job_logs c ON c.id = ? WHERE l.job_id = ? AND (l.created_at > c.created_at OR (l.created_at = c.created_at AND l.id > c.id)) ORDER BY l.created_at ASC, l.id ASC LIMIT ?",
            backend,
        );
        crate::db::query_as(&sql)
            .bind(cursor_id)
            .bind(job_id)
            .bind(fetch_limit)
            .fetch_all(db)
            .await
    } else {
        let sql = adapt_sql(
            "SELECT id, job_id, level, message, created_at FROM happyview_job_logs WHERE job_id = ? ORDER BY created_at ASC, id ASC LIMIT ?",
            backend,
        );
        crate::db::query_as(&sql)
            .bind(job_id)
            .bind(fetch_limit)
            .fetch_all(db)
            .await
    }
    .map_err(|e| AppError::Internal(format!("failed to list job logs: {e}")))?;

    let has_more = rows.len() as i64 > limit;
    let logs: Vec<JobLog> = rows
        .into_iter()
        .take(limit as usize)
        .map(|(id, job_id, level, message, created_at)| JobLog {
            id,
            job_id,
            level,
            message,
            created_at,
        })
        .collect();

    let next_cursor = if has_more {
        logs.last().map(|l| l.id.clone())
    } else {
        None
    };

    Ok((logs, next_cursor))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DatabaseBackend;

    async fn test_pool() -> sqlx::AnyPool {
        sqlx::any::install_default_drivers();
        let pool = sqlx::pool::PoolOptions::<sqlx::Any>::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::db::query(
            "CREATE TABLE happyview_job_logs (
                id TEXT PRIMARY KEY,
                job_id TEXT NOT NULL,
                level TEXT NOT NULL DEFAULT 'info',
                message TEXT NOT NULL,
                created_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    #[tokio::test]
    async fn insert_log_creates_row() {
        let pool = test_pool().await;
        insert_log(
            &pool,
            DatabaseBackend::Sqlite,
            "job-1",
            "info",
            "hello world",
        )
        .await
        .unwrap();

        let row: (String, String, String, String) = crate::db::query_as(
            "SELECT job_id, level, message, created_at FROM happyview_job_logs WHERE job_id = 'job-1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(row.0, "job-1");
        assert_eq!(row.1, "info");
        assert_eq!(row.2, "hello world");
        assert!(!row.3.is_empty());
    }

    #[tokio::test]
    async fn list_logs_returns_chronological_order() {
        let pool = test_pool().await;
        insert_log(&pool, DatabaseBackend::Sqlite, "job-2", "info", "first")
            .await
            .unwrap();
        insert_log(&pool, DatabaseBackend::Sqlite, "job-2", "warn", "second")
            .await
            .unwrap();
        insert_log(&pool, DatabaseBackend::Sqlite, "job-2", "info", "third")
            .await
            .unwrap();

        let (logs, cursor) = list_logs(&pool, DatabaseBackend::Sqlite, "job-2", 10, None)
            .await
            .unwrap();

        assert_eq!(logs.len(), 3);
        assert_eq!(logs[0].message, "first");
        assert_eq!(logs[1].message, "second");
        assert_eq!(logs[1].level, "warn");
        assert_eq!(logs[2].message, "third");
        assert!(cursor.is_none());
    }

    #[tokio::test]
    async fn list_logs_pagination() {
        let pool = test_pool().await;
        for i in 0..5 {
            insert_log(
                &pool,
                DatabaseBackend::Sqlite,
                "job-3",
                "info",
                &format!("msg-{i}"),
            )
            .await
            .unwrap();
        }

        let (page1, cursor1) = list_logs(&pool, DatabaseBackend::Sqlite, "job-3", 2, None)
            .await
            .unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].message, "msg-0");
        assert_eq!(page1[1].message, "msg-1");
        assert!(cursor1.is_some());

        let (page2, cursor2) = list_logs(
            &pool,
            DatabaseBackend::Sqlite,
            "job-3",
            2,
            cursor1.as_deref(),
        )
        .await
        .unwrap();
        assert_eq!(page2.len(), 2);
        assert_eq!(page2[0].message, "msg-2");
        assert!(cursor2.is_some());

        let (page3, _) = list_logs(
            &pool,
            DatabaseBackend::Sqlite,
            "job-3",
            2,
            cursor2.as_deref(),
        )
        .await
        .unwrap();
        assert_eq!(page3.len(), 1);
        assert_eq!(page3[0].message, "msg-4");
    }

    #[tokio::test]
    async fn list_logs_filters_by_job_id() {
        let pool = test_pool().await;
        insert_log(&pool, DatabaseBackend::Sqlite, "job-a", "info", "a-msg")
            .await
            .unwrap();
        insert_log(&pool, DatabaseBackend::Sqlite, "job-b", "info", "b-msg")
            .await
            .unwrap();

        let (logs, _) = list_logs(&pool, DatabaseBackend::Sqlite, "job-a", 10, None)
            .await
            .unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].message, "a-msg");
    }
}
