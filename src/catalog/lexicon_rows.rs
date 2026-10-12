//! Every statement that names the lexicons table. Nothing outside
//! `src/catalog/` may name it; `catalog::tests` enforces that.

use sqlx::AnyPool;

pub const TABLE: &str = "happyview_lexicons";

/// The columns the catalog keeps in memory for a stored row.
pub(crate) struct LoadedRow {
    pub id: String,
    pub lexicon_json: String,
    pub revision: i32,
    pub target_collection: Option<String>,
    pub action: Option<String>,
    pub token_cost: Option<i32>,
    pub source: String,
    pub authority_did: Option<String>,
}

#[allow(clippy::type_complexity)]
pub(crate) async fn load_all(db: &AnyPool) -> Result<Vec<LoadedRow>, sqlx::Error> {
    let rows: Vec<(
        String,
        String,
        i32,
        Option<String>,
        Option<String>,
        Option<i32>,
        String,
        Option<String>,
    )> = crate::db::query_as(
        "SELECT id, lexicon_json, revision, target_collection, action, token_cost, source, authority_did FROM happyview_lexicons",
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(
                id,
                lexicon_json,
                revision,
                target_collection,
                action,
                token_cost,
                source,
                authority_did,
            )| {
                LoadedRow {
                    id,
                    lexicon_json,
                    revision,
                    target_collection,
                    action,
                    token_cost,
                    source,
                    authority_did,
                }
            },
        )
        .collect())
}
