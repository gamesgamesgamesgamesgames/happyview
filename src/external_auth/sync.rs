use crate::db::adapt_sql;
use crate::plugin::SyncRecord;

#[allow(dead_code)] // Used when full sync flow is implemented
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("Validation error: {0}")]
    Validation(String),
    #[error("PDS write error: {0}")]
    PdsWrite(String),
}

/// Process sync records from a plugin
#[allow(dead_code)] // Used when full sync flow is implemented
pub async fn process_sync_records(
    db: &sqlx::AnyPool,
    db_backend: crate::db::DatabaseBackend,
    plugin_id: &str,
    user_did: &str,
    records: Vec<SyncRecord>,
) -> Result<usize, SyncError> {
    let mut processed = 0;

    for record in records {
        // TODO: Validate against lexicon schema
        // TODO: Check dedup_key
        // TODO: Sign attestation
        // TODO: Write to PDS

        // For now, just track dedup key
        if let Some(dedup_key) = &record.dedup_key {
            let sql = adapt_sql(
                "INSERT INTO happyview_plugin_dedup_keys (plugin_id, did, dedup_key, record_uri, updated_at)
                 VALUES (?, ?, ?, ?, datetime('now'))
                 ON CONFLICT (plugin_id, did, dedup_key)
                 DO UPDATE SET record_uri = excluded.record_uri, updated_at = excluded.updated_at",
                db_backend,
            );

            sqlx::query(&sql)
                .bind(plugin_id)
                .bind(user_did)
                .bind(dedup_key)
                .bind("at://placeholder") // TODO: Real URI after PDS write
                .execute(db)
                .await?;
        }

        processed += 1;
    }

    Ok(processed)
}
