//! SyncRecord processing pipeline.
//!
//! Processes records returned by plugin sync_account():
//! - Signs records that have `sign: true`
//! - Resolves game references
//! - Prepares records for writing to PDS

use super::attestation::{AttestationError, AttestationSigner};
use super::types::SyncRecord;
use crate::db::{DatabaseBackend, adapt_sql};
use serde_json::Value;

/// Processed record ready for storage
#[derive(Debug, Clone)]
pub struct ProcessedRecord {
    /// The collection (lexicon ID)
    pub collection: String,
    /// The processed record with signatures added
    pub record: Value,
    /// Deduplication key
    pub dedup_key: Option<String>,
    /// CID of the signed content (if signed)
    pub content_cid: Option<String>,
}

/// Error during sync record processing
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("Attestation signing failed: {0}")]
    Attestation(#[from] AttestationError),

    #[error("Game reference resolution failed: {0}")]
    GameResolution(String),

    #[error("Invalid record: {0}")]
    InvalidRecord(String),
}

/// Process a batch of SyncRecords from a plugin
pub struct SyncProcessor<'a> {
    /// Attestation signer (optional - if None, signing is skipped)
    signer: Option<&'a AttestationSigner>,
    /// Repository DID for the user (used in $sig for replay protection)
    repository_did: String,
}

impl<'a> SyncProcessor<'a> {
    /// Create a new sync processor
    pub fn new(signer: Option<&'a AttestationSigner>, repository_did: String) -> Self {
        Self {
            signer,
            repository_did,
        }
    }

    /// Process a batch of SyncRecords
    pub fn process_records(
        &self,
        records: Vec<SyncRecord>,
    ) -> Result<Vec<ProcessedRecord>, SyncError> {
        let mut processed = Vec::with_capacity(records.len());

        for record in records {
            processed.push(self.process_record(record)?);
        }

        Ok(processed)
    }

    /// Process a single SyncRecord
    fn process_record(&self, sync_record: SyncRecord) -> Result<ProcessedRecord, SyncError> {
        let mut record = sync_record.record;

        // Resolve game references if present
        self.resolve_game_ref(&mut record)?;

        // Sign if requested and signer is available
        let content_cid = if sync_record.sign {
            if let Some(signer) = self.signer {
                let cid = signer.sign_record(&mut record, &self.repository_did)?;
                Some(cid.to_string())
            } else {
                tracing::warn!(
                    collection = %sync_record.collection,
                    "Record requested signing but no signer configured"
                );
                None
            }
        } else {
            None
        };

        Ok(ProcessedRecord {
            collection: sync_record.collection,
            record,
            dedup_key: sync_record.dedup_key,
            content_cid,
        })
    }

    /// Resolve game references in a record
    ///
    /// Looks for game references like `{"platform": "steam", "externalId": "440"}`
    /// and attempts to resolve them to AT URIs.
    fn resolve_game_ref(&self, record: &mut Value) -> Result<(), SyncError> {
        // Look for "game" field with platform/externalId structure
        if let Some(obj) = record.as_object_mut()
            && let Some(game_ref) = obj.get("game")
            && let Some(game_obj) = game_ref.as_object()
            && game_obj.contains_key("platform")
            && game_obj.contains_key("externalId")
            && !game_obj.contains_key("uri")
        {
            // Unresolved reference - log for debugging
            let platform = game_obj
                .get("platform")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let external_id = game_obj
                .get("externalId")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            tracing::debug!(
                platform = %platform,
                external_id = %external_id,
                "Game reference left unresolved - resolution not yet implemented"
            );
        }

        Ok(())
    }
}

/// Helper to create a sync processor with common setup
pub fn create_processor<'a>(
    signer: Option<&'a AttestationSigner>,
    user_did: &str,
) -> SyncProcessor<'a> {
    SyncProcessor::new(signer, user_did.to_string())
}

/// Resolve game references in records by looking up in the database.
///
/// Looks for `game: {platform: "steam", externalId: "440"}` and converts to
/// `game: {uri: "at://...", cid: "..."}` if found.
pub async fn resolve_game_references(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    records: &mut [SyncRecord],
) {
    for record in records.iter_mut() {
        let Some(obj) = record.record.as_object_mut() else {
            continue;
        };
        let Some(game_ref) = obj.get("game").cloned() else {
            continue;
        };
        let Some(game_obj) = game_ref.as_object() else {
            continue;
        };

        // Check for unresolved reference
        if !game_obj.contains_key("platform")
            || !game_obj.contains_key("externalId")
            || game_obj.contains_key("uri")
        {
            continue;
        }

        let platform = game_obj
            .get("platform")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let external_id = game_obj
            .get("externalId")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if let Some((uri, cid)) =
            lookup_game_by_external_id(db, backend, platform, external_id).await
        {
            obj.insert(
                "game".to_string(),
                serde_json::json!({
                    "uri": uri,
                    "cid": cid
                }),
            );
            tracing::debug!(
                platform = %platform,
                external_id = %external_id,
                uri = %uri,
                "Resolved game reference"
            );
        } else {
            tracing::debug!(
                platform = %platform,
                external_id = %external_id,
                "Game not found in database, leaving reference unresolved"
            );
        }
    }
}

/// Look up a game by external ID (e.g., Steam app ID).
///
/// Returns (uri, cid) if found.
async fn lookup_game_by_external_id(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    platform: &str,
    external_id: &str,
) -> Option<(String, String)> {
    // Build JSON path based on platform
    // Looking for records where: record.externalIds.<platform> = external_id
    let json_path = match backend {
        DatabaseBackend::Sqlite => {
            format!("json_extract(record, '$.externalIds.{}')", platform)
        }
        DatabaseBackend::Postgres => {
            format!("record->'externalIds'->>'{}'", platform)
        }
    };

    let sql = adapt_sql(
        &format!(
            "SELECT uri, cid FROM happyview_records WHERE collection = 'games.gamesgamesgamesgames.game' AND {} = ? LIMIT 1",
            json_path
        ),
        backend,
    );

    let result: Option<(String, String)> = sqlx::query_as(&sql)
        .bind(external_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_unsigned_record() {
        let processor = SyncProcessor::new(None, "did:plc:testuser".to_string());

        let records = vec![SyncRecord {
            collection: "test.collection".into(),
            record: serde_json::json!({
                "$type": "test.collection",
                "data": "hello"
            }),
            dedup_key: Some("test:1".into()),
            sign: false,
        }];

        let processed = processor.process_records(records).unwrap();
        assert_eq!(processed.len(), 1);
        assert_eq!(processed[0].collection, "test.collection");
        assert!(processed[0].content_cid.is_none());
    }

    #[test]
    fn test_process_signed_record() {
        let signer =
            AttestationSigner::for_testing("did:web:test#key".into(), "test.signature".into());
        let processor = SyncProcessor::new(Some(&signer), "did:plc:testuser".to_string());

        let records = vec![SyncRecord {
            collection: "games.gamesgamesgamesgames.actor.game".into(),
            record: serde_json::json!({
                "$type": "games.gamesgamesgamesgames.actor.game",
                "game": {"platform": "steam", "externalId": "440"},
                "platform": "steam",
                "createdAt": "2024-01-01T00:00:00Z"
            }),
            dedup_key: Some("steam:game:440".into()),
            sign: true,
        }];

        let processed = processor.process_records(records).unwrap();
        assert_eq!(processed.len(), 1);
        assert!(processed[0].content_cid.is_some());

        // Verify signatures array was added
        let signatures = processed[0].record["signatures"].as_array();
        assert!(signatures.is_some());
        assert_eq!(signatures.unwrap().len(), 1);
    }

    #[test]
    fn test_sign_requested_but_no_signer() {
        let processor = SyncProcessor::new(None, "did:plc:testuser".to_string());

        let records = vec![SyncRecord {
            collection: "test.collection".into(),
            record: serde_json::json!({"data": "hello"}),
            dedup_key: None,
            sign: true, // Requested but no signer
        }];

        let processed = processor.process_records(records).unwrap();
        assert_eq!(processed.len(), 1);
        // No error, but no CID either
        assert!(processed[0].content_cid.is_none());
    }
}
