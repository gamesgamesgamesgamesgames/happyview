//! Operator-configured allowed hosts for `network:request:defined` plugins.
//!
//! `network:request` takes its hosts from the plugin author's manifest;
//! `network:request:defined` takes them from the operator instead, stored
//! alongside a plugin's secrets in `happyview_plugin_configs.config` under an
//! `allowed_hosts` key. The two live in the same JSON blob, so storing one
//! must never clobber the other.

use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::plugin::loader::is_valid_host_pattern;

/// Same shape as manifest `allowed_hosts`: a bare hostname, optionally
/// prefixed `*.` for subdomains. Names the first invalid entry so the caller
/// (an admin endpoint) can report which one to fix.
pub fn validate_hosts(hosts: &[String]) -> Result<(), String> {
    for host in hosts {
        if !is_valid_host_pattern(host) {
            return Err(format!(
                "'{host}' is not a valid allowed_hosts entry (bare hostname, optionally prefixed '*.')"
            ));
        }
    }
    Ok(())
}

/// Read a plugin's stored allowed-hosts list. Only a failed query is an
/// error: "not configured yet" and "the database is down" need different
/// fixes, and an operator staring at the hosts they already typed while
/// every request fails as "no hosts are configured" has no path from that
/// symptom back to a saturated pool or a locked file. No row, no
/// `allowed_hosts` field, config JSON that doesn't parse, and a field
/// present but not a `Vec<String>` all read as `Ok(empty)` — a plugin that
/// has never been configured has nothing it may reach yet. The latter two
/// are logged at `warn` since they mean the row is corrupt rather than
/// simply new.
pub async fn load_allowed_hosts(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    plugin_id: &str,
) -> Result<Vec<String>, sqlx::Error> {
    let sql = adapt_sql(
        "SELECT config FROM happyview_plugin_configs WHERE plugin_id = ?",
        backend,
    );

    let row: Option<(String,)> = crate::db::query_as(&sql)
        .bind(plugin_id)
        .fetch_optional(db)
        .await?;

    let Some((config_json,)) = row else {
        return Ok(Vec::new());
    };

    let config: serde_json::Value = match serde_json::from_str(&config_json) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(plugin_id, error = %e, "plugin config is not valid JSON; treating allowed_hosts as empty");
            return Ok(Vec::new());
        }
    };

    Ok(match config.get("allowed_hosts") {
        None => Vec::new(),
        Some(value) => match serde_json::from_value::<Vec<String>>(value.clone()) {
            Ok(hosts) => hosts,
            Err(e) => {
                tracing::warn!(plugin_id, error = %e, "plugin config allowed_hosts is malformed; treating as empty");
                Vec::new()
            }
        },
    })
}

/// Replace a plugin's stored allowed-hosts list, preserving the row's other
/// fields (`secrets`) the same way `admin::plugins::update_secrets` preserves
/// `allowed_hosts` when it writes `secrets` — a read-merge-upsert rather than
/// an overwrite, since both fields share one row.
pub async fn store_allowed_hosts(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    plugin_id: &str,
    hosts: &[String],
) -> Result<(), AppError> {
    validate_hosts(hosts).map_err(AppError::BadRequest)?;

    let sql = adapt_sql(
        "SELECT config FROM happyview_plugin_configs WHERE plugin_id = ?",
        backend,
    );
    let row: Option<(String,)> = crate::db::query_as(&sql)
        .bind(plugin_id)
        .fetch_optional(db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to fetch plugin config: {e}")))?;

    let mut config: serde_json::Value = match row {
        Some((config_json,)) => serde_json::from_str(&config_json)
            .map_err(|e| AppError::Internal(format!("invalid plugin config JSON: {e}")))?,
        None => serde_json::json!({}),
    };
    config["allowed_hosts"] = serde_json::json!(hosts);

    let config_json = serde_json::to_string(&config)
        .map_err(|e| AppError::Internal(format!("failed to serialize plugin config: {e}")))?;

    let sql = adapt_sql(
        "INSERT INTO happyview_plugin_configs (plugin_id, config, updated_at) VALUES (?, ?, ?)
         ON CONFLICT (plugin_id) DO UPDATE SET config = EXCLUDED.config, updated_at = EXCLUDED.updated_at",
        backend,
    );
    crate::db::query(&sql)
        .bind(plugin_id)
        .bind(&config_json)
        .bind(now_rfc3339())
        .execute(db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to update plugin config: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::migrated_memory_pool;

    /// `happyview_plugin_configs.plugin_id` is a foreign key into
    /// `happyview_plugins`, so a config row needs a matching plugin row
    /// first — the columns beyond `id`/`source`/`api_version` don't matter
    /// to anything under test here.
    async fn insert_plugin_row(pool: &sqlx::AnyPool, plugin_id: &str) {
        let sql = adapt_sql(
            "INSERT INTO happyview_plugins (id, source, api_version) VALUES (?, 'file', '2')",
            DatabaseBackend::Sqlite,
        );
        crate::db::query(&sql)
            .bind(plugin_id)
            .execute(pool)
            .await
            .unwrap();
    }

    #[test]
    fn validate_hosts_accepts_a_wildcard_pattern() {
        assert!(validate_hosts(&["*.example.com".to_string()]).is_ok());
    }

    #[test]
    fn validate_hosts_rejects_a_url() {
        let err = validate_hosts(&["http://x".to_string()]).unwrap_err();
        assert!(err.contains("http://x"), "{err}");
    }

    #[test]
    fn validate_hosts_rejects_a_double_dot() {
        assert!(validate_hosts(&["a..b".to_string()]).is_err());
    }

    #[test]
    fn validate_hosts_rejects_an_empty_entry() {
        assert!(validate_hosts(&[String::new()]).is_err());
    }

    #[test]
    fn validate_hosts_names_the_first_invalid_entry() {
        let err =
            validate_hosts(&["good.example.com".to_string(), "bad host".to_string()]).unwrap_err();
        assert!(err.contains("bad host"), "{err}");
    }

    #[tokio::test]
    async fn load_allowed_hosts_is_empty_with_no_row() {
        let pool = migrated_memory_pool().await;
        let hosts = load_allowed_hosts(&pool, DatabaseBackend::Sqlite, "nope")
            .await
            .unwrap();
        assert!(hosts.is_empty());
    }

    /// An unmigrated pool has no `happyview_plugin_configs` table, so the
    /// query itself fails — the one case that must surface as `Err` rather
    /// than read as "nothing configured yet".
    #[tokio::test]
    async fn load_allowed_hosts_fails_on_a_query_error() {
        let pool = crate::test_support::memory_pool().await;
        let err = load_allowed_hosts(&pool, DatabaseBackend::Sqlite, "steam")
            .await
            .unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("no such table"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn store_then_load_round_trips_and_leaves_secrets_untouched() {
        let pool = migrated_memory_pool().await;
        insert_plugin_row(&pool, "steam").await;

        // A secret written first, as the admin secrets endpoint would.
        let sql = adapt_sql(
            "INSERT INTO happyview_plugin_configs (plugin_id, config, updated_at) VALUES (?, ?, ?)",
            DatabaseBackend::Sqlite,
        );
        crate::db::query(&sql)
            .bind("steam")
            .bind(
                serde_json::json!({"secrets": {"PLUGIN_STEAM_API_KEY": "encrypted-blob"}})
                    .to_string(),
            )
            .bind(now_rfc3339())
            .execute(&pool)
            .await
            .unwrap();

        let hosts = vec![
            "api.example.com".to_string(),
            "*.cdn.example.com".to_string(),
        ];
        store_allowed_hosts(&pool, DatabaseBackend::Sqlite, "steam", &hosts)
            .await
            .unwrap();

        let loaded = load_allowed_hosts(&pool, DatabaseBackend::Sqlite, "steam")
            .await
            .unwrap();
        assert_eq!(loaded, hosts);

        let sql = adapt_sql(
            "SELECT config FROM happyview_plugin_configs WHERE plugin_id = ?",
            DatabaseBackend::Sqlite,
        );
        let (config_json,): (String,) = crate::db::query_as(&sql)
            .bind("steam")
            .fetch_one(&pool)
            .await
            .unwrap();
        let config: serde_json::Value = serde_json::from_str(&config_json).unwrap();
        assert_eq!(
            config["secrets"]["PLUGIN_STEAM_API_KEY"],
            serde_json::json!("encrypted-blob")
        );
    }

    #[tokio::test]
    async fn store_allowed_hosts_refuses_an_invalid_host() {
        let pool = migrated_memory_pool().await;
        let err = store_allowed_hosts(
            &pool,
            DatabaseBackend::Sqlite,
            "steam",
            &["http://x".to_string()],
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }
}
