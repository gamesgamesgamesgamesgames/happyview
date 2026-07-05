use serde_json::Value;
use uuid::Uuid;

use crate::AppState;
use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::error::AppError;

use super::Job;

type JobRow = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    String,
    bool,
);

fn row_to_job(
    (
        id,
        job_type,
        status,
        input,
        progress,
        result,
        error,
        created_by,
        started_at,
        completed_at,
        created_at,
        inherit_auth,
    ): JobRow,
) -> Job {
    Job {
        id,
        job_type,
        status,
        input: serde_json::from_str(&input).unwrap_or(Value::Null),
        progress: serde_json::from_str(&progress).unwrap_or(Value::Null),
        result: result.and_then(|r| serde_json::from_str(&r).ok()),
        error,
        created_by,
        started_at,
        completed_at,
        created_at,
        inherit_auth,
    }
}

pub async fn create_job(
    state: &AppState,
    job_type: &str,
    input: &Value,
    created_by: &str,
    inherit_auth: bool,
) -> Result<String, AppError> {
    let id = Uuid::new_v4().to_string();
    let now = now_rfc3339();
    let input_str = serde_json::to_string(input)
        .map_err(|e| AppError::Internal(format!("failed to serialize job input: {e}")))?;

    let sql = adapt_sql(
        "INSERT INTO happyview_jobs (id, job_type, status, input, created_by, created_at, inherit_auth) VALUES (?, ?, 'pending', ?, ?, ?, ?)",
        state.db_backend,
    );
    sqlx::query(&sql)
        .bind(&id)
        .bind(job_type)
        .bind(&input_str)
        .bind(created_by)
        .bind(&now)
        .bind(inherit_auth)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create job: {e}")))?;

    Ok(id)
}

pub async fn get_job(state: &AppState, id: &str) -> Result<Option<Job>, AppError> {
    let sql = adapt_sql(
        "SELECT * FROM happyview_jobs WHERE id = ?",
        state.db_backend,
    );
    let row: Option<JobRow> = sqlx::query_as(&sql)
        .bind(id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to fetch job: {e}")))?;

    Ok(row.map(row_to_job))
}

pub async fn list_jobs(
    state: &AppState,
    status_filter: Option<&str>,
    limit: i64,
    cursor: Option<&str>,
) -> Result<(Vec<Job>, Option<String>), AppError> {
    let sql = if status_filter.is_some() {
        let base = if cursor.is_some() {
            "SELECT * FROM happyview_jobs WHERE status = ? AND created_at < ? ORDER BY created_at DESC LIMIT ?"
        } else {
            "SELECT * FROM happyview_jobs WHERE status = ? ORDER BY created_at DESC LIMIT ?"
        };
        adapt_sql(base, state.db_backend)
    } else {
        let base = if cursor.is_some() {
            "SELECT * FROM happyview_jobs WHERE created_at < ? ORDER BY created_at DESC LIMIT ?"
        } else {
            "SELECT * FROM happyview_jobs ORDER BY created_at DESC LIMIT ?"
        };
        adapt_sql(base, state.db_backend)
    };

    let mut query = sqlx::query_as::<_, JobRow>(&sql);

    if let Some(status) = status_filter {
        query = query.bind(status);
    }
    if let Some(cursor) = cursor {
        query = query.bind(cursor);
    }
    query = query.bind(limit + 1);

    let rows = query
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list jobs: {e}")))?;

    let has_more = rows.len() as i64 > limit;
    let jobs: Vec<Job> = rows
        .into_iter()
        .take(limit as usize)
        .map(row_to_job)
        .collect();

    let next_cursor = if has_more {
        jobs.last().map(|j| j.created_at.clone())
    } else {
        None
    };

    Ok((jobs, next_cursor))
}

pub async fn set_status(state: &AppState, id: &str, status: &str) -> Result<(), AppError> {
    let now = now_rfc3339();
    let sql = match status {
        "running" => adapt_sql(
            "UPDATE happyview_jobs SET status = ?, started_at = ? WHERE id = ?",
            state.db_backend,
        ),
        "completed" | "failed" | "cancelled" => adapt_sql(
            "UPDATE happyview_jobs SET status = ?, completed_at = ? WHERE id = ?",
            state.db_backend,
        ),
        _ => adapt_sql(
            "UPDATE happyview_jobs SET status = ? WHERE id = ? AND 1=1",
            state.db_backend,
        ),
    };

    match status {
        "running" | "completed" | "failed" | "cancelled" => {
            sqlx::query(&sql)
                .bind(status)
                .bind(&now)
                .bind(id)
                .execute(&state.db)
                .await
                .map_err(|e| AppError::Internal(format!("failed to update job status: {e}")))?;
        }
        _ => {
            let sql = adapt_sql(
                "UPDATE happyview_jobs SET status = ? WHERE id = ?",
                state.db_backend,
            );
            sqlx::query(&sql)
                .bind(status)
                .bind(id)
                .execute(&state.db)
                .await
                .map_err(|e| AppError::Internal(format!("failed to update job status: {e}")))?;
        }
    }

    Ok(())
}

pub async fn update_progress(state: &AppState, id: &str, progress: &Value) -> Result<(), AppError> {
    let progress_str = serde_json::to_string(progress)
        .map_err(|e| AppError::Internal(format!("failed to serialize progress: {e}")))?;
    let sql = adapt_sql(
        "UPDATE happyview_jobs SET progress = ? WHERE id = ?",
        state.db_backend,
    );
    sqlx::query(&sql)
        .bind(&progress_str)
        .bind(id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to update job progress: {e}")))?;
    Ok(())
}

pub async fn set_result(state: &AppState, id: &str, result: &Value) -> Result<(), AppError> {
    let result_str = serde_json::to_string(result)
        .map_err(|e| AppError::Internal(format!("failed to serialize result: {e}")))?;
    let now = now_rfc3339();
    let sql = adapt_sql(
        "UPDATE happyview_jobs SET status = 'completed', result = ?, completed_at = ? WHERE id = ?",
        state.db_backend,
    );
    sqlx::query(&sql)
        .bind(&result_str)
        .bind(&now)
        .bind(id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to set job result: {e}")))?;
    Ok(())
}

pub async fn set_error(state: &AppState, id: &str, error: &str) -> Result<(), AppError> {
    let now = now_rfc3339();
    let sql = adapt_sql(
        "UPDATE happyview_jobs SET status = 'failed', error = ?, completed_at = ? WHERE id = ?",
        state.db_backend,
    );
    sqlx::query(&sql)
        .bind(error)
        .bind(&now)
        .bind(id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to set job error: {e}")))?;
    Ok(())
}

/// Check if a job should stop (status changed to cancelling or pausing).
/// Same cooperative cancellation pattern as the backfill system.
pub async fn should_stop(state: &AppState, id: &str) -> Option<&'static str> {
    let sql = adapt_sql(
        "SELECT status FROM happyview_jobs WHERE id = ?",
        state.db_backend,
    );
    let status = sqlx::query_as::<_, (String,)>(&sql)
        .bind(id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .map(|(s,)| s);
    match status.as_deref() {
        Some("cancelling") => Some("cancelling"),
        Some("pausing") => Some("pausing"),
        _ => None,
    }
}

/// Find jobs that were interrupted by a server restart.
pub async fn find_interrupted_jobs(state: &AppState) -> Vec<Job> {
    let sql = adapt_sql(
        "SELECT * FROM happyview_jobs WHERE status IN ('running', 'cancelling', 'pausing')",
        state.db_backend,
    );
    let rows: Vec<JobRow> = sqlx::query_as(&sql)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();

    rows.into_iter().map(row_to_job).collect()
}

/// Pick the next pending job and atomically set it to running.
pub async fn claim_next_job(state: &AppState) -> Result<Option<Job>, AppError> {
    let now = now_rfc3339();

    let sql = match state.db_backend {
        DatabaseBackend::Postgres => adapt_sql(
            "UPDATE happyview_jobs SET status = 'running', started_at = ? WHERE id = (SELECT id FROM happyview_jobs WHERE status = 'pending' ORDER BY created_at ASC LIMIT 1 FOR UPDATE SKIP LOCKED) RETURNING *",
            state.db_backend,
        ),
        DatabaseBackend::Sqlite => adapt_sql(
            "UPDATE happyview_jobs SET status = 'running', started_at = ? WHERE id = (SELECT id FROM happyview_jobs WHERE status = 'pending' ORDER BY created_at ASC LIMIT 1) AND status = 'pending' RETURNING *",
            state.db_backend,
        ),
    };

    let row: Option<JobRow> = sqlx::query_as(&sql)
        .bind(&now)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to claim job: {e}")))?;

    Ok(row.map(row_to_job))
}
