use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::AnyPool;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
    Error,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Info => write!(f, "info"),
            Severity::Warn => write!(f, "warn"),
            Severity::Error => write!(f, "error"),
        }
    }
}

pub struct EventLog {
    pub event_type: String,
    pub severity: Severity,
    pub actor_did: Option<String>,
    pub subject: Option<String>,
    pub detail: Value,
}

pub async fn spawn_retention_cleanup(db: AnyPool, retention_days: u32, backend: DatabaseBackend) {
    if retention_days == 0 {
        tracing::info!("event log retention cleanup disabled");
        return;
    }

    tracing::info!(retention_days, "starting event log retention cleanup task");

    let interval = tokio::time::Duration::from_secs(3600); // 1 hour

    // Build database-specific cleanup query
    // Cannot use adapt_sql: Postgres uses make_interval(days => $1) which has no SQLite equivalent pattern.
    let cleanup_sql = match backend {
        DatabaseBackend::Sqlite => {
            "DELETE FROM happyview_event_logs WHERE created_at < datetime('now', '-' || ? || ' days')"
                .to_string()
        }
        DatabaseBackend::Postgres => {
            "DELETE FROM happyview_event_logs WHERE created_at < NOW() - make_interval(days => $1)"
                .to_string()
        }
    };

    loop {
        tokio::time::sleep(interval).await;

        let result = sqlx::query(&cleanup_sql)
            .bind(retention_days as i32)
            .execute(&db)
            .await;

        match result {
            Ok(result) => {
                let count = result.rows_affected();
                if count > 0 {
                    tracing::info!(count, "cleaned up old event logs");
                }
            }
            Err(e) => {
                tracing::warn!("failed to clean up event logs: {e}");
            }
        }
    }
}

pub async fn log_event(db: &AnyPool, event: EventLog, backend: DatabaseBackend) {
    let severity = event.severity.to_string();
    let detail_str = serde_json::to_string(&event.detail).unwrap_or_else(|_| "{}".to_string());
    let id = Uuid::new_v4().to_string();
    let created_at = now_rfc3339();

    let sql = adapt_sql(
        "INSERT INTO happyview_event_logs (id, event_type, severity, actor_did, subject, detail, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        backend,
    );

    let result = sqlx::query(&sql)
        .bind(&id)
        .bind(&event.event_type)
        .bind(&severity)
        .bind(&event.actor_did)
        .bind(&event.subject)
        .bind(&detail_str)
        .bind(&created_at)
        .execute(db)
        .await;

    if let Err(e) = result {
        tracing::warn!(event_type = %event.event_type, "failed to log event: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_display() {
        assert_eq!(Severity::Info.to_string(), "info");
        assert_eq!(Severity::Warn.to_string(), "warn");
        assert_eq!(Severity::Error.to_string(), "error");
    }

    #[test]
    fn severity_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Severity::Info).unwrap(), "\"info\"");
        assert_eq!(
            serde_json::to_string(&Severity::Error).unwrap(),
            "\"error\""
        );
    }

    #[test]
    fn event_log_construction() {
        let event = EventLog {
            event_type: "lexicon.created".to_string(),
            severity: Severity::Info,
            actor_did: Some("did:plc:test".to_string()),
            subject: Some("com.example.test".to_string()),
            detail: serde_json::json!({"revision": 1}),
        };
        assert_eq!(event.event_type, "lexicon.created");
        assert_eq!(event.severity, Severity::Info);
        assert_eq!(event.actor_did.unwrap(), "did:plc:test");
    }
}
