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

/// The outcome of processing one record event, returned so a caller can
/// account for it in telemetry rather than this function reaching into
/// `state.telemetry_counters` itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum RecordOutcome {
    Matched,
    Skipped,
    SchemaEvent,
    Errored,
}

/// Process a record event: upsert/delete the record in the database, run index
/// hooks, and handle lexicon schema events. Returns the outcome so the caller
/// can account for it in telemetry — see `RecordOutcome`.
pub async fn handle_record_event(state: &AppState, record: &RecordEvent) -> RecordOutcome {
    let db = &state.db;
    let lexicons = &state.lexicons;

    let uri = format!("at://{}/{}/{}", record.did, record.collection, record.rkey);

    // Handle lexicon schema events for tracked network lexicons.
    if record.collection == LEXICON_SCHEMA_COLLECTION {
        handle_lexicon_schema_event(state, &record.did, record).await;
        return RecordOutcome::SchemaEvent;
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
        return RecordOutcome::Skipped;
    }

    match record.action.as_str() {
        "create" | "update" => {
            let rec = match &record.record {
                Some(r) => r,
                None => return RecordOutcome::Errored,
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
                return RecordOutcome::Skipped;
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
                    // Gated like `record.created`/`record.deleted` below: a
                    // filtering script skips far more records than it keeps, so
                    // logging every skip writes a row per discarded firehose
                    // record.
                    if state.verbose_event_logging.load(Ordering::Relaxed) {
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
                    }
                    return RecordOutcome::Skipped;
                }
                RecordHookOutcome::Replace(v) => v,
                RecordHookOutcome::Proceed => rec.clone(),
            };

            let now = now_rfc3339();
            let backend = state.db_backend;
            let record_json = serde_json::to_string(&rec_to_store).unwrap_or_default();
            let upsert_sql = adapt_sql(
                &format!(
                    "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
                     ON CONFLICT (uri) DO UPDATE SET record = EXCLUDED.record, cid = EXCLUDED.cid, indexed_at = EXCLUDED.indexed_at \
                     WHERE {}",
                    crate::db::record_changed_clause(backend)
                ),
                backend,
            );
            let written = crate::db::retry_on_busy(|| {
                upsert_record_and_refs(
                    db,
                    &upsert_sql,
                    &uri,
                    record,
                    &record_json,
                    cid,
                    &now,
                    &rec_to_store,
                    backend,
                )
            })
            .await;
            match written {
                Ok(changed) => {
                    // An identical redelivery changed nothing, so there is
                    // nothing new to log or to fetch labels for.
                    if changed {
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
                        crate::labeler::backfill_labels_for_uri(
                            Arc::new(state.clone()),
                            uri.clone(),
                        );
                    }
                    RecordOutcome::Matched
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
                    RecordOutcome::Errored
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
                // Gated for the same reason as the create path above.
                if state.verbose_event_logging.load(Ordering::Relaxed) {
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
                }
                return RecordOutcome::Skipped;
            }

            let delete_sql = adapt_sql("DELETE FROM happyview_records WHERE uri = ?", backend);
            match crate::db::retry_on_busy(|| crate::db::query(&delete_sql).bind(&uri).execute(db))
                .await
            {
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
                    RecordOutcome::Matched
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
                    RecordOutcome::Errored
                }
            }
        }
        _ => RecordOutcome::Errored,
    }
}

/// Upsert one record and, only when the stored row changed, its refs, in one
/// transaction: one commit per record, and a refs failure cannot leave a
/// record without them. Returns whether the row was inserted or changed.
#[allow(clippy::too_many_arguments)]
async fn upsert_record_and_refs(
    db: &sqlx::AnyPool,
    upsert_sql: &str,
    uri: &str,
    event: &RecordEvent,
    record_json: &str,
    cid: &str,
    now: &str,
    rec_to_store: &Value,
    backend: crate::db::DatabaseBackend,
) -> Result<bool, sqlx::Error> {
    let mut tx = db.begin().await?;
    let changed = crate::db::query(upsert_sql)
        .bind(uri)
        .bind(&event.did)
        .bind(&event.collection)
        .bind(&event.rkey)
        .bind(record_json)
        .bind(cid)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?
        .rows_affected()
        > 0;
    if changed {
        crate::record_refs::sync_refs_in(&mut tx, uri, &event.collection, rec_to_store, backend)
            .await?;
    }
    tx.commit().await?;
    Ok(changed)
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
            if let Err(e) =
                crate::db::retry_on_busy(|| crate::db::query(&delete_sql).bind(nsid).execute(db))
                    .await
            {
                // The registry keeps the lexicon too, so it and the table
                // cannot disagree; the next delete event retries.
                tracing::error!(nsid, error = %e, "failed to delete network lexicon from event");
                return;
            }

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
    use crate::plugin::{LoadedPlugin, PluginManifest, PluginSource, loader};
    use crate::test_support::{memory_pool, test_state_with_pool};

    const NSID: &str = "com.example.thing";
    const URI: &str = "at://did:plc:abc/com.example.thing/rkey1";

    const ECHO_FIXTURE: &str = "interpreter_echo";
    const ECHO_TARGET: &str = "wasm32-unknown-unknown";

    /// The echo fixture installed as the interpreter for `lua`, the language a
    /// seeded script row names. Its `source` is a directive rather than a
    /// script, so a test here varies what a run returns without depending on a
    /// language to produce it.
    ///
    /// `false` when the module is unbuilt and the test is to skip.
    async fn echo_interpreter(state: &AppState) -> bool {
        // The check answers the fixture's own directory, so the module is
        // reached through it rather than spelled a second time.
        let Some(dir) = loader::built_fixture(ECHO_FIXTURE, ECHO_TARGET) else {
            return false;
        };
        let module = dir.join(format!("target/{ECHO_TARGET}/release/{ECHO_FIXTURE}.wasm"));
        let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
            "id": "echo", "name": "echo", "version": "1.0.0", "api_version": "2",
            "plugin_type": "interpreter", "language_id": "lua",
            "capabilities": ["library:call", "script:host"],
        }))
        .unwrap();
        state
            .plugin_registry
            .register(LoadedPlugin {
                info: manifest.clone().into(),
                source: PluginSource::File { path: dir },
                wasm_bytes: std::fs::read(&module).expect("the echo module should read"),
                manifest: Some(manifest),
            })
            .await;
        true
    }

    /// A state with an interpreter for the language a seeded row names, or
    /// `None` when the fixture's module is unbuilt. Every fixture's `target/`
    /// is gitignored, so that is a build nobody ran rather than a fault here.
    async fn tracked_state_with_interpreter() -> Option<AppState> {
        let state = tracked_state().await;
        echo_interpreter(&state).await.then_some(state)
    }

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
        register_tracked_lexicon(&state).await;
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

    fn create_event() -> RecordEvent {
        RecordEvent {
            did: "did:plc:abc".to_string(),
            collection: NSID.to_string(),
            rkey: "rkey1".to_string(),
            action: "create".to_string(),
            record: Some(serde_json::json!({"text": "hello"})),
            cid: None,
        }
    }

    /// `tracked_state` on the real migrated schema, which the refs tests need:
    /// `tracked_state`'s hand-written `happyview_record_refs` has no
    /// `collection` column.
    async fn migrated_tracked_state() -> AppState {
        let state = test_state_with_pool(crate::test_support::migrated_memory_pool().await);
        register_tracked_lexicon(&state).await;
        state
    }

    async fn register_tracked_lexicon(state: &AppState) {
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
    }

    fn create_with(rkey: &str, body: serde_json::Value) -> RecordEvent {
        RecordEvent {
            did: "did:plc:abc".to_string(),
            collection: NSID.to_string(),
            rkey: rkey.to_string(),
            action: "create".to_string(),
            record: Some(body),
            cid: None,
        }
    }

    async fn indexed_at(state: &AppState, uri: &str) -> Option<String> {
        let sql = adapt_sql(
            "SELECT indexed_at FROM happyview_records WHERE uri = ?",
            state.db_backend,
        );
        let (at,): (Option<String>,) = crate::db::query_as(&sql)
            .bind(uri)
            .fetch_one(&state.db)
            .await
            .expect("read indexed_at");
        at
    }

    async fn clear_indexed_at(state: &AppState, uri: &str) {
        set_indexed_at(state, uri, None).await;
    }

    async fn set_indexed_at(state: &AppState, uri: &str, at: Option<&str>) {
        let sql = adapt_sql(
            "UPDATE happyview_records SET indexed_at = ? WHERE uri = ?",
            state.db_backend,
        );
        crate::db::query(&sql)
            .bind(at)
            .bind(uri)
            .execute(&state.db)
            .await
            .expect("clear indexed_at");
    }

    async fn ref_count(state: &AppState, uri: &str) -> i64 {
        let sql = adapt_sql(
            "SELECT COUNT(*) FROM happyview_record_refs WHERE source_uri = ?",
            state.db_backend,
        );
        let (n,): (i64,) = crate::db::query_as(&sql)
            .bind(uri)
            .fetch_one(&state.db)
            .await
            .expect("count refs");
        n
    }

    async fn delete_refs(state: &AppState, uri: &str) {
        let sql = adapt_sql(
            "DELETE FROM happyview_record_refs WHERE source_uri = ?",
            state.db_backend,
        );
        crate::db::query(&sql)
            .bind(uri)
            .execute(&state.db)
            .await
            .expect("delete refs");
    }

    /// An identical redelivery leaves the row alone: same `indexed_at`, and its
    /// refs are not rewritten (deleted here so a rewrite would show). A real
    /// change rewrites both. A row with no `indexed_at` (written locally) is
    /// stamped by an identical echo, which also writes its refs.
    async fn assert_redelivery_semantics(state: &AppState, rkey: &str) {
        let uri = format!("at://did:plc:abc/{NSID}/{rkey}");
        let body =
            serde_json::json!({"text": "hi", "subject": "at://did:plc:other/app.test.post/1"});

        assert_eq!(
            handle_record_event(state, &create_with(rkey, body.clone())).await,
            RecordOutcome::Matched
        );
        let first = indexed_at(state, &uri).await;
        assert!(first.is_some());
        assert_eq!(ref_count(state, &uri).await, 1);

        delete_refs(state, &uri).await;
        assert_eq!(
            handle_record_event(state, &create_with(rkey, body.clone())).await,
            RecordOutcome::Matched
        );
        assert_eq!(
            indexed_at(state, &uri).await,
            first,
            "an identical record keeps indexed_at"
        );
        assert_eq!(
            ref_count(state, &uri).await,
            0,
            "an identical record does not rewrite refs"
        );

        // A locally written record has no indexed_at; the identical echo stamps it.
        clear_indexed_at(state, &uri).await;
        assert_eq!(
            handle_record_event(state, &create_with(rkey, body)).await,
            RecordOutcome::Matched
        );
        assert!(
            indexed_at(state, &uri).await.is_some(),
            "an identical echo stamps a row with no indexed_at"
        );
        assert_eq!(
            ref_count(state, &uri).await,
            1,
            "the stamped row gets its refs"
        );

        set_indexed_at(state, &uri, Some("2020-01-01T00:00:00+00:00")).await;
        let changed =
            serde_json::json!({"text": "edited", "subject": "at://did:plc:other/app.test.post/1"});
        assert_eq!(
            handle_record_event(state, &create_with(rkey, changed)).await,
            RecordOutcome::Matched
        );
        assert_ne!(
            indexed_at(state, &uri).await.as_deref(),
            Some("2020-01-01T00:00:00+00:00"),
            "a changed record is re-indexed"
        );
        assert_eq!(
            ref_count(state, &uri).await,
            1,
            "a changed record rewrites refs"
        );
    }

    #[tokio::test]
    async fn an_identical_redelivery_is_a_no_op() {
        let state = migrated_tracked_state().await;
        assert_redelivery_semantics(&state, "noop").await;
    }

    #[tokio::test]
    async fn identical_redelivery_is_a_no_op_on_postgres() {
        let Some(state) = crate::test_support::test_state_from_env().await else {
            eprintln!("skipped (TEST_DATABASE_URL not set)");
            return;
        };
        register_tracked_lexicon(&state).await;
        let rkey = format!("noop{}", uuid::Uuid::new_v4().simple());
        assert_redelivery_semantics(&state, &rkey).await;

        let sql = adapt_sql(
            "DELETE FROM happyview_records WHERE uri = ?",
            state.db_backend,
        );
        crate::db::query(&sql)
            .bind(format!("at://did:plc:abc/{NSID}/{rkey}"))
            .execute(&state.db)
            .await
            .expect("clean up");
    }

    /// A record indexed at T1, edited locally (which clears `indexed_at`), then
    /// echoed back identically by Jetstream, is stamped again rather than left
    /// at T1.
    async fn assert_local_edit_is_restamped_by_echo(state: &AppState, rkey: &str) {
        let uri = format!("at://did:plc:abc/{NSID}/{rkey}");
        let old = "2020-01-01T00:00:00+00:00";
        let body = serde_json::json!({"text": "v1"});
        assert_eq!(
            handle_record_event(state, &create_with(rkey, body)).await,
            RecordOutcome::Matched
        );
        set_indexed_at(state, &uri, Some(old)).await;

        let edited = serde_json::json!({"text": "v2"});
        crate::linked_repos::pds::index_write(state, "did:plc:abc", NSID, &uri, "", &edited).await;
        assert_eq!(
            indexed_at(state, &uri).await,
            None,
            "a local edit clears indexed_at"
        );

        // The echo carries no CID, matching the empty one the local write stored.
        let _ = handle_record_event(state, &create_with(rkey, edited)).await;
        let restamped = indexed_at(state, &uri).await;
        assert!(restamped.is_some(), "the echo re-stamps the edited row");
        assert_ne!(restamped.as_deref(), Some(old));
    }

    #[tokio::test]
    async fn a_local_edit_is_restamped_by_the_identical_echo() {
        let state = migrated_tracked_state().await;
        assert_local_edit_is_restamped_by_echo(&state, "local").await;
    }

    #[tokio::test]
    async fn a_local_edit_is_restamped_by_the_identical_echo_on_postgres() {
        let Some(state) = crate::test_support::test_state_from_env().await else {
            eprintln!("skipped (TEST_DATABASE_URL not set)");
            return;
        };
        register_tracked_lexicon(&state).await;
        let rkey = format!("local{}", uuid::Uuid::new_v4().simple());
        assert_local_edit_is_restamped_by_echo(&state, &rkey).await;
        let sql = adapt_sql(
            "DELETE FROM happyview_records WHERE uri = ?",
            state.db_backend,
        );
        crate::db::query(&sql)
            .bind(format!("at://did:plc:abc/{NSID}/{rkey}"))
            .execute(&state.db)
            .await
            .expect("clean up");
    }

    /// The record and its refs commit together: a refs write that fails takes
    /// the record with it, and the failure is logged rather than dropped.
    #[tokio::test]
    async fn a_failed_refs_write_rolls_back_the_record() {
        let state = migrated_tracked_state().await;
        crate::db::query("DROP TABLE happyview_record_refs")
            .execute(&state.db)
            .await
            .expect("drop refs table");

        let outcome = handle_record_event(
            &state,
            &create_with(
                "rkey1",
                serde_json::json!({"subject": "at://did:plc:other/app.test.post/1"}),
            ),
        )
        .await;

        assert_eq!(outcome, RecordOutcome::Errored);
        assert!(
            !record_exists(&state).await,
            "the record must roll back with its refs"
        );
        let (errors,): (i64,) = crate::db::query_as(
            "SELECT COUNT(*) FROM happyview_event_logs WHERE event_type = 'record.created' AND severity = 'error'",
        )
        .fetch_one(&state.db)
        .await
        .expect("count error events");
        assert_eq!(errors, 1);
    }

    async fn skipped_event_count(state: &AppState) -> i64 {
        let row: (i64,) = crate::db::query_as(
            "SELECT COUNT(*) FROM happyview_event_logs WHERE event_type = 'record.skipped'",
        )
        .fetch_one(&state.db)
        .await
        .expect("count record.skipped events");
        row.0
    }

    async fn executed_event_count(state: &AppState) -> i64 {
        let row: (i64,) = crate::db::query_as(
            "SELECT COUNT(*) FROM happyview_event_logs WHERE event_type = 'script.executed'",
        )
        .fetch_one(&state.db)
        .await
        .expect("count script.executed events");
        row.0
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

        let _ = handle_record_event(&state, &delete_event()).await;

        assert!(
            !record_exists(&state).await,
            "delete with no registered script must remove the record"
        );
    }

    /// A value that is neither a table nor nothing is the documented
    /// "proceed, I only had side effects" answer, and on a delete it must
    /// reach the delete rather than the original record body — which a delete
    /// does not carry.
    #[tokio::test]
    async fn delete_with_a_script_that_proceeds_removes_the_record() {
        let Some(state) = tracked_state_with_interpreter().await else {
            return;
        };
        install_script(&state, &format!("record.delete:{NSID}"), "value:other").await;
        insert_record(&state).await;

        let _ = handle_record_event(&state, &delete_event()).await;

        assert!(
            !record_exists(&state).await,
            "a delete script returning true must let the delete proceed"
        );
    }

    /// The documented delete gate: a script that runs and returns `nil`
    /// still keeps the record. This is the one case that must NOT delete.
    #[tokio::test]
    async fn delete_with_a_script_returning_nil_keeps_the_record() {
        let Some(state) = tracked_state_with_interpreter().await else {
            return;
        };
        install_script(&state, &format!("record.delete:{NSID}"), "value:none").await;
        insert_record(&state).await;

        let _ = handle_record_event(&state, &delete_event()).await;

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

        let _ = handle_record_event(
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

    /// `record.skipped` is per-record telemetry on the *most common* outcome
    /// for a filtering script, so it belongs behind the same `verbose_event_logging`
    /// gate as its `record.created`/`record.deleted` siblings. Ungated, it wrote a
    /// row for every discarded firehose record: on upvote.at that was 5.7M of the
    /// 5.8M rows in `happyview_event_logs`, ~2.7 GB, against 397 `record.created`.
    #[tokio::test]
    async fn create_skip_is_not_logged_while_verbose_logging_is_off() {
        let Some(state) = tracked_state_with_interpreter().await else {
            return;
        };
        install_script(&state, &format!("record.create:{NSID}"), "value:none").await;

        let _ = handle_record_event(&state, &create_event()).await;

        assert!(
            !record_exists(&state).await,
            "a create script returning nil must skip indexing"
        );
        assert_eq!(
            skipped_event_count(&state).await,
            0,
            "record.skipped must not be logged while verbose event logging is off"
        );
    }

    #[tokio::test]
    async fn delete_skip_is_not_logged_while_verbose_logging_is_off() {
        let Some(state) = tracked_state_with_interpreter().await else {
            return;
        };
        install_script(&state, &format!("record.delete:{NSID}"), "value:none").await;
        insert_record(&state).await;

        let _ = handle_record_event(&state, &delete_event()).await;

        assert!(
            record_exists(&state).await,
            "a delete script returning nil must keep the record"
        );
        assert_eq!(
            skipped_event_count(&state).await,
            0,
            "record.skipped must not be logged while verbose event logging is off"
        );
    }

    /// The gate must suppress the log, not remove it: with verbose logging on,
    /// both skip paths still report.
    #[tokio::test]
    async fn create_skip_is_logged_when_verbose_logging_is_on() {
        let Some(state) = tracked_state_with_interpreter().await else {
            return;
        };
        state.verbose_event_logging.store(true, Ordering::Relaxed);
        install_script(&state, &format!("record.create:{NSID}"), "value:none").await;

        let _ = handle_record_event(&state, &create_event()).await;

        assert_eq!(
            skipped_event_count(&state).await,
            1,
            "record.skipped must still be logged when verbose event logging is on"
        );
    }

    #[tokio::test]
    async fn delete_skip_is_logged_when_verbose_logging_is_on() {
        let Some(state) = tracked_state_with_interpreter().await else {
            return;
        };
        state.verbose_event_logging.store(true, Ordering::Relaxed);
        install_script(&state, &format!("record.delete:{NSID}"), "value:none").await;
        insert_record(&state).await;

        let _ = handle_record_event(&state, &delete_event()).await;

        assert_eq!(
            skipped_event_count(&state).await,
            1,
            "record.skipped must still be logged when verbose event logging is on"
        );
    }

    /// `script.executed` fires once per record that reaches a script, so it has
    /// the same volume as `record.created` and sits behind the same gate.
    #[tokio::test]
    async fn script_executed_is_not_logged_while_verbose_logging_is_off() {
        let Some(state) = tracked_state_with_interpreter().await else {
            return;
        };
        install_script(&state, &format!("record.create:{NSID}"), "returns:true").await;

        let _ = handle_record_event(&state, &create_event()).await;

        assert!(
            record_exists(&state).await,
            "a create script returning true must index the record"
        );
        assert_eq!(
            executed_event_count(&state).await,
            0,
            "script.executed must not be logged while verbose event logging is off"
        );
    }

    #[tokio::test]
    async fn script_executed_is_logged_when_verbose_logging_is_on() {
        let Some(state) = tracked_state_with_interpreter().await else {
            return;
        };
        state.verbose_event_logging.store(true, Ordering::Relaxed);
        install_script(&state, &format!("record.create:{NSID}"), "returns:true").await;

        let _ = handle_record_event(&state, &create_event()).await;

        assert_eq!(
            executed_event_count(&state).await,
            1,
            "script.executed must still be logged when verbose event logging is on"
        );
    }
}
