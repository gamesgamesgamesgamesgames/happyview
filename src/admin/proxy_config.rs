use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};
use crate::proxy_config::{ProxyConfig, ProxyMode, validate_nsid_pattern};

use super::auth::UserAuth;
use super::permissions::Permission;

const SETTING_KEY: &str = "xrpc_proxy_config";

/// GET /admin/settings/xrpc-proxy
pub(super) async fn get(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<ProxyConfig>, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let config = (**state.proxy_config.load()).clone();
    Ok(Json(config))
}

/// PUT /admin/settings/xrpc-proxy
pub(super) async fn put(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(mut config): Json<ProxyConfig>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::SettingsManage).await?;

    // Clear nsids for modes that don't use them
    if matches!(config.mode, ProxyMode::Disabled | ProxyMode::Open) {
        config.nsids.clear();
    }

    // Validate NSID patterns
    for pattern in &config.nsids {
        validate_nsid_pattern(pattern).map_err(AppError::BadRequest)?;
    }

    let json = serde_json::to_string(&config)
        .map_err(|e| AppError::Internal(format!("failed to serialize proxy config: {e}")))?;

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
    sqlx::query(&sql)
        .bind(SETTING_KEY)
        .bind(&json)
        .bind(&now)
        .bind(&json)
        .bind(&now)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to save proxy config: {e}")))?;

    // Update in-memory cache
    state.proxy_config.store(std::sync::Arc::new(config));

    log_event(
        &state.db,
        EventLog {
            event_type: "setting.updated".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(SETTING_KEY.to_string()),
            detail: serde_json::json!({ "value": json }),
        },
        state.db_backend,
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}
