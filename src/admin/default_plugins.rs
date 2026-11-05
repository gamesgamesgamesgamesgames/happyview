//! `HAPPYVIEW_DEFAULT_PLUGINS`: plugins an operator wants every new instance
//! to start with, installed on the first boot that lists them.
//!
//! Each source is applied once, ever. What was applied is recorded under
//! [`APPLIED_KEY`], rather than checked against what is installed now, so a
//! plugin the owner uninstalls stays uninstalled. A source that fails is not
//! recorded, so the next boot tries it again.
//!
//! Naming a source in the variable is the operator's consent to the
//! capabilities its manifest declares ([`Consent::Operator`]).

use std::time::Duration;

use crate::AppState;
use crate::admin::settings::{put_setting, try_get_setting};

use super::plugins::{Consent, Install, Installed, Origin, install};

/// The `instance_settings` key holding the sources already applied, as a
/// JSON list of strings.
pub const APPLIED_KEY: &str = "default_plugins.applied";

/// How long one source may take to fetch and install before it is given up
/// on until the next boot.
const PER_SOURCE_TIMEOUT: Duration = Duration::from_secs(120);

/// Apply the configured default plugins in the background, so a slow or
/// unreachable source never holds up boot or the HTTP server.
pub fn spawn(state: &AppState) {
    if state.config.default_plugins.is_empty() {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move { apply(&state).await });
}

/// Install each configured source not yet applied, recording each one that
/// installs or whose plugin is already installed. Failures are logged and
/// leave the source to the next boot; nothing here returns an error.
pub async fn apply(state: &AppState) {
    let sources = &state.config.default_plugins;
    if sources.is_empty() {
        return;
    }
    let mut applied: Vec<String> = match try_get_setting(&state.db, APPLIED_KEY, state.db_backend)
        .await
    {
        Ok(None) => Vec::new(),
        Ok(Some(value)) => match serde_json::from_str(&value) {
            Ok(applied) => applied,
            // Guessing would reinstall plugins the owner removed.
            Err(e) => {
                tracing::warn!(key = APPLIED_KEY, error = %e, "the applied default plugins are unreadable; installing none");
                return;
            }
        },
        Err(e) => {
            tracing::warn!(key = APPLIED_KEY, error = %e, "could not read the applied default plugins; installing none");
            return;
        }
    };

    for source in sources {
        if applied.contains(source) {
            continue;
        }
        let attempt = tokio::time::timeout(
            PER_SOURCE_TIMEOUT,
            install(
                state,
                Install {
                    origin: Origin::Url(source),
                    sha256: None,
                    consent: Consent::Operator,
                    actor_did: None,
                    if_absent: true,
                },
            ),
        )
        .await;
        match attempt {
            Ok(Ok(Installed::Added(summary))) => {
                tracing::info!(source = %source, plugin = %summary.id, "installed a default plugin");
            }
            Ok(Ok(Installed::AlreadyInstalled(id))) => {
                tracing::info!(source = %source, plugin = %id, "a default plugin is already installed; marking it applied");
            }
            Ok(Err(e)) => {
                tracing::warn!(source = %source, error = %e, "a default plugin did not install; retrying on the next boot");
                continue;
            }
            Err(_) => {
                tracing::warn!(source = %source, timeout_secs = PER_SOURCE_TIMEOUT.as_secs(), "a default plugin timed out; retrying on the next boot");
                continue;
            }
        }
        applied.push(source.clone());
        let value = serde_json::to_string(&applied).expect("a list of strings serializes");
        if let Err(e) = put_setting(&state.db, APPLIED_KEY, &value, state.db_backend).await {
            // The plugin is installed, so the next boot finds it and marks
            // it applied then.
            tracing::warn!(source = %source, error = %e, "a default plugin installed but was not recorded as applied");
        }
    }
}
