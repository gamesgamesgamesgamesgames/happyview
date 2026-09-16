//! Loading a plugin's stored/configured secrets. Shared by the external-auth
//! routes and library instantiation (`PluginExecutor`); neither should have
//! to depend on the other for it.

use std::collections::HashMap;

/// The environment-variable prefix a plugin's secrets live under.
///
/// `PLUGIN_`, the id upper-cased, `_`. Every byte that is not ASCII
/// alphanumeric becomes `_`: a plugin id may contain characters an env var
/// name may not, and `auth-steam` would otherwise derive `PLUGIN_AUTH-STEAM_`,
/// which no shell can export, so every secret would read as unset.
pub fn secret_env_prefix(plugin_id: &str) -> String {
    let mut prefix = String::with_capacity(plugin_id.len() + 8);
    prefix.push_str("PLUGIN_");
    for byte in plugin_id.bytes() {
        prefix.push(if byte.is_ascii_alphanumeric() {
            byte.to_ascii_uppercase() as char
        } else {
            '_'
        });
    }
    prefix.push('_');
    prefix
}

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
            let prefix = secret_env_prefix(plugin_id);
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
    let prefix = secret_env_prefix(plugin_id);
    std::env::vars()
        .filter_map(|(k, v)| k.strip_prefix(&prefix).map(|name| (name.to_string(), v)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::secret_env_prefix;

    #[test]
    fn a_plain_id_is_simply_upper_cased() {
        assert_eq!(secret_env_prefix("steam"), "PLUGIN_STEAM_");
        assert_eq!(secret_env_prefix("xbox360"), "PLUGIN_XBOX360_");
    }

    #[test]
    fn punctuation_a_plugin_id_allows_but_an_env_var_does_not_becomes_an_underscore() {
        assert_eq!(secret_env_prefix("auth-steam"), "PLUGIN_AUTH_STEAM_");
        assert_eq!(secret_env_prefix("a.b"), "PLUGIN_A_B_");
        assert_eq!(secret_env_prefix("a b"), "PLUGIN_A_B_");
        // Already-underscored ids are unchanged, so existing secrets keep working.
        assert_eq!(secret_env_prefix("sdk_auth"), "PLUGIN_SDK_AUTH_");
    }

    #[test]
    fn an_empty_id_still_produces_a_prefix_rather_than_matching_everything() {
        assert_eq!(secret_env_prefix(""), "PLUGIN__");
    }
}
