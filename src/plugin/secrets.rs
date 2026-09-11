//! Loading a plugin's stored/configured secrets. Shared by the external-auth
//! routes and library instantiation (`PluginExecutor`); neither should have
//! to depend on the other for it.

use std::collections::HashMap;

/// Load a plugin's secrets: from the encrypted DB config if an encryption
/// key is available, falling back to `PLUGIN_<ID>_*` environment variables.
pub async fn load_plugin_secrets(
    db: &sqlx::AnyPool,
    db_backend: crate::db::DatabaseBackend,
    encryption_key: Option<&[u8; 32]>,
    plugin_id: &str,
) -> HashMap<String, String> {
    use crate::plugin::encryption::decrypt;
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

    // Try to load from database first (if encryption key is available)
    if let Some(key) = encryption_key {
        let sql = crate::db::adapt_sql(
            "SELECT config FROM happyview_plugin_configs WHERE plugin_id = ?",
            db_backend,
        );

        if let Ok(Some((config_json,))) = crate::db::query_as::<(String,)>(&sql)
            .bind(plugin_id)
            .fetch_optional(db)
            .await
            && let Ok(config) = serde_json::from_str::<serde_json::Value>(&config_json)
            && let Some(secrets_obj) = config.get("secrets").and_then(|s| s.as_object())
        {
            // DB keys are full env var names (e.g., PLUGIN_STEAM_API_KEY)
            // Strip prefix to get short names for plugin (e.g., API_KEY)
            let prefix = format!("PLUGIN_{}_", plugin_id.to_uppercase());
            let db_secrets: HashMap<String, String> = secrets_obj
                .iter()
                .filter_map(|(k, v)| {
                    v.as_str().and_then(|encrypted_b64| {
                        // Decode base64 and decrypt
                        let encrypted = BASE64.decode(encrypted_b64).ok()?;
                        let decrypted = decrypt(key, &encrypted).ok()?;
                        let value = String::from_utf8(decrypted).ok()?;
                        // Strip prefix from key to get short name
                        let short_key = k.strip_prefix(&prefix).unwrap_or(k).to_string();
                        Some((short_key, value))
                    })
                })
                .collect();

            if !db_secrets.is_empty() {
                return db_secrets;
            }
        }
    }

    // Fall back to environment variables
    let prefix = format!("PLUGIN_{}_", plugin_id.to_uppercase());
    std::env::vars()
        .filter_map(|(k, v)| k.strip_prefix(&prefix).map(|name| (name.to_string(), v)))
        .collect()
}
