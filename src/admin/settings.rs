use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use base64::Engine;
use sqlx::AnyPool;
use std::env;

use crate::AppState;
use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::event_log::{
    DEFAULT_PROTECTED_RETENTION_DAYS, DEFAULT_RETENTION_DAYS, EventLog,
    SPACE_ACCESS_RETENTION_SETTING, Severity, log_event, write_event,
};

use super::auth::UserAuth;
use super::permissions::Permission;
use super::types::{SettingEntry, UpsertSettingBody};

const ENV_FALLBACKS: &[(&str, &str)] = &[
    ("app_name", "APP_NAME"),
    (
        "backfill_concurrent_dids_per_pds",
        "BACKFILL_CONCURRENT_DIDS_PER_PDS",
    ),
    ("backfill_concurrent_pds", "BACKFILL_CONCURRENT_PDS"),
    (
        "backfill_concurrent_resolution",
        "BACKFILL_CONCURRENT_RESOLUTION",
    ),
    ("backfill_retention_days", "BACKFILL_RETENTION_DAYS"),
    ("client_uri", "CLIENT_URI"),
    ("event_log_retention_days", "EVENT_LOG_RETENTION_DAYS"),
    ("feature.space_inspector_enabled", "SPACE_INSPECTOR_ENABLED"),
    ("feature.spaces_enabled", "FEATURE_SPACES_ENABLED"),
    ("logo_uri", "LOGO_URI"),
    (
        "space_access_log_retention_days",
        "SPACE_ACCESS_LOG_RETENTION_DAYS",
    ),
    (
        "space_inspector_max_grant_minutes",
        "SPACE_INSPECTOR_MAX_GRANT_MINUTES",
    ),
    ("tos_uri", "TOS_URI"),
    ("policy_uri", "POLICY_URI"),
    ("verbose_event_logging", "VERBOSE_EVENT_LOGGING"),
];

/// The protected event for an audited setting change: turning the space
/// inspector on or off, and changing how long event logs are kept.
///
/// Compares effective values, env fallback included, so a save that rewrites
/// an unchanged value logs nothing.
fn audited_setting_event(
    auth: &UserAuth,
    key: &str,
    before: &Option<String>,
    after: &Option<String>,
) -> Option<EventLog> {
    let event = match key {
        crate::feature_flags::FeatureFlag::SPACE_INSPECTOR => {
            let on = |v: &Option<String>| {
                v.as_deref()
                    .is_some_and(|v| v.trim().eq_ignore_ascii_case("true"))
            };
            match (on(before), on(after)) {
                (false, true) => Some(("space_inspector.enabled", serde_json::json!({}))),
                (true, false) => Some(("space_inspector.disabled", serde_json::json!({}))),
                _ => None,
            }
        }
        "event_log_retention_days" | SPACE_ACCESS_RETENTION_SETTING => {
            // An unset or unparseable value counts as the default the
            // retention sweep uses in its place.
            let effective = |v: &Option<String>| -> u32 {
                if key == SPACE_ACCESS_RETENTION_SETTING {
                    v.as_deref()
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(DEFAULT_PROTECTED_RETENTION_DAYS)
                } else {
                    v.as_deref()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(DEFAULT_RETENTION_DAYS)
                }
            };
            let (from, to) = (effective(before), effective(after));
            (from != to).then(|| {
                (
                    "event_logs.retention_changed",
                    serde_json::json!({ "from": from, "to": to }),
                )
            })
        }
        _ => None,
    };
    event.map(|(event_type, detail)| EventLog {
        event_type: event_type.to_string(),
        severity: Severity::Warn,
        actor_did: Some(auth.did.clone()),
        subject: Some(key.to_string()),
        detail,
    })
}

/// The env var value a key falls back to when it has no database row.
fn env_fallback(key: &str) -> Option<String> {
    ENV_FALLBACKS
        .iter()
        .find(|(setting_key, _)| *setting_key == key)
        .and_then(|(_, env_var)| env::var(env_var).ok())
}

/// Write a setting change and its audit event together, so an audited setting
/// never changes without its protected record.
async fn write_audited<'q>(
    state: &AppState,
    change: sqlx::query::Query<'q, sqlx::Any, sqlx::any::AnyArguments>,
    event: Option<EventLog>,
) -> Result<u64, AppError> {
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| AppError::Internal(format!("failed to begin transaction: {e}")))?;
    let affected = change
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(format!("failed to write setting: {e}")))?
        .rows_affected();
    if affected > 0
        && let Some(event) = &event
    {
        write_event(&mut *tx, event, state.db_backend)
            .await
            .map_err(|e| AppError::Internal(format!("failed to record the setting change: {e}")))?;
    }
    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("failed to commit setting: {e}")))?;
    Ok(affected)
}

/// Resolve a setting value: check the DB first, then fall back to env var.
pub async fn get_setting(pool: &AnyPool, key: &str, backend: DatabaseBackend) -> Option<String> {
    let sql = adapt_sql(
        "SELECT value FROM happyview_instance_settings WHERE key = ?",
        backend,
    );
    let row: Option<(String,)> = crate::db::query_as(&sql)
        .bind(key)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();

    if let Some((value,)) = row {
        return Some(value);
    }

    // Fall back to env var if one is mapped for this key.
    for (setting_key, env_var) in ENV_FALLBACKS {
        if *setting_key == key {
            return env::var(env_var).ok();
        }
    }

    None
}

/// GET /admin/settings — list all settings with their source.
pub(super) async fn list(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<Vec<SettingEntry>>, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let backend = state.db_backend;
    let sql = adapt_sql(
        "SELECT key, value FROM happyview_instance_settings ORDER BY key",
        backend,
    );
    let rows: Vec<(String, String)> = crate::db::query_as(&sql)
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list settings: {e}")))?;

    let db_keys: std::collections::HashSet<String> = rows.iter().map(|(k, _)| k.clone()).collect();

    let mut entries: Vec<SettingEntry> = rows
        .into_iter()
        .map(|(key, value)| SettingEntry {
            key,
            value,
            source: "database".to_string(),
        })
        .collect();

    // Add env-var fallback entries for keys not already present in DB.
    for (setting_key, env_var) in ENV_FALLBACKS {
        if !db_keys.contains(*setting_key)
            && let Ok(value) = env::var(env_var)
        {
            entries.push(SettingEntry {
                key: setting_key.to_string(),
                value,
                source: "env".to_string(),
            });
        }
    }

    Ok(Json(entries))
}

/// PUT /admin/settings/{key} — create or update a setting.
pub(super) async fn upsert(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(key): Path<String>,
    Json(body): Json<UpsertSettingBody>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let backend = state.db_backend;
    let before = get_setting(&state.db, &key, backend).await;
    let now = now_rfc3339();
    let sql = adapt_sql(
        r#"
        INSERT INTO happyview_instance_settings (key, value, updated_at)
        VALUES (?, ?, ?)
        ON CONFLICT (key) DO UPDATE SET value = ?, updated_at = ?
        "#,
        backend,
    );
    let change = crate::db::query(&sql)
        .bind(&key)
        .bind(&body.value)
        .bind(&now)
        .bind(&body.value)
        .bind(&now);
    let event = audited_setting_event(&auth, &key, &before, &Some(body.value.clone()));
    write_audited(&state, change, event).await?;

    log_event(
        &state.db,
        EventLog {
            event_type: "setting.updated".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(key.clone()),
            detail: serde_json::json!({ "value": body.value }),
        },
        state.db_backend,
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

/// DELETE /admin/settings/{key} — delete a setting.
pub(super) async fn delete(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(key): Path<String>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let backend = state.db_backend;
    let before = get_setting(&state.db, &key, backend).await;
    let sql = adapt_sql(
        "DELETE FROM happyview_instance_settings WHERE key = ?",
        backend,
    );
    let change = crate::db::query(&sql).bind(&key);
    let event = audited_setting_event(&auth, &key, &before, &env_fallback(&key));
    if write_audited(&state, change, event).await? == 0 {
        return Err(AppError::NotFound(format!("setting '{key}' not found")));
    }

    log_event(
        &state.db,
        EventLog {
            event_type: "setting.deleted".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(key.clone()),
            detail: serde_json::json!({}),
        },
        state.db_backend,
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

/// GET /admin/settings/db-info — return database connection pool info.
pub(super) async fn db_info(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let server_max: Option<i64> = if state.db_backend == DatabaseBackend::Postgres {
        crate::db::query_as::<(String,)>("SHOW max_connections")
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten()
            .and_then(|(v,)| v.parse().ok())
    } else {
        None
    };

    let main_pool_size = state.db.options().get_max_connections() as i64;
    let backfill_pool_size = state.backfill_db.options().get_max_connections() as i64;

    let pds: u32 = get_setting(&state.db, "backfill_concurrent_pds", state.db_backend)
        .await
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    let dids: u32 = get_setting(
        &state.db,
        "backfill_concurrent_dids_per_pds",
        state.db_backend,
    )
    .await
    .and_then(|v| v.parse().ok())
    .unwrap_or(3);
    let resolution: u32 = get_setting(
        &state.db,
        "backfill_concurrent_resolution",
        state.db_backend,
    )
    .await
    .and_then(|v| v.parse().ok())
    .unwrap_or(100);
    let would_be_pool_size =
        crate::db::compute_backfill_pool_size(state.db_backend, pds, dids, resolution);
    let restart_recommended = would_be_pool_size as i64 > backfill_pool_size;

    Ok(Json(serde_json::json!({
        "backend": match state.db_backend {
            DatabaseBackend::Sqlite => "sqlite",
            DatabaseBackend::Postgres => "postgres",
        },
        "server_max_connections": server_max,
        "main_pool_size": main_pool_size,
        "backfill_pool_size": backfill_pool_size,
        "restart_recommended": restart_recommended,
    })))
}

/// PUT /admin/settings/logo — upload a logo image (max 5MB).
pub(super) async fn upload_logo(
    State(state): State<AppState>,
    auth: UserAuth,
    mut multipart: axum::extract::Multipart,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let field = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(format!("invalid multipart: {e}")))?
        .ok_or_else(|| AppError::BadRequest("no file uploaded".into()))?;

    let content_type = field
        .content_type()
        .unwrap_or("application/octet-stream")
        .to_string();

    if !content_type.starts_with("image/") {
        return Err(AppError::BadRequest("file must be an image".into()));
    }

    let data = field
        .bytes()
        .await
        .map_err(|e| AppError::BadRequest(format!("failed to read upload: {e}")))?;

    if data.len() > 5 * 1024 * 1024 {
        return Err(AppError::BadRequest("logo must be 5MB or smaller".into()));
    }

    let encoded = base64::engine::general_purpose::STANDARD.encode(&data);

    let backend = state.db_backend;
    let now = now_rfc3339();
    let sql = adapt_sql(
        "INSERT INTO happyview_instance_settings (key, value, updated_at) VALUES (?, ?, ?) ON CONFLICT (key) DO UPDATE SET value = ?, updated_at = ?",
        backend,
    );
    for (key, value) in [
        ("logo_data", encoded.as_str()),
        ("logo_content_type", content_type.as_str()),
    ] {
        crate::db::query(&sql)
            .bind(key)
            .bind(value)
            .bind(&now)
            .bind(value)
            .bind(&now)
            .execute(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("failed to store logo: {e}")))?;
    }

    log_event(
        &state.db,
        EventLog {
            event_type: "setting.updated".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some("logo".to_string()),
            detail: serde_json::json!({ "content_type": content_type, "size_bytes": data.len() }),
        },
        state.db_backend,
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

/// DELETE /admin/settings/logo — remove uploaded logo.
pub(super) async fn delete_logo(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let backend = state.db_backend;
    let sql = adapt_sql(
        "DELETE FROM happyview_instance_settings WHERE key IN (?, ?)",
        backend,
    );
    crate::db::query(&sql)
        .bind("logo_data")
        .bind("logo_content_type")
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete logo: {e}")))?;

    log_event(
        &state.db,
        EventLog {
            event_type: "setting.deleted".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some("logo".to_string()),
            detail: serde_json::json!({}),
        },
        state.db_backend,
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

/// GET /settings/logo — serve the uploaded logo (public, no auth).
pub(crate) async fn serve_logo(
    State(state): State<AppState>,
) -> Result<axum::response::Response, AppError> {
    let backend = state.db_backend;
    let sql = adapt_sql(
        "SELECT key, value FROM happyview_instance_settings WHERE key IN (?, ?)",
        backend,
    );
    let rows: Vec<(String, String)> = crate::db::query_as(&sql)
        .bind("logo_data")
        .bind("logo_content_type")
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to load logo: {e}")))?;

    let data = rows.iter().find(|(k, _)| k == "logo_data").map(|(_, v)| v);
    let ct = rows
        .iter()
        .find(|(k, _)| k == "logo_content_type")
        .map(|(_, v)| v.as_str());

    match (data, ct) {
        (Some(encoded), Some(content_type)) => {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|e| AppError::Internal(format!("failed to decode logo: {e}")))?;
            Ok(axum::response::Response::builder()
                .header("content-type", content_type)
                .header("cache-control", "public, max-age=3600")
                .body(axum::body::Body::from(bytes))
                .unwrap())
        }
        _ => Err(AppError::NotFound("no logo uploaded".into())),
    }
}
