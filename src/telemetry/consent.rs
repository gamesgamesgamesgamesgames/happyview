//! Consent state for telemetry reporting.

use sqlx::AnyPool;

use crate::admin::settings::get_setting;
use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};

pub const KEY_MODE: &str = "telemetry.mode";
pub const KEY_CONTACT: &str = "telemetry.contact";
pub const KEY_LEXICON_NAMES: &str = "telemetry.lexicon_names";
pub const KEY_LEXICON_STRUCTURE: &str = "telemetry.lexicon_structure";
pub const KEY_LEXICON_DOCUMENTS: &str = "telemetry.lexicon_documents";
pub const KEY_INSTANCE_ID: &str = "telemetry.instance_id";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TelemetryMode {
    Off,
    Manual,
    Auto,
}

impl TelemetryMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Manual => "manual",
            Self::Auto => "auto",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "manual" => Self::Manual,
            "auto" => Self::Auto,
            _ => Self::Off,
        }
    }

    pub fn reports(self) -> bool {
        matches!(self, Self::Manual | Self::Auto)
    }
}

#[derive(Debug, Clone)]
pub struct Consent {
    pub mode: TelemetryMode,
    pub contact: Option<String>,
    pub lexicon_names: bool,
    pub lexicon_structure: bool,
    pub lexicon_documents: bool,
    pub instance_id: Option<String>,
}

async fn flag(pool: &AnyPool, backend: DatabaseBackend, key: &str) -> bool {
    get_setting(pool, key, backend)
        .await
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes"))
        .unwrap_or(false)
}

pub async fn load(pool: &AnyPool, backend: DatabaseBackend) -> Consent {
    let mode = get_setting(pool, KEY_MODE, backend)
        .await
        .map(|v| TelemetryMode::parse(&v))
        .unwrap_or(TelemetryMode::Off);

    let contact = get_setting(pool, KEY_CONTACT, backend)
        .await
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty());

    Consent {
        mode,
        contact,
        lexicon_names: flag(pool, backend, KEY_LEXICON_NAMES).await,
        lexicon_structure: flag(pool, backend, KEY_LEXICON_STRUCTURE).await,
        lexicon_documents: flag(pool, backend, KEY_LEXICON_DOCUMENTS).await,
        instance_id: get_setting(pool, KEY_INSTANCE_ID, backend).await,
    }
}

/// Return the instance identifier, minting one if reporting is enabled and
/// none exists yet.
pub async fn ensure_instance_id(pool: &AnyPool, backend: DatabaseBackend) -> Option<String> {
    let mode = get_setting(pool, KEY_MODE, backend)
        .await
        .map(|v| TelemetryMode::parse(&v))
        .unwrap_or(TelemetryMode::Off);

    if !mode.reports() {
        return None;
    }

    if let Some(existing) = get_setting(pool, KEY_INSTANCE_ID, backend).await {
        return Some(existing);
    }

    let id = uuid::Uuid::new_v4().to_string();
    let sql = adapt_sql(
        "INSERT INTO happyview_instance_settings (key, value, updated_at) VALUES (?, ?, ?) ON CONFLICT (key) DO NOTHING",
        backend,
    );
    if let Err(e) = crate::db::query(&sql)
        .bind(KEY_INSTANCE_ID)
        .bind(&id)
        .bind(now_rfc3339())
        .execute(pool)
        .await
    {
        tracing::warn!(error = %e, "instance id insert failed");
    }

    get_setting(pool, KEY_INSTANCE_ID, backend).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DatabaseBackend;
    use crate::test_support::memory_pool;

    async fn pool_with_settings() -> sqlx::AnyPool {
        let pool = memory_pool().await;
        crate::db::query(
            "CREATE TABLE happyview_instance_settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("create instance_settings");
        pool
    }

    async fn set(pool: &sqlx::AnyPool, key: &str, value: &str) {
        let sql = crate::db::adapt_sql(
            "INSERT INTO happyview_instance_settings (key, value, updated_at) VALUES (?, ?, ?)",
            DatabaseBackend::Sqlite,
        );
        crate::db::query(&sql)
            .bind(key)
            .bind(value)
            .bind(crate::db::now_rfc3339())
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn defaults_to_off_with_everything_disabled() {
        let pool = pool_with_settings().await;
        let consent = load(&pool, DatabaseBackend::Sqlite).await;

        assert_eq!(consent.mode, TelemetryMode::Off);
        assert!(consent.contact.is_none());
        assert!(!consent.lexicon_names);
        assert!(!consent.lexicon_structure);
        assert!(!consent.lexicon_documents);
        assert!(consent.instance_id.is_none());
    }

    #[tokio::test]
    async fn unrecognised_mode_reads_as_off() {
        let pool = pool_with_settings().await;
        set(&pool, "telemetry.mode", "enabled").await;

        assert_eq!(
            load(&pool, DatabaseBackend::Sqlite).await.mode,
            TelemetryMode::Off
        );
    }

    #[tokio::test]
    async fn no_instance_id_is_generated_while_off() {
        let pool = pool_with_settings().await;

        assert!(
            ensure_instance_id(&pool, DatabaseBackend::Sqlite)
                .await
                .is_none()
        );

        let stored = crate::admin::settings::get_setting(
            &pool,
            "telemetry.instance_id",
            DatabaseBackend::Sqlite,
        )
        .await;
        assert!(
            stored.is_none(),
            "a disabled instance must hold no telemetry identifier"
        );
    }

    #[tokio::test]
    async fn instance_id_is_generated_on_enable_and_is_stable() {
        let pool = pool_with_settings().await;
        set(&pool, "telemetry.mode", "auto").await;

        let first = ensure_instance_id(&pool, DatabaseBackend::Sqlite)
            .await
            .unwrap();
        let second = ensure_instance_id(&pool, DatabaseBackend::Sqlite)
            .await
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(first.len(), 36, "expected a hyphenated UUID");
    }

    #[tokio::test]
    async fn instance_id_survives_a_disable_reenable_cycle() {
        let pool = pool_with_settings().await;
        set(&pool, "telemetry.mode", "auto").await;
        let original = ensure_instance_id(&pool, DatabaseBackend::Sqlite)
            .await
            .unwrap();

        let sql = crate::db::adapt_sql(
            "UPDATE happyview_instance_settings SET value = ? WHERE key = 'telemetry.mode'",
            DatabaseBackend::Sqlite,
        );
        crate::db::query(&sql)
            .bind("off")
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            ensure_instance_id(&pool, DatabaseBackend::Sqlite)
                .await
                .is_none()
        );

        crate::db::query(&sql)
            .bind("auto")
            .execute(&pool)
            .await
            .unwrap();
        let after = ensure_instance_id(&pool, DatabaseBackend::Sqlite)
            .await
            .unwrap();

        assert_eq!(original, after);
    }

    #[tokio::test]
    async fn lexicon_toggles_are_independent() {
        let pool = pool_with_settings().await;
        set(&pool, "telemetry.mode", "manual").await;
        set(&pool, "telemetry.lexicon_structure", "true").await;

        let consent = load(&pool, DatabaseBackend::Sqlite).await;
        assert_eq!(consent.mode, TelemetryMode::Manual);
        assert!(consent.lexicon_structure);
        assert!(!consent.lexicon_names, "structure must not imply names");
        assert!(!consent.lexicon_documents);
    }

    #[tokio::test]
    async fn ensure_instance_id_does_not_regenerate_a_preexisting_id() {
        let pool = pool_with_settings().await;
        set(&pool, "telemetry.mode", "auto").await;
        set(
            &pool,
            "telemetry.instance_id",
            "00000000-0000-0000-0000-000000000000",
        )
        .await;

        let id = ensure_instance_id(&pool, DatabaseBackend::Sqlite)
            .await
            .unwrap();

        assert_eq!(id, "00000000-0000-0000-0000-000000000000");
    }
}
