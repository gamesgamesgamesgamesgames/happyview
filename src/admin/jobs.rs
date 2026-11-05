use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;

use crate::AppState;
use crate::error::AppError;
use crate::jobs;

use super::auth::UserAuth;
use super::permissions::Permission;

#[derive(Deserialize)]
pub struct ListJobsQuery {
    pub status: Option<String>,
    pub limit: Option<i64>,
    pub cursor: Option<String>,
}

pub async fn list_jobs(
    State(state): State<AppState>,
    auth: UserAuth,
    Query(query): Query<ListJobsQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::JobsRead).await?;

    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let (jobs_list, cursor) = jobs::db::list_jobs(
        &state,
        query.status.as_deref(),
        limit,
        query.cursor.as_deref(),
    )
    .await?;

    Ok(Json(serde_json::json!({
        "jobs": jobs_list,
        "cursor": cursor,
    })))
}

pub async fn get_job(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::JobsRead).await?;

    let job = jobs::db::get_job(&state, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("job not found".into()))?;

    Ok(Json(serde_json::to_value(job).unwrap()))
}

pub async fn cancel_job(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::JobsManage).await?;

    let job = jobs::db::get_job(&state, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("job not found".into()))?;

    // Each write lands only on the status it was read against: a job that
    // finished meanwhile must not be marked as cancelling.
    let to = match job.status.as_str() {
        "running" => "cancelling",
        "pending" | "paused" => "cancelled",
        _ => {
            return Err(AppError::Conflict(format!(
                "cannot cancel job with status: {}",
                job.status
            )));
        }
    };
    if !jobs::db::change_status(&state, &id, &job.status, to).await? {
        return Err(moved_on());
    }
    Ok(Json(serde_json::json!({ "status": to })))
}

pub async fn pause_job(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::JobsManage).await?;

    let job = jobs::db::get_job(&state, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("job not found".into()))?;

    if job.status != "running" {
        return Err(AppError::Conflict(format!(
            "cannot pause job with status: {}",
            job.status
        )));
    }

    if !jobs::db::change_status(&state, &id, "running", "pausing").await? {
        return Err(moved_on());
    }
    Ok(Json(serde_json::json!({ "status": "pausing" })))
}

pub async fn resume_job(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::JobsManage).await?;

    let job = jobs::db::get_job(&state, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("job not found".into()))?;

    if job.status != "paused" {
        return Err(AppError::Conflict(format!(
            "cannot resume job with status: {}",
            job.status
        )));
    }

    if !jobs::db::change_status(&state, &id, "paused", "pending").await? {
        return Err(moved_on());
    }
    Ok(Json(serde_json::json!({ "status": "pending" })))
}

/// The job's status changed between reading it and acting on it.
fn moved_on() -> AppError {
    AppError::Conflict("the job's status changed; reload and try again".into())
}

#[derive(Deserialize)]
pub struct ListJobLogsQuery {
    pub limit: Option<i64>,
    pub cursor: Option<String>,
}

pub async fn list_job_logs(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
    Query(query): Query<ListJobLogsQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::JobsRead).await?;

    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let (logs, cursor) = jobs::logs::list_logs(
        &state.db,
        state.db_backend,
        &id,
        limit,
        query.cursor.as_deref(),
    )
    .await?;

    Ok(Json(serde_json::json!({
        "logs": logs,
        "cursor": cursor,
    })))
}
