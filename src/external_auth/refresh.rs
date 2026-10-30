//! On-demand refresh of a linked external account's tokens.
//!
//! Tokens are stored once at link time. `ensure_fresh_tokens` returns the
//! stored set while it is usable and otherwise exchanges the refresh token
//! through the auth plugin's `refresh_tokens` export and stores the result.
//!
//! There is no single-flight guard. The only caller is the refresh route,
//! driven by one user clicking one button, so two refreshes of the same link
//! cannot overlap. A script surface calling this on every request would need
//! a per-link lock.

use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};

use crate::AppState;
use crate::event_log::{EventLog, Severity, log_event};
use crate::external_auth::tokens::{self, StoredTokens, TokenError};
use crate::plugin::secrets::load_plugin_secrets;
use crate::plugin::{ExecutionError, TokenSetExt};

/// A token is treated as expired this long before its `expires_at`, so a
/// caller handed a "fresh" token has time to use it before the provider
/// rejects it.
pub const REFRESH_SKEW_SECS: i64 = 60;

/// Whether a stored token should be exchanged before use.
///
/// A token with no expiry is never refreshed: providers like Steam and itch
/// issue long-lived credentials with nothing to exchange them for. An expiry
/// that cannot be parsed is treated as already passed, because refreshing a
/// good token costs one request while trusting a bad one costs a failed call
/// somewhere far from the cause.
pub fn needs_refresh(expires_at: Option<&str>, now: DateTime<Utc>) -> bool {
    let Some(raw) = expires_at else {
        return false;
    };
    match DateTime::parse_from_rfc3339(raw) {
        Ok(at) => at.with_timezone(&Utc) <= now + Duration::seconds(REFRESH_SKEW_SECS),
        Err(err) => {
            tracing::warn!(expires_at = raw, "unparseable token expiry: {err}");
            true
        }
    }
}

#[derive(Debug)]
pub enum RefreshOutcome {
    Fresh(StoredTokens),
    /// Re-read after storing, so the caller sees what a later read would.
    Refreshed(StoredTokens),
}

impl RefreshOutcome {
    pub fn tokens(&self) -> &StoredTokens {
        match self {
            RefreshOutcome::Fresh(t) | RefreshOutcome::Refreshed(t) => t,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    #[error("no linked account")]
    NotLinked,
    /// The token has expired and the provider issued nothing to exchange, so
    /// only relinking the account can produce a usable token.
    #[error("token expired and the provider issued no refresh token; relink the account")]
    NoRefreshToken,
    #[error("plugin refresh failed: {0}")]
    Plugin(#[from] ExecutionError),
    #[error("token storage failed: {0}")]
    Token(TokenError),
}

impl From<TokenError> for RefreshError {
    fn from(err: TokenError) -> Self {
        match err {
            TokenError::NotFound => RefreshError::NotLinked,
            other => RefreshError::Token(other),
        }
    }
}

/// Return usable tokens for `did`'s link to `plugin_id`, refreshing them
/// through the plugin when the stored set is at or past its expiry.
pub async fn ensure_fresh_tokens(
    app_state: &AppState,
    did: &str,
    plugin_id: &str,
) -> Result<RefreshOutcome, RefreshError> {
    let encryption_key = app_state.config.token_encryption_key.as_ref();
    let stored = tokens::get_tokens(
        &app_state.db,
        app_state.db_backend,
        encryption_key,
        did,
        plugin_id,
    )
    .await?;

    if !needs_refresh(stored.expires_at.as_deref(), Utc::now()) {
        return Ok(RefreshOutcome::Fresh(stored));
    }

    let Some(refresh_token) = stored.refresh_token.as_deref() else {
        log_refresh(app_state, did, plugin_id, Err("no refresh token")).await;
        return Err(RefreshError::NoRefreshToken);
    };

    match refresh_through_plugin(app_state, did, plugin_id, &stored, refresh_token).await {
        Ok(()) => {}
        Err(err) => {
            log_refresh(app_state, did, plugin_id, Err(&err.to_string())).await;
            return Err(err);
        }
    }

    let refreshed = tokens::get_tokens(
        &app_state.db,
        app_state.db_backend,
        encryption_key,
        did,
        plugin_id,
    )
    .await?;
    log_refresh(
        app_state,
        did,
        plugin_id,
        Ok(refreshed.expires_at.as_deref()),
    )
    .await;
    Ok(RefreshOutcome::Refreshed(refreshed))
}

async fn refresh_through_plugin(
    app_state: &AppState,
    did: &str,
    plugin_id: &str,
    stored: &StoredTokens,
    refresh_token: &str,
) -> Result<(), RefreshError> {
    // Per-account plugin config is not stored with the link, so a refresh runs
    // with the same null config the OAuth link flow used.
    let config = Value::Null;
    let secrets = load_plugin_secrets(
        &app_state.db,
        app_state.db_backend,
        app_state.config.token_encryption_key.as_ref(),
        plugin_id,
    )
    .await;

    let mut instance = app_state
        .plugin_executor()
        .instantiate(plugin_id, did, secrets, config.clone())
        .await?;
    let token_set = instance.call_refresh_tokens(refresh_token, &config).await?;

    let expires_at = token_set
        .resolved_expires_at()
        .map_err(|e| ExecutionError::InvalidResponse(format!("expires_at is not RFC 3339: {e}")))?
        .map(|dt| dt.to_rfc3339());

    // Providers commonly omit the refresh token from a refresh response when
    // the old one stays valid; dropping it would make the next refresh
    // impossible.
    let next_refresh_token = token_set
        .refresh_token
        .as_deref()
        .or(stored.refresh_token.as_deref());

    tokens::store_tokens(
        &app_state.db,
        app_state.db_backend,
        app_state.config.token_encryption_key.as_ref(),
        did,
        plugin_id,
        &stored.account_id,
        &token_set.access_token,
        next_refresh_token,
        Some(&token_set.token_type),
        stored.scope.as_deref(),
        expires_at.as_deref(),
    )
    .await?;

    Ok(())
}

async fn log_refresh(
    app_state: &AppState,
    did: &str,
    plugin_id: &str,
    result: Result<Option<&str>, &str>,
) {
    let (event_type, severity, detail) = match result {
        Ok(expires_at) => (
            "external_auth.refreshed",
            Severity::Info,
            json!({ "expires_at": expires_at }),
        ),
        Err(reason) => (
            "external_auth.refresh_failed",
            Severity::Warn,
            json!({ "reason": reason }),
        ),
    };
    log_event(
        &app_state.db,
        EventLog {
            event_type: event_type.to_string(),
            severity,
            actor_did: Some(did.to_string()),
            subject: Some(plugin_id.to_string()),
            detail,
        },
        app_state.db_backend,
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-01-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn no_expiry_never_refreshes() {
        assert!(!needs_refresh(None, now()));
    }

    #[test]
    fn far_future_expiry_is_fresh() {
        assert!(!needs_refresh(Some("2026-01-01T13:00:00Z"), now()));
    }

    #[test]
    fn expiry_inside_the_skew_window_refreshes() {
        assert!(needs_refresh(Some("2026-01-01T12:00:30Z"), now()));
    }

    #[test]
    fn expiry_just_outside_the_skew_window_is_fresh() {
        assert!(!needs_refresh(Some("2026-01-01T12:01:01Z"), now()));
    }

    #[test]
    fn past_expiry_refreshes() {
        assert!(needs_refresh(Some("2025-12-31T12:00:00Z"), now()));
    }

    #[test]
    fn offset_timestamps_are_compared_as_instants() {
        assert!(!needs_refresh(Some("2026-01-01T14:00:00+01:00"), now()));
        assert!(needs_refresh(Some("2026-01-01T12:00:00+01:00"), now()));
    }

    #[test]
    fn unparseable_expiry_refreshes() {
        assert!(needs_refresh(Some("soon"), now()));
    }
}
