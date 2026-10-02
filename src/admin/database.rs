//! `/admin/database` — database size, free space, and the one-time vacuum.

use axum::Json;
use axum::extract::State;
use serde_json::json;

use crate::AppState;
use crate::db::DatabaseBackend;
use crate::error::AppError;
use crate::maintenance::{disk, vacuum};

use super::auth::UserAuth;
use super::permissions::Permission;

/// `GET /admin/database/status`
pub(super) async fn status(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::SettingsManage).await?;

    // `disk::report` runs several blocking filesystem syscalls (`stat`,
    // `statvfs`) — if the data directory or `TMPDIR` sits on a wedged mount,
    // those can block uninterruptibly. Run it on the blocking pool so a stuck
    // mount can't park a runtime worker thread.
    let db_url = state.config.database_url.clone();
    let report = tokio::task::spawn_blocking(move || disk::report(&db_url))
        .await
        .map_err(|e| AppError::Internal(format!("disk report task panicked: {e}")))?;
    let feasibility = report.as_ref().map(|r| {
        disk::feasibility(
            r.db_bytes + r.wal_bytes,
            r.db_fs_free,
            r.temp_fs_free,
            &r.db_path,
            &r.temp_path,
            r.same_filesystem,
        )
    });
    let status = vacuum::read_status(&state.db, state.db_backend).await?;

    Ok(Json(json!({
        "backend": match state.db_backend {
            DatabaseBackend::Sqlite => "sqlite",
            DatabaseBackend::Postgres => "postgres",
        },
        "disk": report,
        "feasibility": feasibility,
        "vacuum": status,
        "journal_size_limit": state.config.sqlite_journal_size_limit,
    })))
}

/// `POST /admin/database/vacuum/schedule`
///
/// Rejected outright on Postgres: a VACUUM here is inapplicable, and arming
/// `requested_at` anyway would let an admin schedule something that
/// `vacuum::run_if_requested` (SQLite-only) will silently never run, leaving
/// `/database/status` reporting "scheduled" forever with nothing behind it.
/// A clear 400 up front beats that silent no-op.
pub(super) async fn schedule_vacuum(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::SettingsManage).await?;
    if state.db_backend != DatabaseBackend::Sqlite {
        return Err(AppError::BadRequest(
            "vacuum is only applicable to SQLite instances".into(),
        ));
    }
    vacuum::request(&state.db, state.db_backend).await?;
    Ok(Json(json!({ "scheduled": true })))
}

/// `DELETE /admin/database/vacuum/schedule`
pub(super) async fn cancel_vacuum(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::SettingsManage).await?;
    vacuum::cancel_request(&state.db, state.db_backend).await?;
    Ok(Json(json!({ "scheduled": false })))
}
