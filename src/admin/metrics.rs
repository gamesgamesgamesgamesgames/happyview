use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::db::adapt_sql;
use crate::error::AppError;
use crate::metrics::{Step, Window, persist, unix_now};

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
            AppError::BadRequest(format!("window '{raw}' is not one of 1h, 24h, 7d and 30d"))
        }),
    }
}

fn step_of(query: &WindowQuery, window: Window) -> Result<Step, AppError> {
    let step = match query.step.as_deref() {
        None => Step::default_for(window),
        Some(raw) => Step::parse(raw).ok_or_else(|| {
            AppError::BadRequest(format!("step '{raw}' is not one of 1m, 30m, 1h and 1d"))
        })?,
    };
    if window == Window::Day && step == Step::Minute {
        return Err(AppError::BadRequest(
            "the last day is kept by the half hour: use step=30m with window=24h".into(),
        ));
    }
    if window.is_stored() != step.is_stored() {
        return Err(AppError::BadRequest(if window.is_stored() {
            format!(
                "window={} is read by the hour or the day: use step=1h or step=1d",
                window.as_str()
            )
        } else {
            format!(
                "window={} is read from memory by the minute or the half hour: use step=1m or step=30m",
                window.as_str()
            )
        }));
    }
    Ok(step)
}

fn read_failed(e: sqlx::Error) -> AppError {
    AppError::Internal(format!("failed to read stored metrics: {e}"))
}

/// The fields a stored window adds to its response: how long the table
/// keeps rows, so a window longer than that is read as what exists.
async fn stored_fields(state: &AppState, window: Window, body: &mut Value) {
    if window.is_stored() {
        body["retention_days"] =
            json!(crate::metrics::retention::retention_days(&state.db, state.db_backend).await);
    }
}

/// GET /admin/metrics/scripts — every script that ran inside the window.
pub(super) async fn scripts(
    State(state): State<AppState>,
    auth: UserAuth,
    Query(query): Query<WindowQuery>,
) -> Result<Json<Value>, AppError> {
    auth.require(Permission::MetricsRead).await?;
    let window = window_of(&query)?;
    let scripts = if window.is_stored() {
        persist::scripts(
            &state.metrics,
            &state.db,
            state.db_backend,
            unix_now(),
            window,
        )
        .await
        .map_err(read_failed)?
    } else {
        state.metrics.scripts(window)
    };
    let mut body = json!({
        "window": window.as_str(),
        "enabled": state.metrics.enabled(),
        "overflow": state.metrics.overflow(),
        "scripts": scripts,
    });
    stored_fields(&state, window, &mut body).await;
    Ok(Json(body))
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
    let series = if window.is_stored() {
        persist::series(
            &state.metrics,
            &state.db,
            state.db_backend,
            unix_now(),
            &trigger,
            window,
            step,
        )
        .await
        .map_err(read_failed)?
    } else {
        state.metrics.series(&trigger, window, step)
    };
    let series = series.ok_or_else(|| {
        AppError::NotFound(format!(
            "'{trigger}' has not run in the last {}",
            window.describe()
        ))
    })?;
    let mut body = serde_json::to_value(series)
        .map_err(|e| AppError::Internal(format!("failed to encode metrics: {e}")))?;
    body["window"] = json!(window.as_str());
    body["step"] = json!(step.as_str());
    body["enabled"] = json!(state.metrics.enabled());
    stored_fields(&state, window, &mut body).await;
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
    let collections = if window.is_stored() {
        persist::ingest(
            &state.metrics,
            &state.db,
            state.db_backend,
            unix_now(),
            window,
        )
        .await
        .map_err(read_failed)?
    } else {
        state.metrics.ingest(window)
    };
    let mut body = json!({
        "window": window.as_str(),
        "enabled": state.metrics.enabled(),
        "overflow": state.metrics.overflow(),
        // Since boot, not in the window: how often a record hook's lookup
        // missed the project and its batch and read the scripts table.
        "scripts_table_reads": state.scripts.table_reads(),
        "collections": collections,
    });
    stored_fields(&state, window, &mut body).await;
    Ok(Json(body))
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
