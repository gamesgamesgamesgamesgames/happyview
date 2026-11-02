use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use base64::Engine;
use sqlx::AnyPool;

use crate::AppState;
use crate::config::vars;
use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::event_log::{
    DEFAULT_RETENTION_DAYS, EventLog, SPACE_ACCESS_RETENTION_SETTING, Severity,
    effective_protected_retention, log_event, write_event,
};
use crate::feature_flags::FeatureFlag;
use crate::lua::limits as script_limits;

use super::auth::UserAuth;
use super::permissions::Permission;
use super::space_access;
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
    ("tos_uri", "TOS_URI"),
    ("policy_uri", "POLICY_URI"),
    ("script_instruction_limit", "SCRIPT_INSTRUCTION_LIMIT"),
    (crate::metrics::ENABLED_KEY, "SCRIPT_METRICS_ENABLED"),
    ("script_wall_clock_seconds", "SCRIPT_WALL_CLOCK_SECONDS"),
    (
        "space_access_log_retention_days",
        "SPACE_ACCESS_LOG_RETENTION_DAYS",
    ),
    (
        "space_inspector_max_grant_minutes",
        "SPACE_INSPECTOR_MAX_GRANT_MINUTES",
    ),
    ("verbose_event_logging", "VERBOSE_EVENT_LOGGING"),
];

const GENERAL_RETENTION_SETTING: &str = "event_log_retention_days";

/// The environment variable a setting falls back to, if it has one.
pub fn env_var_for(key: &str) -> Option<&'static str> {
    ENV_FALLBACKS
        .iter()
        .find(|(setting_key, _)| *setting_key == key)
        .map(|(_, env_var)| *env_var)
}

/// Resolve a setting value: check the DB first, then fall back to env var.
/// A database that cannot be read counts as holding no row.
pub async fn get_setting(pool: &AnyPool, key: &str, backend: DatabaseBackend) -> Option<String> {
    try_get_setting(pool, key, backend).await.ok().flatten()
}

/// Store a setting in `happyview_instance_settings`, replacing any value.
pub async fn put_setting<'e, E>(
    executor: E,
    key: &str,
    value: &str,
    backend: DatabaseBackend,
) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Any>,
{
    let now = now_rfc3339();
    let sql = adapt_sql(
        r#"
        INSERT INTO happyview_instance_settings (key, value, updated_at)
        VALUES (?, ?, ?)
        ON CONFLICT (key) DO UPDATE SET value = ?, updated_at = ?
        "#,
        backend,
    );
    crate::db::query(&sql)
        .bind(key)
        .bind(value)
        .bind(&now)
        .bind(value)
        .bind(&now)
        .execute(executor)
        .await?;
    Ok(())
}

/// [`get_setting`] for a caller that must tell a missing row from a database
/// it could not ask: the script budgets keep their cached values on the
/// second, where the first means the env var or the default.
pub async fn try_get_setting(
    pool: &AnyPool,
    key: &str,
    backend: DatabaseBackend,
) -> Result<Option<String>, sqlx::Error> {
    if let Some(value) = stored_setting(pool, key, backend).await? {
        return Ok(Some(value));
    }

    Ok(env_var_for(key).and_then(vars::var))
}

/// A setting's stored row alone, without the fallback.
pub async fn stored_setting(
    pool: &AnyPool,
    key: &str,
    backend: DatabaseBackend,
) -> Result<Option<String>, sqlx::Error> {
    let sql = adapt_sql(
        "SELECT value FROM happyview_instance_settings WHERE key = ?",
        backend,
    );
    let row: Option<(String,)> = crate::db::query_as(&sql)
        .bind(key)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|(value,)| value))
}

/// The protected events for an audited setting change: turning the space
/// inspector on or off, and changing how long event logs are kept.
///
/// Compares effective values, fallbacks included, so a save that rewrites an
/// unchanged value logs nothing.
fn audited_setting_events(
    actor_did: &str,
    key: &str,
    before: &Option<String>,
    after: &Option<String>,
    other_retention: &Option<String>,
) -> Vec<EventLog> {
    let event = |subject: &str, event_type: &str, detail: serde_json::Value| EventLog {
        event_type: event_type.to_string(),
        severity: Severity::Warn,
        actor_did: Some(actor_did.to_string()),
        subject: Some(subject.to_string()),
        detail,
    };
    match key {
        FeatureFlag::SPACE_INSPECTOR => {
            let on = |v: &Option<String>| v.as_deref().is_some_and(|v| v == "true");
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

/// The value a key falls back to when it has no database row.
fn fallback_value(key: &str) -> Option<String> {
    env_var_for(key).and_then(vars::var)
}

/// A setting's stored row inside a transaction, without the fallback.
async fn read_stored_in(
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
    Ok(stored.map(|(v,)| v))
}

/// A setting's value inside a transaction, fallback included.
async fn read_setting_in(
    tx: &mut sqlx::Transaction<'static, sqlx::Any>,
    backend: DatabaseBackend,
    key: &str,
) -> Result<Option<String>, AppError> {
    Ok(read_stored_in(tx, backend, key)
        .await?
        .or_else(|| fallback_value(key)))
}

/// What [`write_audited`] does to a setting's row.
#[derive(Clone, Copy)]
enum SettingWrite<'a> {
    Put(&'a str),
    Delete,
}

/// Write a setting change and its audit events together, so an audited setting
/// never changes without its protected record. Returns the rows affected.
async fn write_audited(
    db: &AnyPool,
    backend: DatabaseBackend,
    actor_did: &str,
    key: &str,
    write: SettingWrite<'_>,
) -> Result<u64, AppError> {
    let mut tx = db
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

    let (before, after) = if key == FeatureFlag::SPACE_INSPECTOR {
        // The switch as the inspector reads it, which is not always the
        // stored-then-fallback value every other setting has.
        let on = |stored: Option<&str>| {
            Some(space_access::switch_is_on(stored, fallback_value(key).as_deref()).to_string())
        };
        let before = read_stored_in(&mut tx, backend, key).await?;
        let after = match write {
            SettingWrite::Put(value) => Some(value),
            SettingWrite::Delete => None,
        };
        (on(before.as_deref()), on(after))
    } else {
        let before = read_setting_in(&mut tx, backend, key).await?;
        let after = match write {
            SettingWrite::Put(value) => Some(value.to_string()),
            SettingWrite::Delete => fallback_value(key),
        };
        (before, after)
    };
    let other_retention = match key {
        GENERAL_RETENTION_SETTING => {
            read_setting_in(&mut tx, backend, SPACE_ACCESS_RETENTION_SETTING).await?
        }
        SPACE_ACCESS_RETENTION_SETTING => {
            read_setting_in(&mut tx, backend, GENERAL_RETENTION_SETTING).await?
        }
        _ => None,
    };
    let events = audited_setting_events(actor_did, key, &before, &after, &other_retention);

    let affected = match write {
        SettingWrite::Put(value) => put_setting(&mut *tx, key, value, backend).await.map(|()| 1),
        SettingWrite::Delete => crate::db::query(&adapt_sql(
            "DELETE FROM happyview_instance_settings WHERE key = ?",
            backend,
        ))
        .bind(key)
        .execute(&mut *tx)
        .await
        .map(|result| result.rows_affected()),
    }
    .map_err(|e| AppError::Internal(format!("failed to write setting: {e}")))?;
    if affected > 0 {
        for event in &events {
            write_event(&mut *tx, event, backend).await.map_err(|e| {
                AppError::Internal(format!("failed to record the setting change: {e}"))
            })?;
        }
    }
    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("failed to commit setting: {e}")))?;
    Ok(affected)
}

/// The script budgets and the metrics switch are read from a cache rather
/// than the database on every run, so a write to one of them is folded in
/// here instead of waiting for the next poll.
async fn refresh_cached_setting(state: &AppState, key: &str) {
    if script_limits::is_limit_key(key) {
        state
            .script_limits
            .refresh(&state.db, state.db_backend)
            .await;
    }
    if key == crate::metrics::ENABLED_KEY {
        state.metrics.refresh(&state.db, state.db_backend).await;
    }
}

/// Fallback values for settings with no stored row, labelled with where
/// they came from so the dashboard can say which file or variable to edit.
fn fallback_entries(db_keys: &std::collections::HashSet<String>) -> Vec<SettingEntry> {
    ENV_FALLBACKS
        .iter()
        .filter(|(setting_key, _)| !db_keys.contains(*setting_key))
        .filter_map(|(setting_key, env_var)| {
            vars::var_with_source(env_var).map(|(value, source)| SettingEntry {
                key: setting_key.to_string(),
                value,
                source: source.as_str().to_string(),
            })
        })
        .collect()
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

    entries.extend(fallback_entries(&db_keys));

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
    if key.starts_with("telemetry.") {
        super::telemetry::refuse_if_managed(&state)?;
    }
    script_limits::validate(&key, &body.value).map_err(AppError::BadRequest)?;
    crate::metrics::validate(&key, &body.value).map_err(AppError::BadRequest)?;
    space_access::refuse_switch_on(&key, &body.value)?;

    write_audited(
        &state.db,
        state.db_backend,
        &auth.did,
        &key,
        SettingWrite::Put(&body.value),
    )
    .await?;
    refresh_cached_setting(&state, &key).await;

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
    if key.starts_with("telemetry.") {
        super::telemetry::refuse_if_managed(&state)?;
    }

    let affected = write_audited(
        &state.db,
        state.db_backend,
        &auth.did,
        &key,
        SettingWrite::Delete,
    )
    .await?;
    if affected == 0 {
        return Err(AppError::NotFound(format!("setting '{key}' not found")));
    }
    refresh_cached_setting(&state, &key).await;

    log_event(
        &state.db,
        EventLog {
            event_type: "setting.deleted".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(key),
            detail: serde_json::json!({}),
        },
        state.db_backend,
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

/// The crawl's concurrency as a backfill job will run it: each value from its
/// instance setting, then its environment variable, then its default, the
/// order the job reads them in. The backfill pool is sized from this at boot,
/// so a setting raised from the dashboard takes effect at the next restart.
/// Returns `(pds, dids_per_pds, resolution)`.
pub async fn backfill_concurrency(pool: &AnyPool, backend: DatabaseBackend) -> (u32, u32, u32) {
    let read = |key: &'static str, default: u32| async move {
        get_setting(pool, key, backend)
            .await
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };
    (
        read("backfill_concurrent_pds", 10).await,
        read("backfill_concurrent_dids_per_pds", 3).await,
        read("backfill_concurrent_resolution", 100).await,
    )
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

    let (pds, dids, resolution) = backfill_concurrency(&state.db, state.db_backend).await;
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use serial_test::serial;

    use super::*;
    use crate::config::vars::{LayerReset, set_project_layer};
    use crate::test_support::migrated_memory_pool;

    fn file_says_app_name(value: &str) -> LayerReset {
        set_project_layer([("APP_NAME".to_string(), value.to_string())].into());
        LayerReset
    }

    /// Restores `APP_NAME` to unset even when the test panics.
    struct AppNameEnv;

    impl AppNameEnv {
        fn set(value: Option<&str>) -> Self {
            match value {
                Some(v) => unsafe { std::env::set_var("APP_NAME", v) },
                None => unsafe { std::env::remove_var("APP_NAME") },
            }
            Self
        }
    }

    impl Drop for AppNameEnv {
        fn drop(&mut self) {
            unsafe { std::env::remove_var("APP_NAME") };
        }
    }

    #[tokio::test]
    #[serial]
    async fn the_file_answers_when_neither_the_database_nor_the_env_does() {
        let _env = AppNameEnv::set(None);
        let _reset = file_says_app_name("From The File");
        let pool = migrated_memory_pool().await;
        assert_eq!(
            get_setting(&pool, "app_name", DatabaseBackend::Sqlite)
                .await
                .as_deref(),
            Some("From The File")
        );
    }

    #[tokio::test]
    #[serial]
    async fn the_env_wins_over_the_file() {
        let _env = AppNameEnv::set(Some("From Env"));
        let _reset = file_says_app_name("From The File");
        let pool = migrated_memory_pool().await;
        let value = get_setting(&pool, "app_name", DatabaseBackend::Sqlite).await;
        assert_eq!(value.as_deref(), Some("From Env"));
    }

    #[tokio::test]
    #[serial]
    async fn a_stored_setting_wins_over_the_file() {
        let _env = AppNameEnv::set(None);
        let _reset = file_says_app_name("From The File");
        let pool = migrated_memory_pool().await;
        crate::db::query(
            "INSERT INTO happyview_instance_settings (key, value, updated_at) VALUES (?, ?, ?)",
        )
        .bind("app_name")
        .bind("From The Dashboard")
        .bind("2026-01-01T00:00:00+00:00")
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            get_setting(&pool, "app_name", DatabaseBackend::Sqlite)
                .await
                .as_deref(),
            Some("From The Dashboard")
        );
    }

    async fn retention_changes(pool: &AnyPool) -> Vec<(String, serde_json::Value)> {
        let rows: Vec<(String, String)> = crate::db::query_as(
            "SELECT subject, detail FROM happyview_event_logs \
             WHERE event_type = 'event_logs.retention_changed' ORDER BY created_at",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        rows.into_iter()
            .map(|(subject, detail)| (subject, serde_json::from_str(&detail).unwrap()))
            .collect()
    }

    async fn put_audited(pool: &AnyPool, key: &str, value: &str) {
        write_audited(
            pool,
            DatabaseBackend::Sqlite,
            "did:plc:admin",
            key,
            SettingWrite::Put(value),
        )
        .await
        .unwrap();
    }

    /// A value `happyview.toml` supplies is the one a change is compared
    /// against: saving it again changes nothing, and changing it logs the
    /// file's value as where it came from.
    #[tokio::test]
    #[serial]
    async fn a_retention_from_the_file_is_the_value_before_a_change() {
        unsafe { std::env::remove_var("EVENT_LOG_RETENTION_DAYS") };
        set_project_layer([("EVENT_LOG_RETENTION_DAYS".to_string(), "14".to_string())].into());
        let _reset = LayerReset;
        let pool = migrated_memory_pool().await;

        put_audited(&pool, GENERAL_RETENTION_SETTING, "14").await;
        assert!(retention_changes(&pool).await.is_empty());

        put_audited(&pool, GENERAL_RETENTION_SETTING, "7").await;
        assert_eq!(
            retention_changes(&pool).await,
            vec![(
                GENERAL_RETENTION_SETTING.to_string(),
                serde_json::json!({ "from": 14, "to": 7 })
            )]
        );

        // Deleting the row falls back to the file's value, which is a change
        // back to it.
        let affected = write_audited(
            &pool,
            DatabaseBackend::Sqlite,
            "did:plc:admin",
            GENERAL_RETENTION_SETTING,
            SettingWrite::Delete,
        )
        .await
        .unwrap();
        assert_eq!(affected, 1);
        let changes = retention_changes(&pool).await;
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[1].1, serde_json::json!({ "from": 7, "to": 14 }));
    }

    #[tokio::test]
    #[serial]
    async fn only_a_change_to_an_effective_retention_is_logged() {
        unsafe { std::env::remove_var("EVENT_LOG_RETENTION_DAYS") };
        unsafe { std::env::remove_var("SPACE_ACCESS_LOG_RETENTION_DAYS") };
        let pool = migrated_memory_pool().await;

        // Both settings already hold their defaults while unset.
        put_audited(&pool, SPACE_ACCESS_RETENTION_SETTING, "365").await;
        put_audited(&pool, GENERAL_RETENTION_SETTING, "30").await;
        assert!(retention_changes(&pool).await.is_empty());

        put_audited(&pool, GENERAL_RETENTION_SETTING, "14").await;
        put_audited(&pool, GENERAL_RETENTION_SETTING, "14").await;
        assert_eq!(
            retention_changes(&pool).await,
            vec![(
                GENERAL_RETENTION_SETTING.to_string(),
                serde_json::json!({ "from": 30, "to": 14 })
            )]
        );
    }

    /// With protected retention unset, keeping every event keeps protected
    /// ones too, so both effective values change.
    #[tokio::test]
    #[serial]
    async fn a_general_retention_of_zero_changes_the_protected_one_too() {
        unsafe { std::env::remove_var("EVENT_LOG_RETENTION_DAYS") };
        unsafe { std::env::remove_var("SPACE_ACCESS_LOG_RETENTION_DAYS") };
        let pool = migrated_memory_pool().await;

        put_audited(&pool, GENERAL_RETENTION_SETTING, "0").await;
        let changes = retention_changes(&pool).await;
        assert_eq!(changes.len(), 2, "{changes:?}");
        assert!(changes.contains(&(
            GENERAL_RETENTION_SETTING.to_string(),
            serde_json::json!({ "from": 30, "to": 0 })
        )));
        assert!(changes.contains(&(
            SPACE_ACCESS_RETENTION_SETTING.to_string(),
            serde_json::json!({ "from": 365, "to": 0 })
        )));

        // Setting protected retention explicitly ends the follow.
        put_audited(&pool, SPACE_ACCESS_RETENTION_SETTING, "365").await;
        let changes = retention_changes(&pool).await;
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[2].1, serde_json::json!({ "from": 0, "to": 365 }));
    }

    async fn inspector_events(pool: &AnyPool) -> Vec<String> {
        let rows: Vec<(String,)> = crate::db::query_as(
            "SELECT event_type FROM happyview_event_logs \
             WHERE event_type LIKE 'space_inspector.%' ORDER BY created_at",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        rows.into_iter().map(|(t,)| t).collect()
    }

    /// Turning the inspector on or off is a protected event, judged by the
    /// switch as the inspector reads it. `upsert` refuses `true` until the
    /// dashboard ships, so this drives the write beneath it.
    #[tokio::test]
    #[serial]
    async fn the_inspector_switch_logs_each_effective_change() {
        unsafe { std::env::remove_var("SPACE_INSPECTOR_ENABLED") };
        let pool = migrated_memory_pool().await;
        let key = FeatureFlag::SPACE_INSPECTOR;

        put_audited(&pool, key, "false").await;
        assert!(inspector_events(&pool).await.is_empty());
        put_audited(&pool, key, " true ").await;
        put_audited(&pool, key, "true").await;
        put_audited(&pool, key, "false").await;
        put_audited(&pool, key, "true").await;
        write_audited(
            &pool,
            DatabaseBackend::Sqlite,
            "did:plc:admin",
            key,
            SettingWrite::Delete,
        )
        .await
        .unwrap();
        assert_eq!(
            inspector_events(&pool).await,
            [
                "space_inspector.enabled",
                "space_inspector.disabled",
                "space_inspector.enabled",
                "space_inspector.disabled",
            ]
        );
    }

    /// Configuration that says `true` leaves the inspector off in this build,
    /// so storing `false` over it is no change and logs nothing.
    #[tokio::test]
    #[serial]
    async fn a_configured_inspector_switch_is_ignored() {
        unsafe { std::env::remove_var("SPACE_INSPECTOR_ENABLED") };
        set_project_layer([("SPACE_INSPECTOR_ENABLED".to_string(), "true".to_string())].into());
        let _reset = LayerReset;
        let pool = migrated_memory_pool().await;

        assert_eq!(
            get_setting(&pool, FeatureFlag::SPACE_INSPECTOR, DatabaseBackend::Sqlite)
                .await
                .as_deref(),
            Some("true"),
            "the value is configured"
        );
        assert!(
            !space_access::inspector_enabled(&pool, DatabaseBackend::Sqlite).await,
            "and ignored"
        );
        put_audited(&pool, FeatureFlag::SPACE_INSPECTOR, "false").await;
        assert!(inspector_events(&pool).await.is_empty());
    }

    #[test]
    #[serial]
    fn the_listing_names_the_file_as_the_source() {
        let _env = AppNameEnv::set(None);
        let _reset = file_says_app_name("From The File");
        let entries = fallback_entries(&HashSet::new());
        let app_name = entries.iter().find(|e| e.key == "app_name").unwrap();
        assert_eq!(app_name.value, "From The File");
        assert_eq!(app_name.source, "project");

        let stored: HashSet<String> = ["app_name".to_string()].into();
        assert!(
            fallback_entries(&stored)
                .iter()
                .all(|e| e.key != "app_name")
        );
    }
}
