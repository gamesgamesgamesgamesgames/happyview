use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};
use crate::proxy_config::{ProxyConfig, ProxyMode, ProxyRouting, validate_nsid_pattern};

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

/// The PUT body.
///
/// `routing` is optional and **absent means unchanged**, not "reset to the
/// default". The dashboard's proxy-mode form does not send it, so deserialising
/// straight into [`ProxyConfig`] — where the field carries a serde default —
/// would silently revert an operator's routing choice every time they toggled a
/// mode or edited an NSID.
#[derive(serde::Deserialize)]
pub(super) struct ProxyConfigUpdate {
    mode: ProxyMode,
    #[serde(default)]
    nsids: Vec<String>,
    #[serde(default)]
    routing: Option<ProxyRouting>,
}

/// PUT /admin/settings/xrpc-proxy
pub(super) async fn put(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(update): Json<ProxyConfigUpdate>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let stored = (**state.proxy_config.load()).clone();

    let mut config = ProxyConfig {
        mode: update.mode,
        nsids: update.nsids,
        routing: update.routing.unwrap_or(stored.routing),
    };

    // Clear nsids for modes that don't use them
    if matches!(config.mode, ProxyMode::Disabled | ProxyMode::Open) {
        config.nsids.clear();
    }

    // Validate only patterns that aren't already stored. A pattern accepted
    // by an older, laxer validator and still sitting in the config is
    // grandfathered — revalidating it on every PUT would 400 an operator who
    // never touched it, just for toggling proxy mode. A newly added pattern
    // gets the full check.
    for pattern in &config.nsids {
        if !stored.nsids.contains(pattern) {
            validate_nsid_pattern(pattern).map_err(AppError::BadRequest)?;
        }
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
    crate::db::query(&sql)
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
