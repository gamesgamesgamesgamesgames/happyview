//! `GET /admin/limits`: the plan's limits on this instance and how much of
//! each is in use, for the dashboard.

use axum::Json;
use axum::extract::State;
use happyview_provider::{Limit, Limits};
use serde_json::{Value, json};

use crate::AppState;
use crate::db::adapt_sql;
use crate::error::AppError;

use super::auth::UserAuth;

/// What each counted limit counts, in the instance database.
fn count_sql(limit: Limit) -> Option<&'static str> {
    match limit {
        // Every admin user is a seat, the super user included.
        Limit::Seats => Some("SELECT COUNT(*) FROM happyview_users"),
        // A client an admin created and has not deactivated. Child clients
        // belong to their parent and are not counted on their own.
        Limit::ApiClients => Some(
            "SELECT COUNT(*) FROM happyview_api_clients WHERE parent_client_id IS NULL AND is_active = 1",
        ),
        Limit::LogRetentionDays => None,
    }
}

/// How many of `limit` are in use, or `None` for a limit that caps a
/// setting rather than counting things.
pub async fn used(state: &AppState, limit: Limit) -> Result<Option<u32>, AppError> {
    let Some(sql) = count_sql(limit) else {
        return Ok(None);
    };
    let (n,): (i64,) = crate::db::query_as(&adapt_sql(sql, state.db_backend))
        .fetch_one(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to count {limit}: {e}")))?;
    Ok(Some(u32::try_from(n).unwrap_or(u32::MAX)))
}

/// `GET /admin/limits`
///
/// Any signed-in admin may read it: every admin can run into a limit, and
/// none of it is secret. A limit that is not set reads as `max: null`.
pub(super) async fn get(
    State(state): State<AppState>,
    _auth: UserAuth,
) -> Result<Json<Value>, AppError> {
    let limits: Limits = state.limits.limits();
    let configured_retention =
        crate::event_log::configured_retention_days(&state.db, state.db_backend).await;
    let mut out = serde_json::Map::new();
    for limit in Limit::ALL {
        let entry = match limit {
            Limit::LogRetentionDays => json!({
                "max": limits.get(limit),
                "configured": configured_retention,
            }),
            counted => json!({
                "max": limits.get(counted),
                "used": used(&state, counted).await?,
            }),
        };
        out.insert(limit.as_str().to_string(), entry);
    }
    Ok(Json(Value::Object(out)))
}
