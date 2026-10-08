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
    DEFAULT_RETENTION_DAYS, EventLog, SPACE_ACCESS_RETENTION_SETTING, Severity,
    effective_protected_retention, log_event, write_event,
};

const GENERAL_RETENTION_SETTING: &str = "event_log_retention_days";

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
fn audited_setting_events(
    auth: &UserAuth,
    key: &str,
    before: &Option<String>,
    after: &Option<String>,
    other_retention: &Option<String>,
) -> Vec<EventLog> {
    let event = |subject: &str, event_type: &str, detail: serde_json::Value| EventLog {
        event_type: event_type.to_string(),
        severity: Severity::Warn,
        actor_did: Some(auth.did.clone()),
        subject: Some(subject.to_string()),
        detail,
    };
    match key {
        crate::feature_flags::FeatureFlag::SPACE_INSPECTOR => {
            let on = |v: &Option<String>| {
                v.as_deref()
                    .is_some_and(|v| v.trim().eq_ignore_ascii_case("true"))
            };
            match (on(before), on(after)) {
                (false, true) => vec![event(key, "space_inspector.enabled", serde_json::json!({}))],
                (true, false) => vec![event(
                    key,
                    "space_inspector.disabled",
                    serde_json::json!({}),
                )],
                _ => Vec::new(),
            }
        }
        GENERAL_RETENTION_SETTING | SPACE_ACCESS_RETENTION_SETTING => {
            // Effective values, as the retention sweep reads them: the protected
            // retention depends on the general one, so a change to either can
            // change both.
            let (general, protected) = if key == GENERAL_RETENTION_SETTING {
                ((before, after), (other_retention, other_retention))
            } else {
                ((other_retention, other_retention), (before, after))
            };
            let general_days = |v: &Option<String>| -> u32 {
                v.as_deref()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(DEFAULT_RETENTION_DAYS)
            };
            let (general_from, general_to) = (general_days(general.0), general_days(general.1));
            let protected_from =
                effective_protected_retention(general_from, protected.0.as_deref());
            let protected_to = effective_protected_retention(general_to, protected.1.as_deref());

            let mut events = Vec::new();
            for (subject, from, to) in [
                (GENERAL_RETENTION_SETTING, general_from, general_to),
                (SPACE_ACCESS_RETENTION_SETTING, protected_from, protected_to),
            ] {
                if from != to {
                    events.push(event(
                        subject,
                        "event_logs.retention_changed",
                        serde_json::json!({ "from": from, "to": to }),
                    ));
                }
            }
            events
        }
        _ => Vec::new(),
    }
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
/// A setting's value inside a transaction, env fallback included.
async fn read_setting_in(
    tx: &mut sqlx::Transaction<'static, sqlx::Any>,
    backend: DatabaseBackend,
    key: &str,
) -> Result<Option<String>, AppError> {
    let stored: Option<(String,)> = crate::db::query_as(&adapt_sql(
        "SELECT value FROM happyview_instance_settings WHERE key = ?",
        backend,
    ))
    .bind(key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| AppError::Internal(format!("failed to read setting: {e}")))?;
    Ok(stored.map(|(v,)| v).or_else(|| env_fallback(key)))
}

async fn write_audited<'q>(
    state: &AppState,
    auth: &UserAuth,
    key: &str,
    change: sqlx::query::Query<'q, sqlx::Any, sqlx::any::AnyArguments>,
    after: Option<String>,
) -> Result<u64, AppError> {
    let backend = state.db_backend;
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| AppError::Internal(format!("failed to begin transaction: {e}")))?;

    // Serialize writers of this key before reading its current value, so two
    // concurrent changes can't both compare against the same old value and
    // leave the audit trail out of step with what was stored. Postgres takes a
    // per-key advisory lock; SQLite takes its database write lock. The two
    // retention settings share a lock, since each affects the other's value.
    let is_retention = key == GENERAL_RETENTION_SETTING || key == SPACE_ACCESS_RETENTION_SETTING;
    let lock_key = if is_retention { "retention" } else { key };
    let lock_sql = match backend {
        DatabaseBackend::Postgres => "SELECT pg_advisory_xact_lock(hashtext($1))::text",
        DatabaseBackend::Sqlite => "UPDATE happyview_instance_settings SET key = key WHERE key = ?",
    };
    crate::db::query(lock_sql)
        .bind(lock_key)
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(format!("failed to lock setting: {e}")))?;

    let before = read_setting_in(&mut tx, backend, key).await?;
    let other_retention = match key {
        GENERAL_RETENTION_SETTING => {
            read_setting_in(&mut tx, backend, SPACE_ACCESS_RETENTION_SETTING).await?
        }
        SPACE_ACCESS_RETENTION_SETTING => {
            read_setting_in(&mut tx, backend, GENERAL_RETENTION_SETTING).await?
        }
        _ => None,
    };
    let events = audited_setting_events(auth, key, &before, &after, &other_retention);

    let affected = change
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(format!("failed to write setting: {e}")))?
        .rows_affected();
    if affected > 0 {
        for event in &events {
            write_event(&mut *tx, event, state.db_backend)
                .await
                .map_err(|e| {
                    AppError::Internal(format!("failed to record the setting change: {e}"))
                })?;
        }
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
    write_audited(&state, &auth, &key, change, Some(body.value.clone())).await?;

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
    let sql = adapt_sql(
        "DELETE FROM happyview_instance_settings WHERE key = ?",
        backend,
    );
    let change = crate::db::query(&sql).bind(&key);
    if write_audited(&state, &auth, &key, change, env_fallback(&key)).await? == 0 {
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
