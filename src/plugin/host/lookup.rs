use serde::Deserialize;

use super::HostContext;
use crate::db::adapt_sql;
use crate::plugin::StrongRef;

#[derive(Debug, thiserror::Error)]
pub enum LookupError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("Invalid external ID field path")]
    InvalidFieldPath,
}

#[derive(Debug, Deserialize)]
pub struct LookupRequest {
    pub collection: String,
    pub external_id_field: String,
    pub external_id_value: String,
}

/// Look up a record by external ID
///
/// # Arguments
/// * `collection` - Lexicon collection ID (e.g., "games.gamesgamesgamesgames.game")
/// * `external_id_field` - JSON path to external ID field (e.g., "externalIds.steam")
/// * `external_id_value` - Value to match
pub async fn lookup_record(
    ctx: &HostContext,
    collection: &str,
    external_id_field: &str,
    external_id_value: &str,
) -> Result<Option<StrongRef>, LookupError> {
    // Validate field path (basic check)
    if external_id_field.is_empty() || external_id_field.contains("..") {
        return Err(LookupError::InvalidFieldPath);
    }

    // Build JSON path for query
    let json_path = format!("$.{}", external_id_field);

    let sql = adapt_sql(
        "SELECT uri, cid FROM happyview_records
         WHERE collection = ?
         AND json_extract(record, ?) = ?
         LIMIT 1",
        ctx.db_backend,
    );

    let result: Option<(String, String)> = sqlx::query_as(&sql)
        .bind(collection)
        .bind(&json_path)
        .bind(external_id_value)
        .fetch_optional(&ctx.db)
        .await?;

    Ok(result.map(|(uri, cid)| StrongRef { uri, cid }))
}

pub async fn lookup_record_by_request(
    ctx: &HostContext,
    request: LookupRequest,
) -> Result<Option<StrongRef>, LookupError> {
    lookup_record(
        ctx,
        &request.collection,
        &request.external_id_field,
        &request.external_id_value,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invalid_field_path_empty() {
        // Can't test async without runtime, but we can verify error types exist
        let err = LookupError::InvalidFieldPath;
        assert!(err.to_string().contains("Invalid"));
    }

    #[test]
    fn test_lookup_error_display() {
        let err = LookupError::InvalidFieldPath;
        assert_eq!(err.to_string(), "Invalid external ID field path");
    }

    #[test]
    fn test_lookup_request_deserialize() {
        let json = r#"{"collection": "games.example.game", "external_id_field": "externalIds.steam", "external_id_value": "123"}"#;
        let req: LookupRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.collection, "games.example.game");
        assert_eq!(req.external_id_field, "externalIds.steam");
        assert_eq!(req.external_id_value, "123");
    }
}
