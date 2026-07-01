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

    let limit = query.limit.unwrap_or(50).min(100);
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

    match job.status.as_str() {
        "running" => {
            jobs::db::set_status(&state, &id, "cancelling").await?;
            Ok(Json(serde_json::json!({ "status": "cancelling" })))
        }
        "pending" | "paused" => {
            jobs::db::set_status(&state, &id, "cancelled").await?;
            Ok(Json(serde_json::json!({ "status": "cancelled" })))
        }
        _ => Err(AppError::BadRequest(format!(
            "cannot cancel job with status: {}",
            job.status
        ))),
    }
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
        return Err(AppError::BadRequest(format!(
            "cannot pause job with status: {}",
            job.status
        )));
    }

    jobs::db::set_status(&state, &id, "pausing").await?;
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
        return Err(AppError::BadRequest(format!(
            "cannot resume job with status: {}",
            job.status
        )));
    }

    jobs::db::set_status(&state, &id, "pending").await?;
    Ok(Json(serde_json::json!({ "status": "pending" })))
}
