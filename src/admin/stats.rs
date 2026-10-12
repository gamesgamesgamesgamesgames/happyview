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
    let total: (i64,) = crate::db::query_as("SELECT COUNT(*) FROM happyview_records")
        .fetch_one(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to count records: {e}")))?;

    // Counted per collection and merged here rather than joined in SQL, so
    // the collection list is the catalog's: every layer, not only stored rows.
    let counted: Vec<(String, i64)> = crate::db::query_as(
        "SELECT collection, COUNT(*) FROM happyview_records GROUP BY collection",
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("failed to count by collection: {e}")))?;
    let counts: std::collections::HashMap<String, i64> = counted.into_iter().collect();
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
        total_records: total.0,
        collections,
    }))
}
