//! What an edit to `happyview.toml` means for a running dev server.

use std::sync::Arc;

use crate::AppState;
use crate::config::Config;
use crate::config::project::ProjectConfig;
use crate::dev::DevSession;

/// Settings read once at startup. `state.config` is the boot copy, so an
/// edit to one of these cannot take effect under a running server.
///
/// Not listed because a reload does reach them: `APP_NAME`, `LOGO_URI`,
/// `TOS_URI`, `POLICY_URI` and `EVENT_LOG_RETENTION_DAYS` are read through
/// `vars::var` per request (the instance-settings fallback), and
/// `SUPERADMIN_DID` and the declarations are applied by the reload itself.
/// `USER_AGENT` is derived from `PUBLIC_URL`, which is listed.
///
/// `JETSTREAM_PROTOCOL` and `JETSTREAM_COMPRESSION` are not `Config` fields:
/// the consumer reads them through `vars::var` once, when it is spawned, so
/// they are judged by [`jetstream_changed`] against the boot file instead.
pub fn restart_required(running: &Config, edited: &Config) -> Vec<&'static str> {
    let checks = [
        ("HOST", running.host != edited.host),
        ("PORT", running.port != edited.port),
        ("DATABASE_URL", running.database_url != edited.database_url),
        (
            "DATABASE_BACKEND",
            running.database_backend != edited.database_backend,
        ),
        (
            "SQLITE_JOURNAL_SIZE_LIMIT",
            running.sqlite_journal_size_limit != edited.sqlite_journal_size_limit,
        ),
        ("PUBLIC_URL", running.public_url != edited.public_url),
        ("BASE_PATH", running.base_path != edited.base_path),
        (
            "SESSION_SECRET",
            running.session_secret != edited.session_secret,
        ),
        (
            "TOKEN_ENCRYPTION_KEY",
            running.token_encryption_key != edited.token_encryption_key,
        ),
        (
            "JETSTREAM_URL",
            running.jetstream_url != edited.jetstream_url,
        ),
        ("RELAY_URL", running.relay_url != edited.relay_url),
        ("PLC_URL", running.plc_url != edited.plc_url),
        ("STATIC_DIR", running.static_dir != edited.static_dir),
        (
            "TELEMETRY_COLLECTOR_URL",
            running.telemetry_collector_url != edited.telemetry_collector_url,
        ),
        (
            "DEFAULT_RATE_LIMIT_CAPACITY",
            running.default_rate_limit_capacity != edited.default_rate_limit_capacity,
        ),
        (
            "DEFAULT_RATE_LIMIT_REFILL_RATE",
            running.default_rate_limit_refill_rate != edited.default_rate_limit_refill_rate,
        ),
        (
            "PLUGIN_CACHE_DIR",
            running.plugin_cache_dir != edited.plugin_cache_dir,
        ),
        (
            "PLATFORM_API_KEY_HASH",
            running.platform_api_key_hash != edited.platform_api_key_hash,
        ),
        (
            "JOB_WORKER_CONCURRENCY",
            running.job_worker_concurrency != edited.job_worker_concurrency,
        ),
        (
            "HAPPYVIEW_SCRIPT_ENV_FILE",
            running.script_env_file != edited.script_env_file,
        ),
        (
            "HAPPYVIEW_DEV_BACKFILL",
            running.auto_backfill != edited.auto_backfill,
        ),
        ("PLUGIN_DIR", running.plugin_dir != edited.plugin_dir),
        (
            "JETSTREAM_PROTOCOL",
            jetstream_changed("JETSTREAM_PROTOCOL", running, edited),
        ),
        (
            "JETSTREAM_COMPRESSION",
            jetstream_changed("JETSTREAM_COMPRESSION", running, edited),
        ),
    ];
    checks
        .into_iter()
        .filter_map(|(name, changed)| changed.then_some(name))
        .collect()
}

/// Whether the file's value for `name` differs between the boot project and
/// the edited one. A variable set in the environment wins on both sides, so
/// an edit beneath it changes nothing and reports nothing.
fn jetstream_changed(name: &str, running: &Config, edited: &Config) -> bool {
    if std::env::var(name).is_ok() {
        return false;
    }
    let file_value = |config: &Config| {
        config
            .project
            .as_ref()
            .and_then(|p| p.env_layer(crate::config::ConfigMode::Dev).remove(name))
    };
    file_value(running) != file_value(edited)
}

pub fn dropped_declarations(before: &ProjectConfig, after: &ProjectConfig) -> Vec<String> {
    let labelers = before
        .labels
        .subscribe
        .iter()
        .filter(|did| !after.labels.subscribe.contains(did))
        .map(|did| format!("labeler {did}"));
    let lexicons = before
        .network_lexicons
        .include
        .iter()
        .filter(|nsid| !after.network_lexicons.include.contains(nsid))
        .map(|nsid| format!("network lexicon {nsid}"));
    labelers.chain(lexicons).collect()
}

/// The lines an edit earns, in the order they print.
fn notices(running: &Config, edited: &Config, before: Option<&ProjectConfig>) -> Vec<String> {
    let mut out = Vec::new();
    let restart = restart_required(running, edited);
    if !restart.is_empty() {
        out.push(format!("restart required to apply: {}", restart.join(", ")));
    }
    let empty = ProjectConfig::default();
    let dropped = dropped_declarations(
        before.unwrap_or(&empty),
        edited.project.as_deref().unwrap_or(&empty),
    );
    if !dropped.is_empty() {
        out.push(format!(
            "no longer declared in happyview.toml: {}; they stay until you delete them in the dashboard, which a restart allows",
            dropped.join(", ")
        ));
    }
    out
}

/// Reloads the file and applies what it adds. Removals are only reported:
/// a production redeploy leaves a de-declared row in place too, and the
/// admin API keeps judging "declared" from the boot configuration.
pub async fn reload(
    state: &AppState,
    session: &DevSession,
    before: &mut Option<Arc<ProjectConfig>>,
) -> Result<(), String> {
    let edited =
        crate::dev::load_config(&session.root, &session.secrets).map_err(|e| e.to_string())?;

    for line in notices(&state.config, &edited, before.as_deref()) {
        println!("{line}");
    }

    crate::config::declarations::apply_at_boot(
        &edited,
        &state.db,
        state.db_backend,
        &state.http,
        &state.lexicons,
    )
    .await;
    *before = edited.project.clone();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DatabaseBackend;

    fn base() -> Config {
        crate::test_support::test_config(DatabaseBackend::Sqlite)
    }

    #[test]
    fn listener_and_database_changes_need_a_restart() {
        let running = base();
        let mut edited = running.clone();
        assert!(restart_required(&running, &edited).is_empty());
        edited.port += 1;
        edited.database_url = "sqlite://other.db".into();
        assert_eq!(
            restart_required(&running, &edited),
            vec!["PORT", "DATABASE_URL"]
        );
    }

    #[test]
    fn the_backfill_switch_and_plugin_dir_need_a_restart() {
        let running = base();
        let mut edited = running.clone();
        edited.auto_backfill = !edited.auto_backfill;
        edited.plugin_dir = "./elsewhere".into();
        edited.relay_url = "http://relay.invalid".into();
        edited.job_worker_concurrency += 1;
        edited.session_secret = "another-secret".into();
        assert_eq!(
            restart_required(&running, &edited),
            vec![
                "SESSION_SECRET",
                "RELAY_URL",
                "JOB_WORKER_CONCURRENCY",
                "HAPPYVIEW_DEV_BACKFILL",
                "PLUGIN_DIR"
            ]
        );
    }

    fn with_jetstream(
        protocol: Option<crate::config::project::JetstreamProtocol>,
        compression: Option<bool>,
    ) -> Config {
        let mut config = base();
        let mut project = ProjectConfig::default();
        project.jetstream.protocol = protocol;
        project.jetstream.compression = compression;
        config.project = Some(Arc::new(project));
        config
    }

    /// The consumer reads both once, at spawn, so an edit cannot reach it.
    #[test]
    #[serial_test::serial]
    fn a_jetstream_edit_needs_a_restart() {
        use crate::config::project::JetstreamProtocol;
        for key in ["JETSTREAM_PROTOCOL", "JETSTREAM_COMPRESSION"] {
            unsafe { std::env::remove_var(key) };
        }
        let running = with_jetstream(Some(JetstreamProtocol::V1), None);
        assert!(restart_required(&running, &running.clone()).is_empty());
        let edited = with_jetstream(Some(JetstreamProtocol::V2), Some(false));
        assert_eq!(
            restart_required(&running, &edited),
            vec!["JETSTREAM_PROTOCOL", "JETSTREAM_COMPRESSION"]
        );
    }

    #[test]
    #[serial_test::serial]
    fn a_jetstream_edit_beneath_the_environment_reports_nothing() {
        use crate::config::project::JetstreamProtocol;
        unsafe { std::env::set_var("JETSTREAM_PROTOCOL", "v1") };
        unsafe { std::env::remove_var("JETSTREAM_COMPRESSION") };
        let running = with_jetstream(Some(JetstreamProtocol::V1), None);
        let edited = with_jetstream(Some(JetstreamProtocol::V2), None);
        let reported = restart_required(&running, &edited);
        unsafe { std::env::remove_var("JETSTREAM_PROTOCOL") };
        assert!(reported.is_empty(), "got: {reported:?}");
    }

    #[test]
    fn removed_declarations_are_named() {
        let mut before = ProjectConfig::default();
        before.labels.subscribe = vec!["did:plc:keep".into(), "did:plc:gone".into()];
        before.network_lexicons.include = vec!["app.bsky.actor.profile".into()];
        let mut after = ProjectConfig::default();
        after.labels.subscribe = vec!["did:plc:keep".into(), "did:plc:new".into()];
        assert_eq!(
            dropped_declarations(&before, &after),
            vec![
                "labeler did:plc:gone".to_string(),
                "network lexicon app.bsky.actor.profile".to_string()
            ]
        );
    }

    /// Pins the call site: dropping the `apply_at_boot` call from `reload`
    /// leaves the helpers green and the subscription unwritten.
    #[tokio::test]
    #[serial_test::serial]
    async fn a_reload_applies_an_added_declaration() {
        let _reset = crate::config::vars::LayerReset;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("happyview.toml"),
            "[labels]\nsubscribe = [\"did:plc:added\"]\n",
        )
        .unwrap();
        let secrets = crate::dev::secrets::load_or_create(&dir.path().join(".happyview")).unwrap();
        let session = DevSession {
            root: dir.path().to_path_buf(),
            public_url: String::new(),
            secrets,
            watch_mode: crate::dev::watch::WatchMode::Native,
        };
        let pool = crate::test_support::migrated_memory_pool().await;
        let state = crate::test_support::test_state_with_pool(pool);
        let mut before = None;

        reload(&state, &session, &mut before).await.unwrap();

        let row: Option<(String,)> =
            crate::db::query_as("SELECT did FROM happyview_labeler_subscriptions WHERE did = ?")
                .bind("did:plc:added")
                .fetch_optional(&state.db)
                .await
                .unwrap();
        assert!(row.is_some());
        assert_eq!(
            before.unwrap().labels.subscribe,
            vec!["did:plc:added".to_string()]
        );
    }

    fn session(dir: &std::path::Path) -> DevSession {
        let secrets = crate::dev::secrets::load_or_create(&dir.join(".happyview")).unwrap();
        DevSession {
            root: dir.to_path_buf(),
            public_url: String::new(),
            secrets,
            watch_mode: crate::dev::watch::WatchMode::Native,
        }
    }

    async fn subscribed(state: &AppState, did: &str) -> bool {
        crate::db::query_as::<(String,)>(
            "SELECT did FROM happyview_labeler_subscriptions WHERE did = ?",
        )
        .bind(did)
        .fetch_optional(&state.db)
        .await
        .unwrap()
        .is_some()
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn a_parse_error_names_the_line_and_changes_nothing() {
        let _reset = crate::config::vars::LayerReset;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("happyview.toml"), "[labels\n").unwrap();
        let session = session(dir.path());
        let state = crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        );
        let port = state.config.port;
        let mut before = None;

        let err = reload(&state, &session, &mut before).await.unwrap_err();

        assert!(err.contains("happyview.toml:1"), "got: {err}");
        assert!(before.is_none());
        assert_eq!(state.config.port, port);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn a_removed_declaration_deletes_nothing() {
        let _reset = crate::config::vars::LayerReset;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("happyview.toml");
        std::fs::write(&file, "[labels]\nsubscribe = [\"did:plc:gone\"]\n").unwrap();
        let session = session(dir.path());
        let state = crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        );
        let mut before = None;
        reload(&state, &session, &mut before).await.unwrap();
        assert!(subscribed(&state, "did:plc:gone").await);

        std::fs::write(&file, "").unwrap();
        reload(&state, &session, &mut before).await.unwrap();

        assert!(subscribed(&state, "did:plc:gone").await);
    }

    #[test]
    fn a_startup_only_change_prints_the_restart_line() {
        let running = base();
        let mut edited = running.clone();
        edited.public_url = "http://127.0.0.1:9".into();
        assert_eq!(
            notices(&running, &edited, None),
            vec!["restart required to apply: PUBLIC_URL".to_string()]
        );
    }
}
