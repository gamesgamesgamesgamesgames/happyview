//! Operator view of where permissioned repos live and why.
//!
//! Migration runs in the background, and its most common outcome, waiting for a
//! user to sign in again, looks the same as nothing happening. Reporting it
//! separately keeps it from being mistaken for a failure, and keeps real
//! failures from being overlooked.

use axum::Json;
use axum::extract::State;

use crate::AppState;
use crate::db::adapt_sql;
use crate::error::AppError;

use super::auth::UserAuth;
use super::permissions::Permission;

/// Per-space repo counts by host mode, plus what each PDS was last found to be.
pub async fn migration_status(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::SpacesRead).await?;

    let sql = adapt_sql(
        "SELECT s.id, s.type_nsid, s.skey, r.host_mode, COUNT(*) \
         FROM happyview_space_repo_state r \
         JOIN happyview_spaces s ON s.id = r.space_id \
         GROUP BY s.id, s.type_nsid, s.skey, r.host_mode",
        state.db_backend,
    );
    let rows: Vec<(String, String, String, String, i64)> = crate::db::query_as(&sql)
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to load repo host modes: {e}")))?;

    let mut spaces: std::collections::BTreeMap<String, serde_json::Value> =
        std::collections::BTreeMap::new();
    for (space_id, type_nsid, skey, mode, count) in rows {
        let entry = spaces.entry(space_id.clone()).or_insert_with(|| {
            serde_json::json!({
                "spaceId": space_id,
                "type": type_nsid,
                "skey": skey,
                "polyfill": 0,
                "migrating": 0,
                "native": 0,
            })
        });
        entry[mode.as_str()] = serde_json::json!(count);
    }

    // Accounts with polyfill repos and no session carrying `space:` scope. They
    // are waiting on the user; an operator cannot act on them.
    let awaiting = count_awaiting_authorization(&state).await?;

    let sql = adapt_sql(
        "SELECT pds_endpoint, supported, tier, missing, checked_at \
         FROM happyview_space_pds_support ORDER BY pds_endpoint",
        state.db_backend,
    );
    let detection: Vec<(String, i32, String, String, String)> = crate::db::query_as(&sql)
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to load pds support cache: {e}")))?;

    let detection: Vec<serde_json::Value> = detection
        .into_iter()
        .map(|(endpoint, supported, tier, missing, checked_at)| {
            serde_json::json!({
                "pds": endpoint,
                "supported": supported != 0,
                "tier": tier,
                "missing": serde_json::from_str::<Vec<String>>(&missing).unwrap_or_default(),
                "checkedAt": checked_at,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "spaces": spaces.into_values().collect::<Vec<_>>(),
        "awaitingAuthorization": awaiting,
        "detection": detection,
    })))
}

/// Accounts that own polyfill repos but hold no session granting `space:`.
async fn count_awaiting_authorization(state: &AppState) -> Result<i64, AppError> {
    let sql = adapt_sql(
        "SELECT DISTINCT author_did FROM happyview_space_repo_state WHERE host_mode = 'polyfill'",
        state.db_backend,
    );
    let dids: Vec<(String,)> = crate::db::query_as(&sql)
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list polyfill authors: {e}")))?;

    let mut awaiting = 0i64;
    for (did,) in dids {
        let scope = crate::jobs::native::migrate_space_repo::granted_scope(state, &did).await?;
        let has_space_grant = scope
            .as_deref()
            .is_some_and(|s| s.split_whitespace().any(|t| t.starts_with("space:")));
        if !has_space_grant {
            awaiting += 1;
        }
    }
    Ok(awaiting)
}
