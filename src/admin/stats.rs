use std::collections::HashMap;

use axum::Json;
use axum::extract::State;

use crate::AppState;
use crate::error::AppError;

use super::auth::AllowPlatform;
use super::permissions::Permission;
use super::types::{CollectionStat, StatsResponse};

/// GET /admin/stats — system statistics.
pub(super) async fn stats(
    State(state): State<AppState>,
    AllowPlatform(auth): AllowPlatform,
) -> Result<Json<StatsResponse>, AppError> {
    auth.require(Permission::StatsRead).await?;
    let total = state
        .records
        .count_all()
        .await
        .map_err(|e| count_error(&state, "count records", e))?;

    // Counted per collection and merged here rather than joined in SQL, so
    // the collection list is the catalog's: every layer, not only stored rows.
    let counts: HashMap<String, i64> = state
        .records
        .count_by_collection()
        .await
        .map_err(|e| count_error(&state, "count by collection", e))?
        .into_iter()
        .collect();
    let collections: Vec<CollectionStat> = state
        .lexicons
        .get_record_collections()
        .await
        .into_iter()
        .map(|collection| CollectionStat {
            count: counts.get(&collection).copied().unwrap_or(0),
            collection,
        })
        .collect();

    Ok(Json(StatsResponse {
        total_records: total,
        collections,
    }))
}

/// A failed count: 503 when the external storage is unreachable, as a read
/// of a claimed collection answers, otherwise a 500. A count spans every
/// collection, so the failure is classified by the external storage rather
/// than by one collection's route.
fn count_error(state: &AppState, what: &str, e: sqlx::Error) -> AppError {
    let storage_down = state
        .records
        .external()
        .is_some_and(|ext| ext.unavailable(&e));
    if storage_down {
        crate::error::storage_unavailable()
    } else {
        AppError::Internal(format!("failed to {what}: {e}"))
    }
}
