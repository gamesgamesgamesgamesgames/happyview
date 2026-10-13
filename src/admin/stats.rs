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
        .map_err(|e| AppError::Internal(format!("failed to count records: {e}")))?;

    // Counted per collection and merged here rather than joined in SQL, so
    // the collection list is the catalog's: every layer, not only stored rows.
    let counts: HashMap<String, i64> = state
        .records
        .count_by_collection()
        .await
        .map_err(|e| AppError::Internal(format!("failed to count by collection: {e}")))?
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
