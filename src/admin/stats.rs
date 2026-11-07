use std::collections::HashMap;

use axum::Json;
use axum::extract::State;

use crate::AppState;
use crate::db::adapt_sql;
use crate::error::AppError;
use crate::record_store::CountError;

use super::auth::AllowPlatform;
use super::permissions::Permission;
use super::types::{CollectionStat, CountsResponse, StatsResponse};

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
        .get_record_lexicon_ids()
        .await
        .into_iter()
        .map(|collection| CollectionStat {
            count: counts.get(&collection).copied().unwrap_or(0),
            indexed: state.lexicons.is_indexed(&collection),
            collection,
        })
        .collect();

    Ok(Json(StatsResponse {
        total_records: total,
        collections,
    }))
}

/// GET /admin/stats/counts — admin user and API client counts. Cheap enough
/// to poll, unlike `stats`, which counts every record.
pub(super) async fn counts(
    State(state): State<AppState>,
    AllowPlatform(auth): AllowPlatform,
) -> Result<Json<CountsResponse>, AppError> {
    auth.require(Permission::StatsRead).await?;

    let count = |table: &'static str| {
        let state = state.clone();
        async move {
            let sql = adapt_sql(&format!("SELECT COUNT(*) FROM {table}"), state.db_backend);
            let (n,): (i64,) = crate::db::query_as(&sql)
                .fetch_one(&state.db)
                .await
                .map_err(|e| AppError::Internal(format!("failed to count {table}: {e}")))?;
            Ok::<i64, AppError>(n)
        }
    };

    Ok(Json(CountsResponse {
        users: count("happyview_users").await?,
        api_clients: count("happyview_api_clients").await?,
    }))
}

/// A failed count: 503 when it was a claimed collection's count and the
/// storage that claims it is unreachable, as a read of it answers; a
/// failure of this instance's own tables stays a 500.
fn count_error(state: &AppState, what: &str, e: CountError) -> AppError {
    let storage_down = e
        .claimed
        .as_deref()
        .and_then(|collection| state.records.external_route(collection))
        .is_some_and(|ext| ext.unavailable(&e.error));
    if storage_down {
        crate::error::storage_unavailable()
    } else {
        AppError::Internal(format!("failed to {what}: {e}"))
    }
}
