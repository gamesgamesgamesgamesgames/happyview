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
    // The path is interpolated, not bound: `adapt_sql` rewrites `json_extract`
    // into a Postgres arrow chain by reading the path out of the statement, so
    // a bound one leaves the SQLite function name in a statement Postgres has
    // no such function for. Interpolating is safe only because the validator
    // admits nothing but identifiers and decimal subscripts.
    if !crate::db::is_valid_json_field_path(external_id_field) {
        return Err(LookupError::InvalidFieldPath);
    }

    let sql = adapt_sql(
        &format!(
            "SELECT uri, cid FROM happyview_records
         WHERE collection = ?
         AND json_extract(record, '$.{external_id_field}') = ?
         LIMIT 1"
        ),
        ctx.db_backend,
    );

    let result: Option<(String, String)> = crate::db::query_as(&sql)
        .bind(collection)
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

    use crate::db::DatabaseBackend;
    use serial_test::serial;

    const TEST_COLLECTION: &str = "lookup.test.game";

    async fn context_on(db: sqlx::AnyPool, backend: DatabaseBackend) -> HostContext {
        HostContext {
            plugin_id: "lookup-test".to_string(),
            scope: "did:plc:test".to_string(),
            secrets: Default::default(),
            config: serde_json::Value::Null,
            db,
            db_backend: backend,
            http_client: reqwest::Client::new(),
            lexicons: std::sync::Arc::new(crate::lexicon::LexiconRegistry::new()),
        }
    }

    /// One record carrying a nested external id, which is the shape the
    /// import exists for.
    async fn seed_record(db: &sqlx::AnyPool, backend: DatabaseBackend) {
        let sql = adapt_sql(
            "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            backend,
        );
        crate::db::query(&sql)
            .bind("at://did:plc:author/lookup.test.game/1")
            .bind("did:plc:author")
            .bind(TEST_COLLECTION)
            .bind("1")
            .bind(r#"{"externalIds":{"steam":"440"}}"#)
            .bind("bafytest")
            .bind("2026-01-01T00:00:00+00:00")
            .execute(db)
            .await
            .expect("seed a record");
    }

    async fn forget_record(db: &sqlx::AnyPool, backend: DatabaseBackend) {
        let sql = adapt_sql(
            "DELETE FROM happyview_records WHERE collection = ?",
            backend,
        );
        crate::db::query(&sql)
            .bind(TEST_COLLECTION)
            .execute(db)
            .await
            .expect("clear the seeded record");
    }

    /// The SQLite half, which is the control rather than the proof: the
    /// statement is well-typed there whether the path is bound or
    /// interpolated, so this passes with the fault present.
    #[tokio::test]
    async fn a_record_is_found_by_a_nested_external_id_on_sqlite() {
        let db = crate::test_support::migrated_memory_pool().await;
        let backend = DatabaseBackend::Sqlite;
        seed_record(&db, backend).await;
        let ctx = context_on(db, backend).await;

        let hit = lookup_record(&ctx, TEST_COLLECTION, "externalIds.steam", "440")
            .await
            .expect("the lookup should run");
        assert_eq!(
            hit.map(|r| r.uri).as_deref(),
            Some("at://did:plc:author/lookup.test.game/1")
        );

        let miss = lookup_record(&ctx, TEST_COLLECTION, "externalIds.steam", "441")
            .await
            .expect("the lookup should run");
        assert!(miss.is_none(), "a value that matches nothing finds nothing");
    }

    /// Postgres, which is what this pins. `adapt_sql` rewrites `json_extract`
    /// into an arrow chain by reading the path out of the statement, so a
    /// bound path leaves the SQLite function name in the statement and every
    /// lookup fails with an undefined function — not a miss, an error, on a
    /// backend the SQLite test cannot speak for.
    ///
    /// The skip below is invisible under libtest's captured output, so a run
    /// with no `TEST_DATABASE_URL` reports `ok` having proved nothing.
    #[tokio::test]
    #[serial]
    async fn a_record_is_found_by_a_nested_external_id_on_postgres() {
        if std::env::var("TEST_DATABASE_URL").is_err() {
            eprintln!("skipped (TEST_DATABASE_URL not set)");
            return;
        }
        let url = std::env::var("TEST_DATABASE_URL").expect("the caller checks it first");
        let backend = DatabaseBackend::from_url(&url);
        let db = crate::db::connect(&url, backend).await;

        forget_record(&db, backend).await;
        seed_record(&db, backend).await;
        let ctx = context_on(db.clone(), backend).await;

        let hit = lookup_record(&ctx, TEST_COLLECTION, "externalIds.steam", "440")
            .await
            .expect("the lookup should run");
        assert_eq!(
            hit.map(|r| r.uri).as_deref(),
            Some("at://did:plc:author/lookup.test.game/1")
        );

        let miss = lookup_record(&ctx, TEST_COLLECTION, "externalIds.steam", "441")
            .await
            .expect("the lookup should run");
        assert!(miss.is_none(), "a value that matches nothing finds nothing");

        forget_record(&db, backend).await;
    }

    /// A path outside the validator's charset is refused before it reaches a
    /// statement, which is what makes interpolating it safe.
    #[tokio::test]
    async fn a_path_the_validator_refuses_never_reaches_the_database() {
        let db = crate::test_support::migrated_memory_pool().await;
        let ctx = context_on(db, DatabaseBackend::Sqlite).await;

        for path in ["", "externalIds..steam", "externalIds.steam'", "a[]"] {
            let err = lookup_record(&ctx, TEST_COLLECTION, path, "440")
                .await
                .expect_err("the path should be refused");
            assert!(
                matches!(err, LookupError::InvalidFieldPath),
                "{path:?} should be an invalid path, got {err:?}"
            );
        }
    }

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
