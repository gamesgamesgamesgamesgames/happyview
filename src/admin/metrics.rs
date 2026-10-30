use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::db::adapt_sql;
use crate::error::AppError;
use crate::metrics::{Step, Window};

use super::auth::UserAuth;
use super::permissions::Permission;

#[derive(Deserialize)]
pub struct WindowQuery {
    pub window: Option<String>,
    pub step: Option<String>,
}

fn window_of(query: &WindowQuery) -> Result<Window, AppError> {
    match query.window.as_deref() {
        None => Ok(Window::Hour),
        Some(raw) => Window::parse(raw).ok_or_else(|| {
            AppError::BadRequest(format!(
                "window '{raw}' is not kept: metrics are held in memory for the last hour (1h) and the last day (24h) only"
            ))
        }),
    }
}

fn step_of(query: &WindowQuery, window: Window) -> Result<Step, AppError> {
    let step = match query.step.as_deref() {
        None => Step::default_for(window),
        Some(raw) => Step::parse(raw).ok_or_else(|| {
            AppError::BadRequest(format!("step '{raw}' is not one of 1m and 30m"))
        })?,
    };
    if window == Window::Day && step == Step::Minute {
        return Err(AppError::BadRequest(
            "the last day is kept by the half hour: use step=30m with window=24h".into(),
        ));
    }
    Ok(step)
}

/// GET /admin/metrics/scripts — every script that ran inside the window.
pub(super) async fn scripts(
    State(state): State<AppState>,
    auth: UserAuth,
    Query(query): Query<WindowQuery>,
) -> Result<Json<Value>, AppError> {
    auth.require(Permission::MetricsRead).await?;
    let window = window_of(&query)?;
    Ok(Json(json!({
        "window": window.as_str(),
        "enabled": state.metrics.enabled(),
        "overflow": state.metrics.overflow(),
        "scripts": state.metrics.scripts(window),
    })))
}

/// GET /admin/metrics/scripts/{trigger} — one script's series.
pub(super) async fn script(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(trigger): Path<String>,
    Query(query): Query<WindowQuery>,
) -> Result<Json<Value>, AppError> {
    auth.require(Permission::MetricsRead).await?;
    let window = window_of(&query)?;
    let step = step_of(&query, window)?;
    let series = state
        .metrics
        .series(&trigger, window, step)
        .ok_or_else(|| {
            AppError::NotFound(format!(
                "'{trigger}' has not run in the last {}",
                match window {
                    Window::Hour => "hour",
                    Window::Day => "day",
                }
            ))
        })?;
    let mut body = serde_json::to_value(series)
        .map_err(|e| AppError::Internal(format!("failed to encode metrics: {e}")))?;
    body["window"] = json!(window.as_str());
    body["step"] = json!(match step {
        Step::Minute => "1m",
        Step::HalfHour => "30m",
    });
    body["enabled"] = json!(state.metrics.enabled());
    Ok(Json(body))
}

/// GET /admin/metrics/ingest — the record pipeline, by collection and action.
pub(super) async fn ingest(
    State(state): State<AppState>,
    auth: UserAuth,
    Query(query): Query<WindowQuery>,
) -> Result<Json<Value>, AppError> {
    auth.require(Permission::MetricsRead).await?;
    let window = window_of(&query)?;
    Ok(Json(json!({
        "window": window.as_str(),
        "enabled": state.metrics.enabled(),
        "overflow": state.metrics.overflow(),
        // Since boot, not in the window: how often a record hook's lookup
        // missed the project and its batch and read the scripts table.
        "scripts_table_reads": state.scripts.table_reads(),
        "collections": state.metrics.ingest(window),
    })))
}

/// GET /admin/metrics/live — what is running now.
pub(super) async fn live(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<Value>, AppError> {
    auth.require(Permission::MetricsRead).await?;
    let rows: Vec<(String, i64)> = crate::db::query_as(&adapt_sql(
        "SELECT status, COUNT(*) FROM happyview_jobs WHERE status IN ('pending', 'running') GROUP BY status",
        state.db_backend,
    ))
    .fetch_all(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("failed to count jobs: {e}")))?;
    let jobs = |status: &str| {
        rows.iter()
            .find(|(s, _)| s == status)
            .map_or(0, |(_, n)| *n)
    };
    let active: serde_json::Map<String, Value> = state
        .metrics
        .active_runs()
        .into_iter()
        .map(|(kind, n)| (kind.to_string(), json!(n)))
        .collect();
    let runtime = &state.wasm_runtime;
    Ok(Json(json!({
        "enabled": state.metrics.enabled(),
        "active_runs": active,
        "jobs": { "pending": jobs("pending"), "running": jobs("running") },
        "backfill_jobs": state.metrics.live_jobs(),
        "module_cache": {
            "hits": runtime.cache_hits(),
            "misses": runtime.cache_misses(),
            "compiles": runtime.compile_count(),
        },
    })))
}
