use axum::Json;
use axum::extract::State;

use crate::AppState;
use crate::db::adapt_sql;
use crate::error::AppError;

use super::auth::UserAuth;
use super::permissions::Permission;
use super::types::{CollectionStat, StatsResponse};

/// GET /admin/stats — system statistics.
pub(super) async fn stats(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<StatsResponse>, AppError> {
    auth.require(Permission::StatsRead).await?;
    let total: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM happyview_records")
        .fetch_one(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to count records: {e}")))?;

    let collection_sql = adapt_sql(
        r#"
        SELECT c.collection, COALESCE(r.cnt, 0) AS count
        FROM (
            SELECT id AS collection FROM happyview_lexicons
            WHERE json_extract(lexicon_json, '$.defs.main.type') = 'record'
        ) c
        LEFT JOIN (
            SELECT collection, COUNT(*) AS cnt FROM happyview_records GROUP BY collection
        ) r ON r.collection = c.collection
        ORDER BY c.collection
        "#,
        state.db_backend,
    );

    let collections: Vec<(String, i64)> = sqlx::query_as(&collection_sql)
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to count by collection: {e}")))?;

    Ok(Json(StatsResponse {
        total_records: total.0,
        collections: collections
            .into_iter()
            .map(|(collection, count)| CollectionStat { collection, count })
            .collect(),
    }))
}
