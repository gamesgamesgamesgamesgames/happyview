use tracing::{debug, error, info, warn};

/// The severity a plugin named, as the SDK defines it. A level the SDK does not
/// know fails to parse; the binding logs such a line at `info` rather than
/// dropping it.
pub use happyview_plugin_sdk::wire::Level as LogLevel;

/// Log a message from a plugin.
///
/// Always emits to `tracing`. If `db` is `Some`, also spawns a detached task
/// that writes the event to the `event_logs` table so it appears in the
/// Event Logs UI. The spawned task is fire-and-forget; errors are logged by
/// `event_log::log_event` but not returned to the caller.
pub fn log(
    plugin_id: &str,
    level: LogLevel,
    message: &str,
    db: Option<sqlx::AnyPool>,
    db_backend: crate::db::DatabaseBackend,
) {
    match level {
        LogLevel::Debug => debug!(plugin = %plugin_id, "{}", message),
        LogLevel::Info => info!(plugin = %plugin_id, "{}", message),
        LogLevel::Warn => warn!(plugin = %plugin_id, "{}", message),
        LogLevel::Error => error!(plugin = %plugin_id, "{}", message),
    }

    let Some(db) = db else { return };

    let severity = match level {
        LogLevel::Debug | LogLevel::Info => crate::event_log::Severity::Info,
        LogLevel::Warn => crate::event_log::Severity::Warn,
        LogLevel::Error => crate::event_log::Severity::Error,
    };
    let level_str = level.as_str();

    let event = crate::event_log::EventLog {
        event_type: "plugin.log".to_string(),
        severity,
        actor_did: None,
        subject: Some(plugin_id.to_string()),
        detail: serde_json::json!({
            "level": level_str,
            "message": message,
        }),
    };

    tokio::spawn(async move {
        crate::event_log::log_event(&db, event, db_backend).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The binding parses the level string with `unwrap_or_default()`, so
    /// these are the levels the host actually ends up logging at.
    #[test]
    fn test_log_level_from_str_known_values() {
        for (text, expected) in [
            ("debug", LogLevel::Debug),
            ("DEBUG", LogLevel::Debug),
            ("info", LogLevel::Info),
            ("INFO", LogLevel::Info),
            ("warn", LogLevel::Warn),
            ("warning", LogLevel::Warn),
            ("WARN", LogLevel::Warn),
            ("error", LogLevel::Error),
            ("ERROR", LogLevel::Error),
        ] {
            assert_eq!(
                text.parse::<LogLevel>().unwrap_or_default(),
                expected,
                "{text}"
            );
        }
    }

    #[test]
    fn test_log_level_from_str_unknown_defaults_to_info() {
        for text in ["trace", "", "unknown"] {
            assert!(text.parse::<LogLevel>().is_err(), "{text}");
            assert_eq!(text.parse::<LogLevel>().unwrap_or_default(), LogLevel::Info);
        }
    }

    #[test]
    fn test_log_does_not_panic() {
        // With db=None, only the tracing path runs. Verifies each level does not panic.
        let backend = crate::db::DatabaseBackend::Sqlite;
        log(
            "test-plugin",
            LogLevel::Debug,
            "debug message",
            None,
            backend,
        );
        log("test-plugin", LogLevel::Info, "info message", None, backend);
        log("test-plugin", LogLevel::Warn, "warn message", None, backend);
        log(
            "test-plugin",
            LogLevel::Error,
            "error message",
            None,
            backend,
        );
    }
}
