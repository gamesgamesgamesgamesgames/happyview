use super::HostContext;
use crate::db::adapt_sql;
use crate::plugin::StrongRef;

/// Which record a plugin asked for. The SDK's type, as the guest built it.
pub use happyview_plugin_sdk::wire::LookupRequest;

#[derive(Debug, thiserror::Error)]
pub enum LookupError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("Invalid external ID field path")]
    InvalidFieldPath,
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
    // The path is interpolated, not bound: `adapt_sql` rewrites
    // `json_extract` into a Postgres JSON chain only where it can read the
    // path, and a bound one leaves the SQLite function name in the statement
    // for a backend that has no such function. Interpolating is safe because
    // the expression accepts only a validated path.
    let field = super::records::record_text_field_sql(external_id_field)
        .map_err(|_| LookupError::InvalidFieldPath)?;

    let sql = adapt_sql(
        &format!(
            "SELECT uri, cid FROM happyview_records
         WHERE collection = ?
         AND {field} = ?
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

    use crate::db::DatabaseBackend;
    use serde_json::json;
    use serial_test::serial;

    /// Every backend the suite can reach; the Postgres half needs a database
    /// to be pointed at and says so when there is none.
    async fn contexts() -> Vec<HostContext> {
        let mut out = vec![context_on(
            crate::test_support::migrated_memory_pool().await,
            DatabaseBackend::Sqlite,
        )];
        match std::env::var("TEST_DATABASE_URL") {
            Ok(url) => {
                let backend = DatabaseBackend::from_url(&url);
                out.push(context_on(crate::db::connect(&url, backend).await, backend));
            }
            Err(_) => eprintln!("TEST_DATABASE_URL unset: the Postgres half is skipped"),
        }
        out
    }

    fn context_on(db: sqlx::AnyPool, db_backend: DatabaseBackend) -> HostContext {
        HostContext {
            plugin_id: "lookup_test".to_string(),
            scope: "did:plc:test".to_string(),
            secrets: Default::default(),
            config: serde_json::Value::Null,
            db,
            db_backend,
            http_client: reqwest::Client::new(),
            lexicons: std::sync::Arc::new(crate::lexicon::LexiconRegistry::new()),
        }
    }

    const COLLECTION: &str = "host.lookup.test";

    async fn seed(ctx: &HostContext, record: serde_json::Value) {
        crate::db::query(&adapt_sql(
            "DELETE FROM happyview_records WHERE collection = ?",
            ctx.db_backend,
        ))
        .bind(COLLECTION)
        .execute(&ctx.db)
        .await
        .expect("clear the collection");
        crate::db::query(&adapt_sql(
            "INSERT INTO happyview_records \
             (uri, did, collection, rkey, record, cid, indexed_at, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, NULL, ?)",
            ctx.db_backend,
        ))
        .bind(format!("at://did:plc:seed/{COLLECTION}/1"))
        .bind("did:plc:seed")
        .bind(COLLECTION)
        .bind("1")
        .bind(record.to_string())
        .bind("bafylookup")
        .bind("2026-01-01T00:00:00+00:00")
        .execute(&ctx.db)
        .await
        .expect("seed a record");
    }

    /// The whole of the read, on every backend. Binding the JSON path leaves
    /// the statement naming `json_extract`, which `adapt_sql` cannot rewrite
    /// without reading the path and Postgres does not have, so every lookup
    /// fails there whatever the table holds. Both halves are asserted because
    /// only the pair tells a working predicate from an absent one.
    #[tokio::test]
    #[serial]
    async fn lookup_record_resolves_a_nested_external_id_on_every_backend() {
        for ctx in contexts().await {
            seed(&ctx, json!({"externalIds": {"steam": "440"}})).await;
            let found = lookup_record(&ctx, COLLECTION, "externalIds.steam", "440")
                .await
                .expect("the lookup should run")
                .expect("the seeded record");
            assert_eq!(found.uri, format!("at://did:plc:seed/{COLLECTION}/1"));
            assert_eq!(found.cid, "bafylookup");

            assert!(
                lookup_record(&ctx, COLLECTION, "externalIds.steam", "999")
                    .await
                    .expect("the lookup should run")
                    .is_none(),
                "an unknown external ID resolves to nothing on {:?}",
                ctx.db_backend
            );
        }
    }

    /// An external ID stored as a JSON number, which SQLite's `json_extract`
    /// hands back as a number: the cast to text is what lets it equal the
    /// text a caller passes, as it already does on Postgres.
    #[tokio::test]
    #[serial]
    async fn lookup_record_matches_a_numeric_external_id_on_every_backend() {
        for ctx in contexts().await {
            seed(&ctx, json!({"externalIds": {"steam": 440}})).await;
            assert!(
                lookup_record(&ctx, COLLECTION, "externalIds.steam", "440")
                    .await
                    .expect("the lookup should run")
                    .is_some(),
                "a numeric external ID resolves on {:?}",
                ctx.db_backend
            );
        }
    }

    #[tokio::test]
    async fn lookup_record_refuses_an_invalid_field_path() {
        let ctx = context_on(
            crate::test_support::migrated_memory_pool().await,
            DatabaseBackend::Sqlite,
        );
        for path in ["", "a..b", "externalIds'--", "a.b; DROP TABLE x"] {
            assert!(
                matches!(
                    lookup_record(&ctx, COLLECTION, path, "1").await,
                    Err(LookupError::InvalidFieldPath)
                ),
                "{path} should be refused"
            );
        }
    }
}
