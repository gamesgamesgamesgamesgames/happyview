use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use std::collections::HashMap;

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};
use crate::plugin::encryption::{decrypt, encrypt};
use crate::plugin::loader;
use crate::plugin::official_registry::{OfficialPlugin, ReleaseEntry};

/// If the reload request provides a new URL, use it and clear the old sha256
/// (the new version has its own hash). Otherwise keep the stored values.
fn resolve_reload_url(
    current: (String, Option<String>),
    body: Option<super::types::ReloadPluginBody>,
) -> (String, Option<String>) {
    match body {
        Some(b) if b.url.is_some() => (b.url.unwrap(), None),
        _ => current,
    }
}

struct UpdateInfo {
    update_available: bool,
    latest_version: Option<String>,
    pending_releases: Vec<ReleaseEntry>,
}

fn compute_update_info(
    installed_version: &str,
    cache_entry: Option<&OfficialPlugin>,
) -> UpdateInfo {
    let Some(entry) = cache_entry else {
        return UpdateInfo {
            update_available: false,
            latest_version: None,
            pending_releases: Vec::new(),
        };
    };

    let installed = match semver::Version::parse(installed_version) {
        Ok(v) => v,
        Err(_) => {
            return UpdateInfo {
                update_available: false,
                latest_version: Some(entry.latest_version.clone()),
                pending_releases: Vec::new(),
            };
        }
    };

    let pending: Vec<ReleaseEntry> = entry
        .releases
        .iter()
        .filter(|r| {
            semver::Version::parse(&r.version)
                .map(|v| v > installed)
                .unwrap_or(false)
        })
        .cloned()
        .collect();

    UpdateInfo {
        update_available: !pending.is_empty(),
        latest_version: Some(entry.latest_version.clone()),
        pending_releases: pending,
    }
}

use super::auth::UserAuth;
use super::permissions::Permission;
use super::types::{
    AddPluginBody, PluginPreviewResponse, PluginSecretsResponse, PluginSummary,
    PluginsListResponse, PreviewPluginBody, UpdatePluginSecretsBody,
};

/// GET /admin/plugins - list all loaded plugins
pub(super) async fn list(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<PluginsListResponse>, AppError> {
    auth.require(Permission::PluginsRead).await?;

    let plugins = state.plugin_registry.list().await;

    // Query which plugins have secrets configured
    let configured_plugins: std::collections::HashSet<String> = {
        let sql = adapt_sql(
            "SELECT plugin_id FROM happyview_plugin_configs WHERE config IS NOT NULL",
            state.db_backend,
        );
        sqlx::query_scalar::<_, String>(&sql)
            .fetch_all(&state.db)
            .await
            .unwrap_or_default()
            .into_iter()
            .collect()
    };

    let official_guard = state.official_registry.read().await;

    let summaries: Vec<PluginSummary> = plugins
        .into_iter()
        .map(|p| {
            let (source, url, sha256) = match &p.source {
                crate::plugin::PluginSource::File { path } => {
                    ("file".to_string(), Some(path.display().to_string()), None)
                }
                crate::plugin::PluginSource::Url { url, sha256 } => {
                    ("url".to_string(), Some(url.clone()), sha256.clone())
                }
            };

            // Use manifest for rich secret metadata if available, otherwise fallback to basic keys
            let required_secrets: Vec<super::types::SecretDefinition> =
                if let Some(manifest) = &p.manifest {
                    manifest
                        .required_secrets
                        .iter()
                        .map(|s| super::types::SecretDefinition {
                            key: s.key.clone(),
                            name: s.name.clone(),
                            description: s.description.clone(),
                        })
                        .collect()
                } else {
                    // Legacy plugins without manifest - create minimal SecretDefinition from keys
                    p.info
                        .required_secrets
                        .iter()
                        .map(|key| super::types::SecretDefinition {
                            key: key.clone(),
                            name: key.clone(), // Use key as name for legacy
                            description: None,
                        })
                        .collect()
                };

            // Plugin is configured if it has no required secrets OR has a config entry
            let secrets_configured =
                required_secrets.is_empty() || configured_plugins.contains(&p.info.id);

            let update_info =
                compute_update_info(&p.info.version, official_guard.plugins.get(&p.info.id));

            PluginSummary {
                id: p.info.id.clone(),
                name: p.info.name.clone(),
                version: p.info.version.clone(),
                source,
                url,
                sha256,
                enabled: true, // Currently all loaded plugins are enabled
                auth_type: p.info.auth_type.clone(),
                required_secrets,
                secrets_configured,
                loaded_at: None, // Would need to track this in registry
                update_available: update_info.update_available,
                latest_version: update_info.latest_version,
                pending_releases: update_info.pending_releases,
            }
        })
        .collect();

    Ok(Json(PluginsListResponse {
        plugins: summaries,
        encryption_configured: state.config.token_encryption_key.is_some(),
    }))
}

/// POST /admin/plugins/preview - preview a plugin from URL (fetches manifest only)
pub(super) async fn preview(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<PreviewPluginBody>,
) -> Result<Json<PluginPreviewResponse>, AppError> {
    auth.require(Permission::PluginsCreate).await?;

    let preview = loader::fetch_manifest(&state.http, &body.url)
        .await
        .map_err(|e| AppError::BadRequest(format!("Failed to fetch manifest: {}", e)))?;

    Ok(Json(PluginPreviewResponse {
        id: preview.manifest.id,
        name: preview.manifest.name,
        version: preview.manifest.version,
        description: preview.manifest.description,
        icon_url: preview.manifest.icon_url,
        auth_type: preview.manifest.auth_type,
        required_secrets: preview
            .manifest
            .required_secrets
            .into_iter()
            .map(|s| super::types::SecretDefinition {
                key: s.key,
                name: s.name,
                description: s.description,
            })
            .collect(),
        manifest_url: preview.manifest_url,
        wasm_url: preview.wasm_url,
    }))
}

/// POST /admin/plugins - add a new plugin from URL
pub(super) async fn add(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<AddPluginBody>,
) -> Result<Json<PluginSummary>, AppError> {
    auth.require(Permission::PluginsCreate).await?;

    // Load plugin via manifest (required)
    let preview = loader::fetch_manifest(&state.http, &body.url)
        .await
        .map_err(|e| AppError::BadRequest(format!("Failed to fetch manifest: {}", e)))?;

    let plugin = loader::load_from_manifest(&state.http, &preview, body.sha256.as_deref())
        .await
        .map_err(|e| AppError::BadRequest(format!("Failed to load plugin: {}", e)))?;

    // Use manifest for rich secret metadata if available
    let required_secrets: Vec<super::types::SecretDefinition> =
        if let Some(manifest) = &plugin.manifest {
            manifest
                .required_secrets
                .iter()
                .map(|s| super::types::SecretDefinition {
                    key: s.key.clone(),
                    name: s.name.clone(),
                    description: s.description.clone(),
                })
                .collect()
        } else {
            plugin
                .info
                .required_secrets
                .iter()
                .map(|key| super::types::SecretDefinition {
                    key: key.clone(),
                    name: key.clone(),
                    description: None,
                })
                .collect()
        };

    // Newly added plugins are not configured (unless they have no required secrets)
    let secrets_configured = required_secrets.is_empty();

    let summary = PluginSummary {
        id: plugin.info.id.clone(),
        name: plugin.info.name.clone(),
        version: plugin.info.version.clone(),
        source: "url".to_string(),
        url: Some(body.url.clone()),
        sha256: body.sha256.clone(),
        enabled: true,
        auth_type: plugin.info.auth_type.clone(),
        required_secrets,
        secrets_configured,
        loaded_at: Some(now_rfc3339()),
        update_available: false,
        latest_version: None,
        pending_releases: Vec::new(),
    };

    let plugin_id = plugin.info.id.clone();

    // Register the plugin (this also persists to DB)
    state.plugin_registry.register(plugin).await;

    log_event(
        &state.db,
        EventLog {
            event_type: "plugin.added".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(plugin_id),
            detail: serde_json::json!({ "url": body.url }),
        },
        state.db_backend,
    )
    .await;

    Ok(Json(summary))
}

/// DELETE /admin/plugins/{id} - remove a plugin
pub(super) async fn remove(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(plugin_id): Path<String>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::PluginsDelete).await?;

    // Remove from registry
    let removed = state.plugin_registry.remove(&plugin_id).await;

    if removed.is_none() {
        return Err(AppError::NotFound(format!(
            "Plugin '{}' not found",
            plugin_id
        )));
    }

    // Remove from database
    let sql = adapt_sql(
        "DELETE FROM happyview_plugins WHERE id = ?",
        state.db_backend,
    );
    sqlx::query(&sql)
        .bind(&plugin_id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to delete plugin: {}", e)))?;

    log_event(
        &state.db,
        EventLog {
            event_type: "plugin.removed".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(plugin_id),
            detail: serde_json::json!({}),
        },
        state.db_backend,
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

/// POST /admin/plugins/{id}/reload - reload a plugin from its source
pub(super) async fn reload(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(plugin_id): Path<String>,
    body: Option<Json<super::types::ReloadPluginBody>>,
) -> Result<Json<PluginSummary>, AppError> {
    auth.require(Permission::PluginsCreate).await?;

    // Get current plugin to find its source
    let current = state
        .plugin_registry
        .get(&plugin_id)
        .await
        .ok_or_else(|| AppError::NotFound(format!("Plugin '{}' not found", plugin_id)))?;

    let (url, sha256) = match &current.source {
        crate::plugin::PluginSource::Url { url, sha256 } => (url.clone(), sha256.clone()),
        crate::plugin::PluginSource::File { .. } => {
            return Err(AppError::BadRequest(
                "Cannot reload file-based plugins via API".into(),
            ));
        }
    };

    let (url, sha256) = resolve_reload_url((url, sha256), body.map(|Json(b)| b));

    // Remove old plugin
    state.plugin_registry.remove(&plugin_id).await;

    // Reload via manifest
    let preview = loader::fetch_manifest(&state.http, &url)
        .await
        .map_err(|e| AppError::BadRequest(format!("Failed to fetch manifest: {}", e)))?;

    let plugin = loader::load_from_manifest(&state.http, &preview, sha256.as_deref())
        .await
        .map_err(|e| AppError::BadRequest(format!("Failed to reload plugin: {}", e)))?;

    // Use manifest for rich secret metadata if available
    let required_secrets: Vec<super::types::SecretDefinition> =
        if let Some(manifest) = &plugin.manifest {
            manifest
                .required_secrets
                .iter()
                .map(|s| super::types::SecretDefinition {
                    key: s.key.clone(),
                    name: s.name.clone(),
                    description: s.description.clone(),
                })
                .collect()
        } else {
            plugin
                .info
                .required_secrets
                .iter()
                .map(|key| super::types::SecretDefinition {
                    key: key.clone(),
                    name: key.clone(),
                    description: None,
                })
                .collect()
        };

    // Check if reloaded plugin still has its config
    let secrets_configured = required_secrets.is_empty() || {
        let sql = adapt_sql(
            "SELECT 1 FROM happyview_plugin_configs WHERE plugin_id = ?",
            state.db_backend,
        );
        sqlx::query_scalar::<_, i32>(&sql)
            .bind(&plugin.info.id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten()
            .is_some()
    };

    let summary = PluginSummary {
        id: plugin.info.id.clone(),
        name: plugin.info.name.clone(),
        version: plugin.info.version.clone(),
        source: "url".to_string(),
        url: Some(url.clone()),
        sha256,
        enabled: true,
        auth_type: plugin.info.auth_type.clone(),
        required_secrets,
        secrets_configured,
        loaded_at: Some(now_rfc3339()),
        update_available: false,
        latest_version: None,
        pending_releases: Vec::new(),
    };

    // Persist the (possibly new) URL so restarts pick it up
    let persist_sql = adapt_sql(
        "UPDATE happyview_plugins SET url = ?, sha256 = NULL WHERE id = ?",
        state.db_backend,
    );
    sqlx::query(&persist_sql)
        .bind(&url)
        .bind(&plugin.info.id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to persist plugin URL: {}", e)))?;

    // Register the reloaded plugin
    state.plugin_registry.register(plugin).await;

    log_event(
        &state.db,
        EventLog {
            event_type: "plugin.reloaded".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(plugin_id),
            detail: serde_json::json!({ "url": url }),
        },
        state.db_backend,
    )
    .await;

    Ok(Json(summary))
}

/// GET /admin/plugins/{id}/secrets - get plugin secrets (values masked)
pub(super) async fn get_secrets(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(plugin_id): Path<String>,
) -> Result<Json<PluginSecretsResponse>, AppError> {
    auth.require(Permission::PluginsRead).await?;

    let encryption_key = state
        .config
        .token_encryption_key
        .as_ref()
        .ok_or_else(|| AppError::Internal("Encryption key not configured".into()))?;

    // Verify plugin exists
    state
        .plugin_registry
        .get(&plugin_id)
        .await
        .ok_or_else(|| AppError::NotFound(format!("Plugin '{}' not found", plugin_id)))?;

    // Get secrets from plugin_configs table
    let sql = adapt_sql(
        "SELECT config FROM happyview_plugin_configs WHERE plugin_id = ?",
        state.db_backend,
    );

    let row: Option<(String,)> = sqlx::query_as(&sql)
        .bind(&plugin_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to fetch config: {}", e)))?;

    let secrets: HashMap<String, String> = match row {
        Some((config_json,)) => {
            let config: serde_json::Value = serde_json::from_str(&config_json)
                .map_err(|e| AppError::Internal(format!("Invalid config JSON: {}", e)))?;

            // Extract and decrypt secrets, then mask for display
            if let Some(secrets_obj) = config.get("secrets").and_then(|s| s.as_object()) {
                secrets_obj
                    .iter()
                    .filter_map(|(k, v)| {
                        v.as_str().and_then(|encrypted_b64| {
                            // Decode base64 and decrypt
                            let encrypted = BASE64.decode(encrypted_b64).ok()?;
                            let decrypted = decrypt(encryption_key, &encrypted).ok()?;
                            let val = String::from_utf8(decrypted).ok()?;

                            // Mask the value for display
                            let masked = if val.len() > 8 {
                                format!("********{}", &val[val.len() - 4..])
                            } else {
                                "********".to_string()
                            };
                            Some((k.clone(), masked))
                        })
                    })
                    .collect()
            } else {
                HashMap::new()
            }
        }
        None => HashMap::new(),
    };

    Ok(Json(PluginSecretsResponse { plugin_id, secrets }))
}

/// PUT /admin/plugins/{id}/secrets - update plugin secrets
pub(super) async fn update_secrets(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(plugin_id): Path<String>,
    Json(body): Json<UpdatePluginSecretsBody>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::PluginsCreate).await?;

    let encryption_key = state
        .config
        .token_encryption_key
        .as_ref()
        .ok_or_else(|| AppError::Internal("Encryption key not configured".into()))?;

    // Verify plugin exists
    state
        .plugin_registry
        .get(&plugin_id)
        .await
        .ok_or_else(|| AppError::NotFound(format!("Plugin '{}' not found", plugin_id)))?;

    // Get existing config or create new one
    let sql = adapt_sql(
        "SELECT config FROM happyview_plugin_configs WHERE plugin_id = ?",
        state.db_backend,
    );

    let row: Option<(String,)> = sqlx::query_as(&sql)
        .bind(&plugin_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to fetch config: {}", e)))?;

    let mut config: serde_json::Value = match row {
        Some((config_json,)) => serde_json::from_str(&config_json)
            .map_err(|e| AppError::Internal(format!("Invalid config JSON: {}", e)))?,
        None => serde_json::json!({}),
    };

    // Get existing encrypted secrets to preserve unchanged values
    let existing_secrets: HashMap<String, String> = config
        .get("secrets")
        .and_then(|s| s.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    // Merge secrets: if new value starts with "********", keep existing encrypted value
    // Otherwise, encrypt the new value
    let mut merged_secrets = serde_json::Map::new();
    for (key, value) in body.secrets {
        if value.starts_with("********") {
            // Keep existing encrypted value if present
            if let Some(existing) = existing_secrets.get(&key) {
                merged_secrets.insert(key, serde_json::Value::String(existing.clone()));
            }
        } else if !value.is_empty() {
            // Encrypt new value and store as base64
            let encrypted = encrypt(encryption_key, value.as_bytes())
                .map_err(|e| AppError::Internal(format!("Encryption failed: {}", e)))?;
            let encoded = BASE64.encode(&encrypted);
            merged_secrets.insert(key, serde_json::Value::String(encoded));
        }
        // Empty values are not stored (allows clearing a secret)
    }

    // Update config with merged secrets
    config["secrets"] = serde_json::Value::Object(merged_secrets);

    let config_json = serde_json::to_string(&config)
        .map_err(|e| AppError::Internal(format!("Failed to serialize config: {}", e)))?;

    // Upsert into plugin_configs
    let sql = adapt_sql(
        "INSERT INTO happyview_plugin_configs (plugin_id, config, updated_at) VALUES (?, ?, ?)
         ON CONFLICT (plugin_id) DO UPDATE SET config = EXCLUDED.config, updated_at = EXCLUDED.updated_at",
        state.db_backend,
    );

    sqlx::query(&sql)
        .bind(&plugin_id)
        .bind(&config_json)
        .bind(now_rfc3339())
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to update secrets: {}", e)))?;

    log_event(
        &state.db,
        EventLog {
            event_type: "plugin.secrets_updated".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(plugin_id),
            detail: serde_json::json!({}),
        },
        state.db_backend,
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

/// POST /admin/plugins/{id}/check-update — force a cache refresh for one plugin
pub(super) async fn check_update(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(plugin_id): Path<String>,
) -> Result<Json<PluginSummary>, AppError> {
    auth.require(Permission::PluginsCreate).await?;

    crate::plugin::official_registry::refresh_plugin(
        &state.http,
        &state.official_registry_config,
        &state.official_registry,
        &plugin_id,
    )
    .await
    .map_err(|e| AppError::BadRequest(format!("Update check failed: {}", e)))?;

    // Return the refreshed PluginSummary by re-running the same join logic
    let current = state
        .plugin_registry
        .get(&plugin_id)
        .await
        .ok_or_else(|| AppError::NotFound(format!("Plugin '{}' not found", plugin_id)))?;

    let guard = state.official_registry.read().await;
    let update_info = compute_update_info(&current.info.version, guard.plugins.get(&plugin_id));

    let required_secrets: Vec<super::types::SecretDefinition> =
        if let Some(manifest) = &current.manifest {
            manifest
                .required_secrets
                .iter()
                .map(|s| super::types::SecretDefinition {
                    key: s.key.clone(),
                    name: s.name.clone(),
                    description: s.description.clone(),
                })
                .collect()
        } else {
            current
                .info
                .required_secrets
                .iter()
                .map(|key| super::types::SecretDefinition {
                    key: key.clone(),
                    name: key.clone(),
                    description: None,
                })
                .collect()
        };

    let (source, url, sha256) = match &current.source {
        crate::plugin::PluginSource::File { path } => {
            ("file".to_string(), Some(path.display().to_string()), None)
        }
        crate::plugin::PluginSource::Url { url, sha256 } => {
            ("url".to_string(), Some(url.clone()), sha256.clone())
        }
    };

    Ok(Json(PluginSummary {
        id: current.info.id.clone(),
        name: current.info.name.clone(),
        version: current.info.version.clone(),
        source,
        url,
        sha256,
        enabled: true,
        auth_type: current.info.auth_type.clone(),
        required_secrets,
        secrets_configured: true,
        loaded_at: None,
        update_available: update_info.update_available,
        latest_version: update_info.latest_version,
        pending_releases: update_info.pending_releases,
    }))
}

/// GET /admin/plugins/official — list plugins from the official registry cache
pub(super) async fn list_official(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<super::types::OfficialPluginsListResponse>, AppError> {
    auth.require(Permission::PluginsRead).await?;

    let guard = state.official_registry.read().await;
    let plugins = guard
        .plugins
        .values()
        .map(|p| super::types::OfficialPluginSummary {
            id: p.id.clone(),
            name: p.name.clone(),
            description: p.description.clone(),
            icon_url: p.icon_url.clone(),
            latest_version: p.latest_version.clone(),
            manifest_url: p.manifest_url.clone(),
        })
        .collect::<Vec<_>>();

    Ok(Json(super::types::OfficialPluginsListResponse {
        plugins,
        last_refreshed_at: guard.last_refreshed_at.clone(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::official_registry::{OfficialPlugin, ReleaseEntry};

    fn entry(versions: &[&str]) -> OfficialPlugin {
        OfficialPlugin {
            id: "steam".into(),
            name: "steam".into(),
            description: None,
            icon_url: None,
            latest_version: versions[0].into(),
            manifest_url: "m".into(),
            wasm_url: "w".into(),
            releases: versions
                .iter()
                .map(|v| ReleaseEntry {
                    version: (*v).into(),
                    name: format!("v{v}"),
                    published_at: "2026-04-10T00:00:00Z".into(),
                    body: "notes".into(),
                })
                .collect(),
        }
    }

    #[test]
    fn compute_update_info_flags_update_when_behind() {
        let cached = entry(&["1.2.0", "1.1.0", "1.0.0"]);
        let info = compute_update_info("1.0.0", Some(&cached));
        assert!(info.update_available);
        assert_eq!(info.latest_version.as_deref(), Some("1.2.0"));
        assert_eq!(info.pending_releases.len(), 2);
        assert_eq!(info.pending_releases[0].version, "1.2.0");
        assert_eq!(info.pending_releases[1].version, "1.1.0");
    }

    #[test]
    fn compute_update_info_no_update_when_current() {
        let cached = entry(&["1.2.0"]);
        let info = compute_update_info("1.2.0", Some(&cached));
        assert!(!info.update_available);
        assert_eq!(info.latest_version.as_deref(), Some("1.2.0"));
        assert!(info.pending_releases.is_empty());
    }

    #[test]
    fn compute_update_info_no_cache_entry() {
        let info = compute_update_info("1.2.0", None);
        assert!(!info.update_available);
        assert!(info.latest_version.is_none());
        assert!(info.pending_releases.is_empty());
    }

    #[test]
    fn compute_update_info_handles_malformed_installed_version() {
        let cached = entry(&["1.2.0"]);
        let info = compute_update_info("not-semver", Some(&cached));
        assert!(!info.update_available);
        assert_eq!(info.latest_version.as_deref(), Some("1.2.0"));
    }

    #[test]
    fn resolve_reload_url_uses_override_and_clears_sha() {
        let current = ("https://old".to_string(), Some("deadbeef".to_string()));
        let body = super::super::types::ReloadPluginBody {
            url: Some("https://new".into()),
        };
        let (url, sha) = resolve_reload_url(current, Some(body));
        assert_eq!(url, "https://new");
        assert_eq!(sha, None);
    }

    #[test]
    fn resolve_reload_url_keeps_current_when_body_absent() {
        let current = ("https://old".to_string(), Some("deadbeef".to_string()));
        let (url, sha) = resolve_reload_url(current, None);
        assert_eq!(url, "https://old");
        assert_eq!(sha.as_deref(), Some("deadbeef"));
    }

    #[test]
    fn resolve_reload_url_keeps_current_when_body_url_is_none() {
        let current = ("https://old".to_string(), Some("deadbeef".to_string()));
        let body = super::super::types::ReloadPluginBody { url: None };
        let (url, sha) = resolve_reload_url(current, Some(body));
        assert_eq!(url, "https://old");
        assert_eq!(sha.as_deref(), Some("deadbeef"));
    }
}
