use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde_json::Value;

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339};
use crate::event_log::{EventLog, Severity, log_event};
use crate::lexicon::{LexiconType, ParsedLexicon, ProcedureAction};
use crate::lua::RecordHookOutcome;

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

            // Reject records whose claimed CID doesn't match their content
            // (security review L9). A hostile source can otherwise store a
            // record under a mismatched CID. Skip indexing entirely on
            // mismatch; `Skipped` (no/unencodable CID) proceeds unchanged.
            if crate::cid_verify::verify_record_cid(cid, rec)
                == crate::cid_verify::CidCheck::Mismatch
            {
                log_event(
                    db,
                    EventLog {
                        event_type: "record.cid_mismatch".to_string(),
                        severity: Severity::Warn,
                        actor_did: None,
                        subject: Some(uri.clone()),
                        detail: serde_json::json!({
                            "collection": record.collection,
                            "did": record.did,
                            "rkey": record.rkey,
                            "claimed_cid": cid,
                            "reason": "record content does not match claimed CID",
                        }),
                    },
                    state.db_backend,
                )
                .await;
                return;
            }

            // Run record-event script (if any) before storing. The script's
            // return value determines what gets written:
            //   Skip → skip indexing entirely
            //   Replace(record) → upsert with that record body
            //   Proceed → upsert with the record as it arrived
            // The dispatcher cascades `record.<action>:<nsid>` →
            // `record.index:<nsid>`; failures are dead-lettered fail-open.
            let hook_result = crate::lua::run_record_event_script(
                state,
                crate::lua::RecordEventPayload {
                    nsid: &record.collection,
                    action: &record.action,
                    uri: &uri,
                    did: &record.did,
                    rkey: &record.rkey,
                    record: Some(rec),
                },
            )
            .await;
            let rec_to_store = match hook_result {
                RecordHookOutcome::Skip => {
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
                RecordHookOutcome::Replace(v) => v,
                RecordHookOutcome::Proceed => rec.clone(),
            };

            let now = now_rfc3339();
            let backend = state.db_backend;
            let insert_sql = adapt_sql(
                r#"
                INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT (uri) DO UPDATE
                    SET record = EXCLUDED.record,
                        cid = EXCLUDED.cid,
                        indexed_at = ?
                "#,
                backend,
            );
            match crate::db::query(&insert_sql)
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

                    if state.verbose_event_logging.load(Ordering::Relaxed) {
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
                    }

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

            // Run record-event script (if any) before deleting. Only a
            // script that actually ran and returned `nil` aborts the
            // delete — no script, or a dead-lettered one, proceeds.
            let hook_result = crate::lua::run_record_event_script(
                state,
                crate::lua::RecordEventPayload {
                    nsid: &record.collection,
                    action: "delete",
                    uri: &uri,
                    did: &record.did,
                    rkey: &record.rkey,
                    record: None,
                },
            )
            .await;
            if hook_result == RecordHookOutcome::Skip {
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

            let delete_sql = adapt_sql("DELETE FROM happyview_records WHERE uri = ?", backend);
            match crate::db::query(&delete_sql).bind(&uri).execute(db).await {
                Ok(_) => {
                    if state.verbose_event_logging.load(Ordering::Relaxed) {
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
        "SELECT target_collection FROM happyview_lexicons WHERE id = ? AND source = 'network' AND authority_did = ?",
        backend,
    );
    let tracked: Option<(Option<String>,)> = crate::db::query_as(&select_sql)
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
                INSERT INTO happyview_lexicons (id, lexicon_json, backfill, target_collection, source, authority_did, last_fetched_at, created_at)
                VALUES (?, ?, 0, ?, 'network', ?, ?, ?)
                ON CONFLICT (id) DO UPDATE SET
                    lexicon_json = EXCLUDED.lexicon_json,
                    target_collection = EXCLUDED.target_collection,
                    last_fetched_at = ?,
                    revision = happyview_lexicons.revision + 1,
                    updated_at = ?
                "#,
                backend,
            );
            if let Err(e) = crate::db::query(&upsert_sql)
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
            let delete_sql = adapt_sql("DELETE FROM happyview_lexicons WHERE id = ?", backend);
            let _ = crate::db::query(&delete_sql).bind(nsid).execute(db).await;

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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::lexicon::ProcedureAction;
    use crate::test_support::{memory_pool, test_state_with_pool};

    const NSID: &str = "com.example.thing";
    const URI: &str = "at://did:plc:abc/com.example.thing/rkey1";

    /// A state with the record/script/event-log tables and `NSID` registered as
    /// a record-type lexicon, so `handle_record_event` treats it as tracked.
    async fn tracked_state() -> AppState {
        let pool = memory_pool().await;
        for ddl in [
            "CREATE TABLE happyview_records (
                uri TEXT PRIMARY KEY,
                did TEXT NOT NULL,
                collection TEXT NOT NULL,
                rkey TEXT NOT NULL,
                record TEXT NOT NULL,
                cid TEXT,
                indexed_at TEXT NOT NULL,
                created_at TEXT NOT NULL
            )",
            "CREATE TABLE happyview_scripts (
                id TEXT PRIMARY KEY,
                body TEXT NOT NULL,
                script_type TEXT NOT NULL DEFAULT 'lua'
            )",
            "CREATE TABLE happyview_event_logs (
                id TEXT PRIMARY KEY,
                event_type TEXT NOT NULL,
                severity TEXT NOT NULL,
                actor_did TEXT,
                subject TEXT,
                detail TEXT,
                created_at TEXT NOT NULL
            )",
            "CREATE TABLE happyview_record_refs (
                source_uri TEXT NOT NULL,
                target_uri TEXT NOT NULL,
                field TEXT NOT NULL
            )",
        ] {
            crate::db::query(ddl)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("create table: {e}"));
        }

        let state = test_state_with_pool(pool);
        let parsed = ParsedLexicon::parse(
            serde_json::json!({
                "lexicon": 1,
                "id": NSID,
                "defs": {"main": {"type": "record", "key": "tid"}},
            }),
            1,
            Some(NSID.to_string()),
            ProcedureAction::Upsert,
            None,
        )
        .expect("parse test lexicon");
        state.lexicons.upsert(parsed).await;
        state
    }

    async fn insert_record(state: &AppState) {
        crate::db::query(
            "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(URI)
        .bind("did:plc:abc")
        .bind(NSID)
        .bind("rkey1")
        .bind(r#"{"text":"hello"}"#)
        .bind("bafyreiabc")
        .bind("2026-01-01T00:00:00+00:00")
        .bind("2026-01-01T00:00:00+00:00")
        .execute(&state.db)
        .await
        .expect("seed record");
    }

    async fn record_exists(state: &AppState) -> bool {
        let row: Option<(String,)> =
            crate::db::query_as("SELECT uri FROM happyview_records WHERE uri = ?")
                .bind(URI)
                .fetch_optional(&state.db)
                .await
                .expect("query record");
        row.is_some()
    }

    fn delete_event() -> RecordEvent {
        RecordEvent {
            did: "did:plc:abc".to_string(),
            collection: NSID.to_string(),
            rkey: "rkey1".to_string(),
            action: "delete".to_string(),
            record: None,
            cid: None,
        }
    }

    async fn install_script(state: &AppState, trigger: &str, body: &str) {
        crate::db::query(
            "INSERT INTO happyview_scripts (id, body, script_type) VALUES (?, ?, 'lua')",
        )
        .bind(trigger)
        .bind(body)
        .execute(&state.db)
        .await
        .expect("install script");
    }

    /// Regression test for #80: a Jetstream delete for a tracked collection with
    /// no registered script must delete the row. The "no script ran" and "the
    /// script returned nil" signals used to be spelled the same way, so an
    /// instance with no scripts at all skipped every delete.
    #[tokio::test]
    async fn delete_without_any_script_removes_the_record() {
        let state = tracked_state().await;
        insert_record(&state).await;

        handle_record_event(&state, &delete_event()).await;

        assert!(
            !record_exists(&state).await,
            "delete with no registered script must remove the record"
        );
    }

    /// `return true` is the documented "proceed, I only had side effects"
    /// return. On a delete it used to fall through to the original record
    /// body — which is nil for a delete — and abort.
    #[tokio::test]
    async fn delete_with_a_script_returning_true_removes_the_record() {
        let state = tracked_state().await;
        install_script(
            &state,
            &format!("record.delete:{NSID}"),
            "function handle() return true end",
        )
        .await;
        insert_record(&state).await;

        handle_record_event(&state, &delete_event()).await;

        assert!(
            !record_exists(&state).await,
            "a delete script returning true must let the delete proceed"
        );
    }

    /// The documented delete gate: a script that runs and returns `nil`
    /// still keeps the record. This is the one case that must NOT delete.
    #[tokio::test]
    async fn delete_with_a_script_returning_nil_keeps_the_record() {
        let state = tracked_state().await;
        install_script(
            &state,
            &format!("record.delete:{NSID}"),
            "function handle() return nil end",
        )
        .await;
        insert_record(&state).await;

        handle_record_event(&state, &delete_event()).await;

        assert!(
            record_exists(&state).await,
            "a delete script returning nil must keep the record"
        );
    }

    /// The create path's pass-through: no script means index the record as it
    /// arrived, not skip it.
    #[tokio::test]
    async fn create_without_any_script_indexes_the_record() {
        let state = tracked_state().await;

        handle_record_event(
            &state,
            &RecordEvent {
                did: "did:plc:abc".to_string(),
                collection: NSID.to_string(),
                rkey: "rkey1".to_string(),
                action: "create".to_string(),
                record: Some(serde_json::json!({"text": "hello"})),
                cid: None,
            },
        )
        .await;

        assert!(
            record_exists(&state).await,
            "create with no registered script must index the record"
        );
    }
}
