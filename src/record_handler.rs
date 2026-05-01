use std::sync::Arc;

use serde_json::Value;

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339};
use crate::event_log::{EventLog, Severity, log_event};
use crate::lexicon::{LexiconType, ParsedLexicon, ProcedureAction};

/// The static collection we always include for lexicon schema updates.
pub const LEXICON_SCHEMA_COLLECTION: &str = "com.atproto.lexicon.schema";

/// A generic record event that can originate from any source (Jetstream, backfill, etc.).
pub struct RecordEvent {
    pub did: String,
    pub collection: String,
    pub rkey: String,
    pub action: String,
    pub record: Option<Value>,
    pub cid: Option<String>,
}

/// Process a record event: upsert/delete the record in the database, run index
/// hooks, and handle lexicon schema events.
pub async fn handle_record_event(state: &AppState, record: &RecordEvent) {
    let db = &state.db;
    let lexicons = &state.lexicons;

    let uri = format!("at://{}/{}/{}", record.did, record.collection, record.rkey);

    // Handle lexicon schema events for tracked network lexicons.
    if record.collection == LEXICON_SCHEMA_COLLECTION {
        handle_lexicon_schema_event(state, &record.did, record).await;
        return;
    }

    // Skip records whose collection is not tracked by a registered record-type lexicon.
    let is_tracked = lexicons
        .get(&record.collection)
        .await
        .is_some_and(|lex| lex.lexicon_type == LexiconType::Record);

    if !is_tracked {
        tracing::debug!(
            collection = %record.collection,
            "skipping record for untracked collection"
        );
        return;
    }

    match record.action.as_str() {
        "create" | "update" => {
            let rec = match &record.record {
                Some(r) => r,
                None => return,
            };
            let cid = record.cid.as_deref().unwrap_or_default();

            // Run record-event script (if any) before storing. The script's
            // return value determines what gets written:
            //   None → skip indexing entirely
            //   Some(record) → upsert with that record body
            // The dispatcher cascades `record.<action>:<nsid>` →
            // `record.index:<nsid>`; failures are dead-lettered fail-open.
            let hook_result = crate::lua::run_record_event_script(
                state,
                &record.collection,
                &record.action,
                &uri,
                &record.did,
                &record.rkey,
                Some(rec),
            )
            .await;
            let rec_to_store = match hook_result {
                None => {
                    log_event(
                        db,
                        EventLog {
                            event_type: "record.skipped".to_string(),
                            severity: Severity::Info,
                            actor_did: None,
                            subject: Some(uri.clone()),
                            detail: serde_json::json!({
                                "collection": record.collection,
                                "did": record.did,
                                "rkey": record.rkey,
                                "reason": "script returned nil",
                            }),
                        },
                        state.db_backend,
                    )
                    .await;
                    return;
                }
                Some(v) => v,
            };

            let now = now_rfc3339();
            let backend = state.db_backend;
            let insert_sql = adapt_sql(
                r#"
                INSERT INTO records (uri, did, collection, rkey, record, cid, indexed_at, created_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT (uri) DO UPDATE
                    SET record = EXCLUDED.record,
                        cid = EXCLUDED.cid,
                        indexed_at = ?
                "#,
                backend,
            );
            match sqlx::query(&insert_sql)
                .bind(&uri)
                .bind(&record.did)
                .bind(&record.collection)
                .bind(&record.rkey)
                .bind(serde_json::to_string(&rec_to_store).unwrap_or_default())
                .bind(cid)
                .bind(&now)
                .bind(&now)
                .bind(&now)
                .execute(db)
                .await
            {
                Ok(_) => {
                    let _ = crate::record_refs::sync_refs(
                        db,
                        &uri,
                        &record.collection,
                        &rec_to_store,
                        backend,
                    )
                    .await;

                    log_event(
                        db,
                        EventLog {
                            event_type: "record.created".to_string(),
                            severity: Severity::Info,
                            actor_did: None,
                            subject: Some(uri.clone()),
                            detail: serde_json::json!({
                                "collection": record.collection,
                                "did": record.did,
                                "rkey": record.rkey,
                            }),
                        },
                        backend,
                    )
                    .await;

                    crate::labeler::backfill_labels_for_uri(Arc::new(state.clone()), uri.clone());
                }
                Err(e) => {
                    tracing::warn!(uri = %uri, "failed to upsert record: {e}");
                    log_event(
                        db,
                        EventLog {
                            event_type: "record.created".to_string(),
                            severity: Severity::Error,
                            actor_did: None,
                            subject: Some(uri.clone()),
                            detail: serde_json::json!({
                                "collection": record.collection,
                                "did": record.did,
                                "rkey": record.rkey,
                                "error": e.to_string(),
                            }),
                        },
                        backend,
                    )
                    .await;
                }
            }
        }
        "delete" => {
            let backend = state.db_backend;

            // Run record-event script (if any) before deleting. A nil
            // return aborts the delete; any other return continues.
            let hook_result = crate::lua::run_record_event_script(
                state,
                &record.collection,
                "delete",
                &uri,
                &record.did,
                &record.rkey,
                None,
            )
            .await;
            if hook_result.is_none() {
                log_event(
                    db,
                    EventLog {
                        event_type: "record.skipped".to_string(),
                        severity: Severity::Info,
                        actor_did: None,
                        subject: Some(uri.clone()),
                        detail: serde_json::json!({
                            "collection": record.collection,
                            "did": record.did,
                            "rkey": record.rkey,
                            "reason": "script returned nil",
                        }),
                    },
                    backend,
                )
                .await;
                return;
            }

            let delete_sql = adapt_sql("DELETE FROM records WHERE uri = ?", backend);
            match sqlx::query(&delete_sql).bind(&uri).execute(db).await {
                Ok(_) => {
                    log_event(
                        db,
                        EventLog {
                            event_type: "record.deleted".to_string(),
                            severity: Severity::Info,
                            actor_did: None,
                            subject: Some(uri.clone()),
                            detail: serde_json::json!({
                                "collection": record.collection,
                                "did": record.did,
                                "rkey": record.rkey,
                            }),
                        },
                        backend,
                    )
                    .await;
                }
                Err(e) => {
                    tracing::warn!(uri = %uri, "failed to delete record: {e}");
                    log_event(
                        db,
                        EventLog {
                            event_type: "record.deleted".to_string(),
                            severity: Severity::Error,
                            actor_did: None,
                            subject: Some(uri.clone()),
                            detail: serde_json::json!({
                                "collection": record.collection,
                                "did": record.did,
                                "rkey": record.rkey,
                                "error": e.to_string(),
                            }),
                        },
                        backend,
                    )
                    .await;
                }
            }
        }
        _ => {}
    }
}

/// Handle a `com.atproto.lexicon.schema` record event for tracked network lexicons.
pub async fn handle_lexicon_schema_event(state: &AppState, did: &str, record: &RecordEvent) {
    let db = &state.db;
    let lexicons = &state.lexicons;
    let collections_tx = &state.collections_tx;
    let nsid = &record.rkey;

    let backend = state.db_backend;

    // Check if this NSID is one we're tracking and the DID matches the authority.
    let select_sql = adapt_sql(
        "SELECT target_collection FROM lexicons WHERE id = ? AND source = 'network' AND authority_did = ?",
        backend,
    );
    let tracked: Option<(Option<String>,)> = sqlx::query_as(&select_sql)
        .bind(nsid)
        .bind(did)
        .fetch_optional(db)
        .await
        .unwrap_or(None);

    let target_collection = match tracked {
        Some((tc,)) => tc,
        None => return, // Not a tracked network lexicon.
    };

    match record.action.as_str() {
        "create" | "update" => {
            let rec = match &record.record {
                Some(r) => r,
                None => return,
            };

            let parsed = match ParsedLexicon::parse(
                rec.clone(),
                1,
                target_collection.clone(),
                ProcedureAction::Upsert,
                None,
                None,
                None,
            ) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(nsid, "failed to parse lexicon schema event: {e}");
                    return;
                }
            };

            let is_record = parsed.lexicon_type == crate::lexicon::LexiconType::Record;

            // Upsert into lexicons table with last_fetched_at.
            let now = now_rfc3339();
            let upsert_sql = adapt_sql(
                r#"
                INSERT INTO lexicons (id, lexicon_json, backfill, target_collection, source, authority_did, last_fetched_at, created_at)
                VALUES (?, ?, 0, ?, 'network', ?, ?, ?)
                ON CONFLICT (id) DO UPDATE SET
                    lexicon_json = EXCLUDED.lexicon_json,
                    target_collection = EXCLUDED.target_collection,
                    last_fetched_at = ?,
                    revision = lexicons.revision + 1,
                    updated_at = ?
                "#,
                backend,
            );
            if let Err(e) = sqlx::query(&upsert_sql)
                .bind(nsid)
                .bind(serde_json::to_string(rec).unwrap_or_default())
                .bind(&target_collection)
                .bind(did)
                .bind(&now)
                .bind(&now)
                .bind(&now)
                .bind(&now)
                .execute(db)
                .await
            {
                tracing::warn!(nsid, "failed to upsert lexicon from event: {e}");
                return;
            }

            lexicons.upsert(parsed).await;
            tracing::info!(nsid, "updated network lexicon from network event");

            if is_record {
                let collections = lexicons.get_record_collections().await;
                let _ = collections_tx.send(collections);
            }
        }
        "delete" => {
            // Remove from lexicons table and registry.
            let delete_sql = adapt_sql("DELETE FROM lexicons WHERE id = ?", backend);
            let _ = sqlx::query(&delete_sql).bind(nsid).execute(db).await;

            let was_present = lexicons.remove(nsid).await;
            if was_present {
                tracing::info!(nsid, "removed network lexicon from network delete event");
                let collections = lexicons.get_record_collections().await;
                let _ = collections_tx.send(collections);
            }
        }
        _ => {}
    }
}
