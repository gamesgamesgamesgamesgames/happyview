use atrium_api::types::string::Did;

use crate::AppState;
use crate::error::AppError;

use super::db;
use super::flow;
use super::types::{LinkedRepo, STATUS_ACTIVE};

/// How often the loop wakes.
const TICK_SECS: u64 = 30 * 60;
/// Only touch grants that have not been refreshed within this window.
const STALE_AFTER_SECS: i64 = 60 * 60;

pub async fn refresh_grant(state: &AppState, grant: &LinkedRepo) -> Result<(), AppError> {
    let Some(ref did_str) = grant.did else {
        return Ok(());
    };

    let did = Did::new(did_str.clone())
        .map_err(|_| AppError::Internal(format!("invalid DID on grant: {did_str}")))?;

    let client = match flow::client_for_grant(state, grant).await {
        Ok(client) => client,
        Err(e) => {
            let message = format!("{e}");
            db::mark_needs_reauth(state, &grant.id, &message).await?;
            tracing::warn!(
                grant_id = %grant.id,
                did = %did_str,
                error = %message,
                "linked repo client construction failed"
            );
            return Err(e);
        }
    };

    match client.restore(&did).await {
        Ok(_session) => {
            db::touch_refreshed(state, &grant.id).await?;
            Ok(())
        }
        Err(e) => {
            let message = format!("{e}");
            db::mark_needs_reauth(state, &grant.id, &message).await?;
            tracing::warn!(
                grant_id = %grant.id,
                did = %did_str,
                error = %message,
                "linked repo session refresh failed"
            );
            Err(AppError::Auth(format!(
                "linked repo {did_str} needs reauthorization: {message}"
            )))
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

            let _ = refresh_grant(&state, &grant).await;
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
