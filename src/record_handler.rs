use serde_json::Value;

use crate::AppState;

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
    /// The rev (TID) of the repo commit that produced this event, when the
    /// source carries one (Jetstream does; `listRecords` and dead-letter
    /// retries do not). It orders deliveries: see `crate::tombstones`.
    pub rev: Option<String>,
}

/// The outcome of processing one record event, returned so a caller can
/// account for it in telemetry rather than this function reaching into
/// `state.telemetry_counters` itself.
pub use happyview_provider::IngestOutcome as RecordOutcome;

impl RecordEvent {
    /// This event as the ingest pipeline takes it, from `origin`. `None`
    /// when its action is not one the pipeline knows.
    pub fn to_source_event(
        &self,
        origin: crate::ingest::Origin,
    ) -> Option<crate::ingest::SourceEvent> {
        let action = crate::ingest::Action::parse(&self.action)?;
        Some(crate::ingest::SourceEvent::Record(
            crate::ingest::RecordEvent {
                key: crate::ingest::RecordKey {
                    did: self.did.clone(),
                    collection: self.collection.clone(),
                    rkey: self.rkey.clone(),
                },
                action,
                cid: self.cid.clone(),
                rev: self.rev.clone(),
                body: self.record.clone(),
                time_us: None,
                origin,
                stored: false,
            },
        ))
    }
}

/// Process a record event through `crate::ingest` as a live event: upsert or
/// delete the record, run its hook, or handle a lexicon schema event. Returns
/// the outcome so the caller can account for it in telemetry — see
/// `RecordOutcome`.
///
/// An action the pipeline does not know is judged as before the pipeline
/// existed: a lexicon schema event's own rules first, then an untracked
/// collection is `Skipped`, and only a tracked one is `Invalid`.
pub async fn handle_record_event(state: &AppState, record: &RecordEvent) -> RecordOutcome {
    if let Some(event) = record.to_source_event(crate::ingest::Origin::Live) {
        return crate::ingest::apply(state, event).await;
    }
    if record.collection == LEXICON_SCHEMA_COLLECTION {
        return handle_lexicon_schema_event(state, &record.did, record).await;
    }
    if !crate::ingest::is_tracked(state, &record.collection).await {
        tracing::debug!(
            collection = %record.collection,
            "skipping record for untracked collection"
        );
        return RecordOutcome::Skipped;
    }
    RecordOutcome::Invalid
}

/// Handle a `com.atproto.lexicon.schema` record event for tracked network
/// lexicons. `SchemaEvent` when it was applied or is not one HappyView
/// tracks; a database error is classified like a record write's.
pub async fn handle_lexicon_schema_event(
    state: &AppState,
    did: &str,
    record: &RecordEvent,
) -> RecordOutcome {
    crate::ingest::lexicon_schema_event(
        state,
        did,
        &record.rkey,
        &record.action,
        record.record.as_ref(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    use crate::db::now_rfc3339;

    use crate::catalog::LexiconWrite;
    use crate::db::adapt_sql;
    use crate::lexicon::ParsedLexicon;
    use crate::lexicon::ProcedureAction;
    use crate::plugin::{LoadedPlugin, PluginManifest, PluginSource, loader};
    use crate::project::{ProjectLexicon, ProjectSnapshot};
    use crate::test_support::test_state_with_pool;

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

    /// A migrated state with `NSID` registered as a record-type lexicon, so
    /// `handle_record_event` treats it as tracked.
    async fn tracked_state() -> AppState {
        let state = test_state_with_pool(crate::test_support::migrated_memory_pool().await);
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
            rev: None,
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
            rev: None,
        }
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
            rev: None,
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
        let state = tracked_state().await;
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
        crate::linked_repos::pds::index_write(state, "did:plc:abc", NSID, &uri, "", &edited)
            .await
            .expect("index the edited record");
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
        let state = tracked_state().await;
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
        let state = tracked_state().await;
        crate::db::query("DROP TABLE happyview_record_refs_v2")
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
        state
            .scripts
            .put_manual(
                &state.db,
                state.db_backend,
                crate::catalog::ScriptWrite {
                    id: trigger.to_string(),
                    script_type: "lua".to_string(),
                    body: body.to_string(),
                    description: None,
                },
            )
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
                rev: None,
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

    fn rev(n: i64) -> String {
        crate::tid::tid_from_unix_microseconds(1_700_000_000_000_000 + n)
    }

    fn event_with(
        rkey: &str,
        action: &str,
        body: Option<serde_json::Value>,
        rev: Option<String>,
    ) -> RecordEvent {
        RecordEvent {
            did: "did:plc:abc".to_string(),
            collection: NSID.to_string(),
            rkey: rkey.to_string(),
            action: action.to_string(),
            record: body,
            cid: None,
            rev,
        }
    }

    async fn body_of(state: &AppState, uri: &str) -> Option<serde_json::Value> {
        let sql = adapt_sql(
            "SELECT record FROM happyview_records WHERE uri = ?",
            state.db_backend,
        );
        let row: Option<(String,)> = crate::db::query_as(&sql)
            .bind(uri)
            .fetch_optional(&state.db)
            .await
            .expect("read record");
        row.map(|(r,)| serde_json::from_str(&r).expect("record json"))
    }

    async fn stored_rev(state: &AppState, uri: &str) -> Option<String> {
        let sql = adapt_sql(
            "SELECT rev FROM happyview_records WHERE uri = ?",
            state.db_backend,
        );
        let row: Option<(Option<String>,)> = crate::db::query_as(&sql)
            .bind(uri)
            .fetch_optional(&state.db)
            .await
            .expect("read rev");
        row.and_then(|(r,)| r)
    }

    async fn tombstone_rev(state: &AppState, uri: &str) -> Option<String> {
        let sql = adapt_sql(
            "SELECT rev FROM happyview_record_tombstones WHERE uri = ?",
            state.db_backend,
        );
        let row: Option<(String,)> = crate::db::query_as(&sql)
            .bind(uri)
            .fetch_optional(&state.db)
            .await
            .expect("read tombstone");
        row.map(|(r,)| r)
    }

    async fn clean_up_rkey(state: &AppState, rkey: &str) {
        let uri = format!("at://did:plc:abc/{NSID}/{rkey}");
        for sql in [
            "DELETE FROM happyview_records WHERE uri = ?",
            "DELETE FROM happyview_record_tombstones WHERE uri = ?",
        ] {
            crate::db::query(&adapt_sql(sql, state.db_backend))
                .bind(&uri)
                .execute(&state.db)
                .await
                .expect("clean up");
        }
    }

    /// An older rev never overwrites a newer one; a write with no rev (a
    /// crawl row, a local echo, a pre-upgrade row) still applies, as before
    /// revs existed, but keeps the stored rev.
    async fn assert_revs_order_updates(state: &AppState, rkey: &str) {
        let uri = format!("at://did:plc:abc/{NSID}/{rkey}");
        let update = |text: &str, r: Option<String>| {
            event_with(rkey, "update", Some(serde_json::json!({ "text": text })), r)
        };

        assert_eq!(
            handle_record_event(state, &update("new", Some(rev(20)))).await,
            RecordOutcome::Matched
        );
        let _ = handle_record_event(state, &update("old", Some(rev(10)))).await;
        assert_eq!(
            body_of(state, &uri).await,
            Some(serde_json::json!({"text": "new"})),
            "an older rev is ignored"
        );
        let _ = handle_record_event(state, &update("newer", Some(rev(30)))).await;
        assert_eq!(
            body_of(state, &uri).await,
            Some(serde_json::json!({"text": "newer"}))
        );
        assert_eq!(stored_rev(state, &uri).await, Some(rev(30)));
        let _ = handle_record_event(state, &update("unversioned", None)).await;
        assert_eq!(
            body_of(state, &uri).await,
            Some(serde_json::json!({"text": "unversioned"})),
            "a write without a rev applies as before"
        );
        assert_eq!(
            stored_rev(state, &uri).await,
            Some(rev(30)),
            "a write without a rev keeps the stored rev"
        );
        let _ = handle_record_event(state, &update("stale", Some(rev(25)))).await;
        assert_eq!(
            body_of(state, &uri).await,
            Some(serde_json::json!({"text": "unversioned"})),
            "the kept rev still orders later writes"
        );
    }

    /// A delete with a rev leaves a tombstone: nothing at or before it, and
    /// nothing without a rev, brings the record back. A delete older than the
    /// stored row leaves the row alone and leaves no tombstone.
    async fn assert_deletes_leave_tombstones(state: &AppState, rkey: &str) {
        let uri = format!("at://did:plc:abc/{NSID}/{rkey}");
        let create = |r: Option<String>| {
            event_with(rkey, "create", Some(serde_json::json!({"text": "hi"})), r)
        };
        let delete = |r: Option<String>| event_with(rkey, "delete", None, r);

        let _ = handle_record_event(state, &create(Some(rev(20)))).await;
        let _ = handle_record_event(state, &delete(Some(rev(10)))).await;
        assert!(
            body_of(state, &uri).await.is_some(),
            "a delete older than the stored row leaves it"
        );
        assert_eq!(
            tombstone_rev(state, &uri).await,
            None,
            "a delete older than the stored row leaves no tombstone"
        );
        assert_eq!(
            handle_record_event(state, &delete(Some(rev(30)))).await,
            RecordOutcome::Matched
        );
        assert!(body_of(state, &uri).await.is_none());
        assert_eq!(tombstone_rev(state, &uri).await, Some(rev(30)));

        for (r, why) in [
            (Some(rev(25)), "an older create"),
            (Some(rev(30)), "a create at the delete's rev"),
            (None, "a create without a rev"),
        ] {
            assert_eq!(
                handle_record_event(state, &create(r)).await,
                RecordOutcome::Matched
            );
            assert!(
                body_of(state, &uri).await.is_none(),
                "{why} must not resurrect the record"
            );
        }

        let _ = handle_record_event(state, &create(Some(rev(40)))).await;
        assert!(
            body_of(state, &uri).await.is_some(),
            "a newer create recreates the record"
        );
    }

    /// A delete that arrives before its record's create still tombstones it,
    /// for an account that holds records in the collection.
    async fn assert_a_delete_with_no_row_tombstones(state: &AppState, rkey: &str) {
        let uri = format!("at://did:plc:abc/{NSID}/{rkey}");
        let held = event_with(
            &format!("{rkey}held"),
            "create",
            Some(serde_json::json!({"text": "held"})),
            Some(rev(10)),
        );
        assert_eq!(
            handle_record_event(state, &held).await,
            RecordOutcome::Matched
        );
        let _ = handle_record_event(state, &event_with(rkey, "delete", None, Some(rev(30)))).await;
        assert_eq!(tombstone_rev(state, &uri).await, Some(rev(30)));
        let create = event_with(
            rkey,
            "create",
            Some(serde_json::json!({"text": "hi"})),
            Some(rev(20)),
        );
        let _ = handle_record_event(state, &create).await;
        assert!(
            body_of(state, &uri).await.is_none(),
            "the create the delete overtook must not land"
        );
    }

    #[tokio::test]
    async fn an_older_rev_never_overwrites_a_newer_one() {
        let state = tracked_state().await;
        assert_revs_order_updates(&state, "revs").await;
    }

    #[tokio::test]
    async fn an_older_rev_never_overwrites_a_newer_one_on_postgres() {
        let Some(state) = crate::test_support::test_state_from_env().await else {
            eprintln!("skipped (TEST_DATABASE_URL not set)");
            return;
        };
        register_tracked_lexicon(&state).await;
        let rkey = format!("revs{}", uuid::Uuid::new_v4().simple());
        assert_revs_order_updates(&state, &rkey).await;
        clean_up_rkey(&state, &rkey).await;
    }

    #[tokio::test]
    async fn a_delete_leaves_a_tombstone_that_blocks_resurrection() {
        let state = tracked_state().await;
        assert_deletes_leave_tombstones(&state, "tomb").await;
    }

    #[tokio::test]
    async fn a_delete_leaves_a_tombstone_that_blocks_resurrection_on_postgres() {
        let Some(state) = crate::test_support::test_state_from_env().await else {
            eprintln!("skipped (TEST_DATABASE_URL not set)");
            return;
        };
        register_tracked_lexicon(&state).await;
        let rkey = format!("tomb{}", uuid::Uuid::new_v4().simple());
        assert_deletes_leave_tombstones(&state, &rkey).await;
        clean_up_rkey(&state, &rkey).await;
    }

    async fn add_backfill_job(state: &AppState, status: &str, collection: Option<&str>) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        crate::db::query(&adapt_sql(
            "INSERT INTO happyview_backfill_jobs (id, collection, status, created_at) VALUES (?, ?, ?, ?)",
            state.db_backend,
        ))
        .bind(&id)
        .bind(collection)
        .bind(status)
        .bind(now_rfc3339())
        .execute(&state.db)
        .await
        .expect("seed backfill job");
        id
    }

    /// Ruling FR1: a delete for a record this instance never held, from an
    /// account it holds nothing for in the collection, leaves no tombstone
    /// unless a backfill still writing (running, pausing or cancelling)
    /// covers the collection.
    #[tokio::test]
    async fn a_delete_with_no_row_tombstones_only_when_a_create_may_follow() {
        let state = tracked_state().await;
        let did = "did:plc:stranger";
        let delete = |rkey: &str| RecordEvent {
            did: did.to_string(),
            collection: NSID.to_string(),
            rkey: rkey.to_string(),
            action: "delete".to_string(),
            record: None,
            cid: None,
            rev: Some(rev(30)),
        };
        let tombstoned = |rkey: &str| {
            let uri = format!("at://{did}/{NSID}/{rkey}");
            let state = state.clone();
            async move { tombstone_rev(&state, &uri).await.is_some() }
        };

        // Another account's records and other collections' jobs don't count.
        insert_record(&state).await;
        add_backfill_job(&state, "running", Some("com.example.other")).await;
        add_backfill_job(&state, "completed", None).await;
        let _ = handle_record_event(&state, &delete("never")).await;
        assert!(!tombstoned("never").await, "nothing could resurrect it");

        for (status, collection) in [
            ("running", None),
            ("pausing", Some(NSID)),
            ("cancelling", Some(NSID)),
        ] {
            let job = add_backfill_job(&state, status, collection).await;
            let rkey = format!("job{status}");
            let _ = handle_record_event(&state, &delete(&rkey)).await;
            assert!(
                tombstoned(&rkey).await,
                "a {status} backfill covering the collection may crawl it"
            );
            crate::db::query("DELETE FROM happyview_backfill_jobs WHERE id = ?")
                .bind(job)
                .execute(&state.db)
                .await
                .expect("drop job");
        }

        let held = RecordEvent {
            action: "create".to_string(),
            record: Some(serde_json::json!({"text": "held"})),
            rev: Some(rev(10)),
            ..delete("held")
        };
        assert_eq!(
            handle_record_event(&state, &held).await,
            RecordOutcome::Matched
        );
        let _ = handle_record_event(&state, &delete("later")).await;
        assert!(
            tombstoned("later").await,
            "the account holds records in the collection"
        );
    }

    #[tokio::test]
    async fn a_delete_with_no_row_still_tombstones() {
        let state = tracked_state().await;
        assert_a_delete_with_no_row_tombstones(&state, "early").await;
    }

    #[tokio::test]
    async fn a_delete_with_no_row_still_tombstones_on_postgres() {
        let Some(state) = crate::test_support::test_state_from_env().await else {
            eprintln!("skipped (TEST_DATABASE_URL not set)");
            return;
        };
        register_tracked_lexicon(&state).await;
        let rkey = format!("early{}", uuid::Uuid::new_v4().simple());
        assert_a_delete_with_no_row_tombstones(&state, &rkey).await;
        clean_up_rkey(&state, &rkey).await;
        clean_up_rkey(&state, &format!("{rkey}held")).await;
    }

    /// A delete without a rev (a dead-letter retry) tombstones the deleted
    /// row at the rev it was stored at, so a replay of that create, or of an
    /// older one, cannot bring it back; a newer create still applies. A row
    /// stored without a rev leaves no tombstone.
    async fn assert_unversioned_deletes_tombstone_the_stored_rev(state: &AppState, rkey: &str) {
        let uri = format!("at://did:plc:abc/{NSID}/{rkey}");
        let create = |r: Option<String>| {
            event_with(rkey, "create", Some(serde_json::json!({"text": "hi"})), r)
        };
        let delete = event_with(rkey, "delete", None, None);

        let _ = handle_record_event(state, &create(Some(rev(20)))).await;
        assert_eq!(
            handle_record_event(state, &delete).await,
            RecordOutcome::Matched
        );
        assert!(body_of(state, &uri).await.is_none());
        assert_eq!(tombstone_rev(state, &uri).await, Some(rev(20)));
        for (r, why) in [
            (rev(20), "a replay of the deleted create"),
            (rev(10), "an older create"),
        ] {
            let _ = handle_record_event(state, &create(Some(r))).await;
            assert!(
                body_of(state, &uri).await.is_none(),
                "{why} must not resurrect the record"
            );
        }
        let _ = handle_record_event(state, &create(Some(rev(30)))).await;
        assert!(
            body_of(state, &uri).await.is_some(),
            "a newer create still applies"
        );

        let unversioned = format!("{rkey}u");
        let uri = format!("at://did:plc:abc/{NSID}/{unversioned}");
        let _ = handle_record_event(
            state,
            &event_with(
                &unversioned,
                "create",
                Some(serde_json::json!({"text": "hi"})),
                None,
            ),
        )
        .await;
        let _ = handle_record_event(state, &event_with(&unversioned, "delete", None, None)).await;
        assert!(body_of(state, &uri).await.is_none());
        assert_eq!(
            tombstone_rev(state, &uri).await,
            None,
            "a row without a rev leaves no tombstone"
        );
    }

    #[tokio::test]
    async fn an_unversioned_delete_tombstones_the_stored_rev() {
        let state = tracked_state().await;
        assert_unversioned_deletes_tombstone_the_stored_rev(&state, "unv").await;
    }

    #[tokio::test]
    async fn an_unversioned_delete_tombstones_the_stored_rev_on_postgres() {
        let Some(state) = crate::test_support::test_state_from_env().await else {
            eprintln!("skipped (TEST_DATABASE_URL not set)");
            return;
        };
        register_tracked_lexicon(&state).await;
        let rkey = format!("unv{}", uuid::Uuid::new_v4().simple());
        assert_unversioned_deletes_tombstone_the_stored_rev(&state, &rkey).await;
        clean_up_rkey(&state, &rkey).await;
        clean_up_rkey(&state, &format!("{rkey}u")).await;
    }

    /// A delete with a rev tombstones the record.
    #[tokio::test]
    async fn a_delete_with_a_rev_tombstones_the_record() {
        let state = tracked_state().await;
        insert_record(&state).await;
        let mut event = delete_event();
        event.rev = Some(rev(1));
        assert_eq!(
            handle_record_event(&state, &event).await,
            RecordOutcome::Matched
        );
        assert!(!record_exists(&state).await);
    }

    /// On Postgres the tombstone check and the upsert it guards are one
    /// critical section: a create racing a delete's uncommitted tombstone
    /// waits for it and then sees it, rather than checking before the
    /// tombstone lands and writing after.
    #[tokio::test]
    async fn a_create_waits_for_a_concurrent_tombstone_on_postgres() {
        let Some(state) = crate::test_support::test_state_from_env().await else {
            eprintln!("skipped (TEST_DATABASE_URL not set)");
            return;
        };
        register_tracked_lexicon(&state).await;
        let rkey = format!("race{}", uuid::Uuid::new_v4().simple());
        let uri = format!("at://did:plc:abc/{NSID}/{rkey}");

        let mut tx = crate::db::begin_write(&state.db, state.db_backend)
            .await
            .expect("begin");
        crate::tombstones::lock_uris(&mut tx, state.db_backend, &[&uri])
            .await
            .expect("lock uri");
        crate::tombstones::write_record_tombstone(
            &mut tx,
            state.db_backend,
            &uri,
            &rev(30),
            &now_rfc3339(),
        )
        .await
        .expect("write tombstone");

        let create = event_with(
            &rkey,
            "create",
            Some(serde_json::json!({"text": "hi"})),
            Some(rev(20)),
        );
        let racer = {
            let state = state.clone();
            tokio::spawn(async move { handle_record_event(&state, &create).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(
            !racer.is_finished(),
            "the create must wait for the uncommitted tombstone"
        );
        tx.commit().await.expect("commit tombstone");

        assert_eq!(racer.await.expect("join"), RecordOutcome::Matched);
        assert!(
            body_of(&state, &uri).await.is_none(),
            "the create must see the tombstone it waited for"
        );
        clean_up_rkey(&state, &rkey).await;
    }

    fn schema_event(action: &str) -> RecordEvent {
        RecordEvent {
            did: "did:plc:abc".to_string(),
            collection: LEXICON_SCHEMA_COLLECTION.to_string(),
            rkey: "com.example.schema".to_string(),
            action: action.to_string(),
            record: (action != "delete").then(|| {
                serde_json::json!({
                    "lexicon": 1,
                    "id": "com.example.schema",
                    "defs": {"main": {"type": "record", "key": "tid"}},
                })
            }),
            cid: None,
            rev: None,
        }
    }

    const NET: &str = "com.example.net";
    const AUTHORITY: &str = "did:plc:authority";

    fn net_json(description: &str) -> serde_json::Value {
        serde_json::json!({
            "lexicon": 1, "id": NET, "description": description,
            "defs": { "main": { "type": "record", "key": "tid" } },
        })
    }

    // Not `schema_event`: the module already has one, for an untracked NSID.
    fn net_event(action: &str, record: Option<serde_json::Value>) -> RecordEvent {
        RecordEvent {
            did: AUTHORITY.to_string(),
            collection: LEXICON_SCHEMA_COLLECTION.to_string(),
            rkey: NET.to_string(),
            action: action.to_string(),
            record,
            cid: None,
            rev: None,
        }
    }

    async fn network_state() -> AppState {
        let state = test_state_with_pool(crate::test_support::migrated_memory_pool().await);
        state
            .lexicons
            .put_network(
                &state.db,
                state.db_backend,
                LexiconWrite {
                    lexicon_json: net_json("v1"),
                    backfill: false,
                    target_collection: None,
                    action: ProcedureAction::Upsert,
                    token_cost: None,
                    authority_did: Some(AUTHORITY.to_string()),
                },
            )
            .await
            .expect("seed a network lexicon");
        state
    }

    #[tokio::test]
    async fn a_schema_event_from_the_authority_refreshes_the_row_and_memory() {
        let state = network_state().await;
        assert_eq!(
            handle_lexicon_schema_event(
                &state,
                AUTHORITY,
                &net_event("update", Some(net_json("v2")))
            )
            .await,
            RecordOutcome::SchemaEvent
        );

        let net = state.lexicons.get_network(NET).await.unwrap();
        assert_eq!(
            (net.parsed.revision, net.parsed.raw["description"].clone()),
            (2, "v2".into())
        );
        let fresh = crate::catalog::LexiconCatalog::detached();
        fresh.load_from_db(&state.db).await.unwrap();
        assert_eq!(
            fresh.get(NET).await.unwrap().revision,
            2,
            "the row moved too"
        );
    }

    #[tokio::test]
    async fn a_schema_event_from_another_did_changes_nothing() {
        let state = network_state().await;
        let outcome = handle_lexicon_schema_event(
            &state,
            "did:plc:someone",
            &net_event("update", Some(net_json("v2"))),
        )
        .await;
        assert_eq!(outcome, RecordOutcome::SchemaEvent);
        assert_eq!(
            state
                .lexicons
                .get_network(NET)
                .await
                .unwrap()
                .parsed
                .revision,
            1
        );
    }

    #[tokio::test]
    async fn a_schema_delete_from_the_authority_drops_the_row_and_memory() {
        let state = network_state().await;
        let outcome =
            handle_lexicon_schema_event(&state, AUTHORITY, &net_event("delete", None)).await;
        assert_eq!(outcome, RecordOutcome::SchemaEvent);
        assert!(state.lexicons.get(NET).await.is_none());
        let fresh = crate::catalog::LexiconCatalog::detached();
        fresh.load_from_db(&state.db).await.unwrap();
        assert!(fresh.get(NET).await.is_none());
    }

    /// The network row keeps refreshing under a file, so it is current when
    /// the file goes; every read meanwhile answers the file.
    #[tokio::test]
    async fn a_schema_event_for_a_shadowed_network_lexicon_refreshes_the_row_only() {
        let state = network_state().await;
        let mut snap = ProjectSnapshot::empty();
        snap.lexicons.insert(
            NET.to_string(),
            ProjectLexicon {
                parsed: crate::lexicon::ParsedLexicon::parse(
                    net_json("file"),
                    0,
                    None,
                    ProcedureAction::Upsert,
                    None,
                )
                .unwrap(),
                path: "src/lexicons/com/example/net.json".into(),
                backfill: false,
            },
        );
        state.project.store(std::sync::Arc::new(snap));

        let outcome = handle_lexicon_schema_event(
            &state,
            AUTHORITY,
            &net_event("update", Some(net_json("v2"))),
        )
        .await;
        assert_eq!(outcome, RecordOutcome::SchemaEvent);
        assert_eq!(
            state
                .lexicons
                .get_network(NET)
                .await
                .unwrap()
                .parsed
                .revision,
            2
        );
        assert_eq!(
            state.lexicons.get(NET).await.unwrap().raw["description"],
            "file"
        );
    }

    /// Tracks `schema_event`'s NSID as a network lexicon from its DID, so the
    /// event reaches the database.
    async fn track_schema_nsid(state: &AppState) {
        state
            .lexicons
            .put_network(
                &state.db,
                state.db_backend,
                crate::catalog::LexiconWrite {
                    lexicon_json: schema_event("create").record.unwrap(),
                    backfill: false,
                    target_collection: None,
                    action: ProcedureAction::Upsert,
                    token_cost: None,
                    authority_did: Some("did:plc:abc".to_string()),
                },
            )
            .await
            .expect("track the schema NSID");
    }

    /// A schema event cannot be dropped because the database was down: the
    /// caller is told to retry it.
    #[tokio::test]
    async fn a_schema_event_during_an_outage_is_unavailable() {
        let state = tracked_state().await;
        track_schema_nsid(&state).await;
        state.db.close().await;
        for action in ["create", "update", "delete"] {
            assert_eq!(
                handle_record_event(&state, &schema_event(action)).await,
                RecordOutcome::Unavailable,
                "{action}"
            );
        }
    }

    #[tokio::test]
    async fn a_schema_event_whose_query_fails_is_errored() {
        let state = tracked_state().await;
        track_schema_nsid(&state).await;
        crate::db::query(&format!(
            "DROP TABLE {}",
            crate::catalog::lexicon_rows::TABLE
        ))
        .execute(&state.db)
        .await
        .expect("drop lexicons table");
        assert_eq!(
            handle_record_event(&state, &schema_event("create")).await,
            RecordOutcome::Errored
        );
    }

    #[tokio::test]
    async fn a_schema_event_for_an_untracked_lexicon_changes_nothing() {
        let state = tracked_state().await;
        assert_eq!(
            handle_record_event(&state, &schema_event("create")).await,
            RecordOutcome::SchemaEvent
        );
    }

    #[tokio::test]
    async fn a_write_with_no_body_or_an_unknown_operation_is_invalid() {
        let state = tracked_state().await;
        let mut no_body = create_event();
        no_body.record = None;
        assert_eq!(
            handle_record_event(&state, &no_body).await,
            RecordOutcome::Invalid
        );
        let mut unknown = create_event();
        unknown.action = "frobnicate".to_string();
        assert_eq!(
            handle_record_event(&state, &unknown).await,
            RecordOutcome::Invalid
        );
        assert!(!record_exists(&state).await);
    }

    /// Postgres text cannot hold a NUL byte (`22021`), so a record keyed by
    /// one can never be stored: it is invalid, not a failure worth retrying.
    #[tokio::test]
    async fn a_record_postgres_cannot_store_is_invalid() {
        let Some(state) = crate::test_support::test_state_from_env().await else {
            eprintln!("skipped (TEST_DATABASE_URL not set)");
            return;
        };
        if state.db_backend != crate::db::DatabaseBackend::Postgres {
            return;
        }
        register_tracked_lexicon(&state).await;
        let rkey = format!("nul{}\0", uuid::Uuid::new_v4().simple());
        let event = create_with(&rkey, serde_json::json!({"text": "hello"}));
        assert_eq!(
            handle_record_event(&state, &event).await,
            RecordOutcome::Invalid
        );
    }
}
