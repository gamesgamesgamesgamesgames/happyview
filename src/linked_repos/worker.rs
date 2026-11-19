use atrium_api::types::string::Did;

use crate::AppState;
use crate::error::AppError;

use super::db;
use super::flow;
use super::refresh::{self, RefreshFailure};
use super::types::{LinkedRepo, STATUS_ACTIVE};

/// How often the loop wakes.
const TICK_SECS: u64 = 30 * 60;
/// Only touch grants that have not been refreshed within this window.
const STALE_AFTER_SECS: i64 = 60 * 60;
/// The longest the keep-alive waits to retry a grant after transient failures.
const MAX_BACKOFF_SECS: u64 = 8 * 60 * 60;

/// What one keep-alive attempt did to a grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// The session restored; the grant's refresh time moved on.
    Refreshed,
    /// The grant has no account bound yet, so there is nothing to refresh.
    Unbound,
    /// The authorization server refused the grant; it now needs
    /// reauthorization.
    NeedsReauth,
    /// The server or the network failed. The grant stays active with the
    /// error recorded, and the keep-alive retries later.
    Transient,
}

/// Restore a grant's session, and settle what a failure means for it. See
/// [`refresh::classify`].
pub async fn refresh_grant(
    state: &AppState,
    grant: &LinkedRepo,
) -> Result<RefreshOutcome, AppError> {
    let Some(ref did_str) = grant.did else {
        return Ok(RefreshOutcome::Unbound);
    };

    let did = Did::new(did_str.clone())
        .map_err(|_| AppError::Internal(format!("invalid DID on grant: {did_str}")))?;

    let failure = match flow::client_for_grant(state, grant).await {
        // The client refuses only when the key the session was made with is
        // no longer held; anything else is this instance failing to build it.
        Err(e @ AppError::Auth(_)) => Some((format!("{e}"), RefreshFailure::Refused)),
        Err(e) => Some((format!("{e}"), RefreshFailure::Transient)),
        Ok(client) => match client.restore(&did).await {
            Ok(_session) => None,
            Err(e) => {
                let message = format!("{e}");
                let kind = refresh::classify(&message);
                Some((message, kind))
            }
        },
    };

    let Some((message, kind)) = failure else {
        db::touch_refreshed(state, &grant.id).await?;
        return Ok(RefreshOutcome::Refreshed);
    };

    match kind {
        RefreshFailure::Refused => {
            db::mark_needs_reauth(state, &grant.id, &message).await?;
            tracing::warn!(
                grant_id = %grant.id,
                did = %did_str,
                error = %message,
                "linked repo session refused; it needs reauthorization"
            );
            Ok(RefreshOutcome::NeedsReauth)
        }
        RefreshFailure::Transient => {
            db::record_error(state, &grant.id, &message).await?;
            tracing::warn!(
                grant_id = %grant.id,
                did = %did_str,
                error = %message,
                "linked repo session refresh failed; retrying later"
            );
            Ok(RefreshOutcome::Transient)
        }
    }
}

fn is_stale(last_refreshed_at: Option<&str>, cutoff: chrono::DateTime<chrono::Utc>) -> bool {
    match last_refreshed_at {
        Some(ts) => chrono::DateTime::parse_from_rfc3339(ts)
            .map(|t| t.with_timezone(&chrono::Utc) < cutoff)
            .unwrap_or(true),
        None => true,
    }
}

pub async fn run_keepalive(state: AppState) {
    tracing::info!("starting linked repo keep-alive task");
    let interval = tokio::time::Duration::from_secs(TICK_SECS);

    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    // Grants whose last attempts failed transiently: how many in a row, and
    // when to try again. Kept in memory: a restart retries them at once.
    let mut retry: std::collections::HashMap<String, (u32, tokio::time::Instant)> =
        std::collections::HashMap::new();

    loop {
        let grants = match db::list(&state).await {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!(error = %e, "linked repo keep-alive could not list grants");
                tokio::time::sleep(interval).await;
                continue;
            }
        };

        let cutoff = chrono::Utc::now() - chrono::Duration::seconds(STALE_AFTER_SECS);

        for grant in grants {
            if grant.status != STATUS_ACTIVE {
                continue;
            }

            if !is_stale(grant.last_refreshed_at.as_deref(), cutoff) {
                continue;
            }

            if retry
                .get(&grant.id)
                .is_some_and(|(_, at)| tokio::time::Instant::now() < *at)
            {
                continue;
            }

            match refresh_grant(&state, &grant).await {
                Ok(RefreshOutcome::Transient) => {
                    let failures = retry.get(&grant.id).map_or(0, |(n, _)| *n) + 1;
                    let wait = refresh::backoff(
                        failures,
                        interval,
                        tokio::time::Duration::from_secs(MAX_BACKOFF_SECS),
                    );
                    retry.insert(
                        grant.id.clone(),
                        (failures, tokio::time::Instant::now() + wait),
                    );
                }
                Ok(_) => {
                    retry.remove(&grant.id);
                }
                Err(e) => {
                    tracing::warn!(grant_id = %grant.id, error = %e, "linked repo keep-alive failed");
                }
            }
        }

        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::is_stale;
    use chrono::{Duration, Utc};

    #[test]
    fn never_refreshed_is_stale() {
        let cutoff = Utc::now() - Duration::seconds(3600);
        assert!(is_stale(None, cutoff));
    }

    #[test]
    fn unparseable_timestamp_is_stale() {
        let cutoff = Utc::now() - Duration::seconds(3600);
        assert!(is_stale(Some("not-a-timestamp"), cutoff));
    }

    #[test]
    fn timestamp_older_than_cutoff_is_stale() {
        let cutoff = Utc::now() - Duration::seconds(3600);
        let old = (cutoff - Duration::seconds(60)).to_rfc3339();
        assert!(is_stale(Some(&old), cutoff));
    }

    #[test]
    fn timestamp_newer_than_cutoff_is_not_stale() {
        let cutoff = Utc::now() - Duration::seconds(3600);
        let recent = (cutoff + Duration::seconds(60)).to_rfc3339();
        assert!(!is_stale(Some(&recent), cutoff));
    }
}
