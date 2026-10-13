//! What an edit to `happyview.toml` means for a running dev server.

use std::sync::Arc;

use crate::AppState;
use crate::config::Config;
use crate::config::project::ProjectConfig;
use crate::dev::DevSession;

/// Settings read once at startup. `state.config` is the boot copy, so an
/// edit to one of these cannot take effect under a running server.
pub fn restart_required(running: &Config, edited: &Config) -> Vec<&'static str> {
    let mut out = Vec::new();
    if running.host != edited.host {
        out.push("HOST");
    }
    if running.port != edited.port {
        out.push("PORT");
    }
    if running.database_url != edited.database_url {
        out.push("DATABASE_URL");
    }
    if running.public_url != edited.public_url {
        out.push("PUBLIC_URL");
    }
    if running.base_path != edited.base_path {
        out.push("BASE_PATH");
    }
    if running.auto_backfill != edited.auto_backfill {
        out.push("HAPPYVIEW_DEV_BACKFILL");
    }
    if running.plugin_dir != edited.plugin_dir {
        out.push("PLUGIN_DIR");
    }
    out
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

    let restart = restart_required(&state.config, &edited);
    if !restart.is_empty() {
        println!("restart required to apply: {}", restart.join(", "));
    }

    let empty = ProjectConfig::default();
    let dropped = dropped_declarations(
        before.as_deref().unwrap_or(&empty),
        edited.project.as_deref().unwrap_or(&empty),
    );
    if !dropped.is_empty() {
        println!(
            "no longer declared in happyview.toml: {}; they stay until you delete them in the dashboard, which a restart allows",
            dropped.join(", ")
        );
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
        assert_eq!(
            restart_required(&running, &edited),
            vec!["HAPPYVIEW_DEV_BACKFILL", "PLUGIN_DIR"]
        );
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
}
