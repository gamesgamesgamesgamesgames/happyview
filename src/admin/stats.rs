use std::collections::HashMap;

use axum::Json;
use axum::extract::State;

use crate::AppState;
use crate::db::adapt_sql;
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

    let lexicon_sql = adapt_sql(
        "SELECT id FROM happyview_lexicons \
         WHERE json_extract(lexicon_json, '$.defs.main.type') = 'record' ORDER BY id",
        state.db_backend,
    );
    let record_lexicons: Vec<(String,)> = crate::db::query_as(&lexicon_sql)
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to count by collection: {e}")))?;
    let counts: HashMap<String, i64> = state
        .records
        .count_by_collection()
        .await
        .map_err(|e| AppError::Internal(format!("failed to count by collection: {e}")))?
        .into_iter()
        .collect();

    Ok(Json(StatsResponse {
        total_records: total,
        collections: record_lexicons
            .into_iter()
            .map(|(collection,)| {
                let count = counts.get(&collection).copied().unwrap_or(0);
                CollectionStat { collection, count }
            })
            .collect(),
    }))
}
