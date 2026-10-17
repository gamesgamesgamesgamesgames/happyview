//! What each Jetstream event does to the database.
//!
//! With no `kinds` filter, account, identity and sync events arrive for every
//! repo on the network, not only those this instance holds, so nothing here
//! logs unless it found or changed records for the DID.
//!
//! Every event comes to an `ApplyOutcome`: `Unavailable` when the database
//! could not be reached (`db::is_unavailable`), which the worker retries until
//! it applies; `Failed` for any other error, which it retries a few times and
//! then gives up (`give_up_fn` logs that); `Applied` otherwise, including a
//! write the record pipeline skipped or a tombstone refused, and a commit it
//! found invalid (logged once as `jetstream.record_invalid`).

use std::sync::Arc;
use std::sync::atomic::Ordering;

use futures_util::FutureExt;
use tokio::sync::watch;

use super::event::{Commit, CursorPoint, Event, EventKind};
use super::workers::{ApplyFn, ApplyOutcome, GiveUpFn};
use crate::AppState;
use crate::event_log::{EventLog, Severity, log_event};
use crate::ingest::{Origin, SourceEvent};
use crate::record_handler::{self, LEXICON_SCHEMA_COLLECTION, RecordEvent, RecordOutcome};

/// The worker's `ApplyFn`. `collections` is the registered record
/// collections; only a `#sync` reads them, so only a `#sync` clones them.
pub fn apply_fn(state: AppState, collections: watch::Receiver<Vec<String>>) -> ApplyFn {
    Arc::new(move |event: Event| {
        let state = state.clone();
        let tracked = match event.kind {
            EventKind::Sync { .. } => collections.borrow().clone(),
            _ => Vec::new(),
        };
        async move { apply_event(&state, &tracked, event).await }.boxed()
    })
}

/// The worker's `GiveUpFn`: logs `jetstream.apply_failed` for an event that
/// kept failing, with the record's URI (or the event's kind, and an account
/// event's status), its seq and the last error. It is the one error event a
/// failed event writes; the attempts before it are only traced.
pub fn give_up_fn(state: AppState) -> GiveUpFn {
    Arc::new(move |event: Event, error: String| {
        let state = state.clone();
        async move {
            let mut detail = match &event.kind {
                EventKind::Commit(commit) => serde_json::json!({
                    "uri": format!("at://{}/{}/{}", event.did, commit.collection, commit.rkey),
                }),
                EventKind::Account { status, .. } => {
                    serde_json::json!({ "kind": "account", "status": status })
                }
                EventKind::Identity { .. } => serde_json::json!({ "kind": "identity" }),
                EventKind::Sync { .. } => serde_json::json!({ "kind": "sync" }),
            };
            detail["seq"] = event.cursor.seq().into();
            detail["error"] = error.into();
            log(
                &state,
                "jetstream.apply_failed",
                Severity::Error,
                &event.did,
                detail,
            )
            .await;
        }
        .boxed()
    })
}

/// Apply one event. `tracked` is the registered record collections, which
/// bound what a `#sync` purges; other events ignore it. Commits go through
/// the ingest pipeline as live events; account, sync and identity events go
/// through it acting now (`as_of_us: None`).
pub async fn apply_event(state: &AppState, tracked: &[String], event: Event) -> ApplyOutcome {
    let Event { did, cursor, kind } = event;
    match kind {
        EventKind::Commit(commit) => apply_commit(state, did, cursor, commit).await,
        kind => apply_marker(state, tracked, did, cursor, kind, None).await,
    }
}

/// Apply an account or sync marker replayed from the archive. It acts as of
/// its own time (plan ruling P2): it purges only rows older than itself and
/// never lifts or lowers a newer tombstone, so applying it after the commits
/// of the same range leaves what seq order would, and live writes made since
/// survive. `tracked` scopes a `#sync` purge, as for live.
pub async fn apply_replayed_marker(
    state: &AppState,
    tracked: &[String],
    event: Event,
) -> ApplyOutcome {
    let Event { did, cursor, kind } = event;
    match kind {
        EventKind::Commit(_) | EventKind::Identity { .. } => {
            debug_assert!(
                false,
                "apply_replayed_marker takes account and sync markers only"
            );
            ApplyOutcome::Applied
        }
        kind => {
            let as_of_us = Some(cursor.time_us());
            apply_marker(state, tracked, did, cursor, kind, as_of_us).await
        }
    }
}

/// The ingest event for an account, sync or identity event seen at `cursor`.
pub(crate) fn marker_event(
    did: String,
    cursor: CursorPoint,
    kind: EventKind,
    as_of_us: Option<i64>,
) -> SourceEvent {
    let (seq, time_us) = (cursor.seq(), Some(cursor.time_us()));
    match kind {
        EventKind::Account { active, status } => SourceEvent::Account {
            did,
            active,
            status,
            as_of_us,
            seq,
            time_us,
        },
        EventKind::Sync { rev } => SourceEvent::Sync {
            did,
            rev,
            as_of_us,
            seq,
            time_us,
        },
        EventKind::Identity { handle } => SourceEvent::Identity { did, handle },
        EventKind::Commit(_) => unreachable!("a commit is not a marker"),
    }
}

async fn apply_marker(
    state: &AppState,
    tracked: &[String],
    did: String,
    cursor: CursorPoint,
    kind: EventKind,
    as_of_us: Option<i64>,
) -> ApplyOutcome {
    let event = marker_event(did, cursor, kind, as_of_us);
    worker_outcome(crate::ingest::apply_in(state, event, Some(tracked)).await)
}

/// What the worker makes of an account, sync or identity event's outcome:
/// `Unavailable` is held until it applies, any other failure is retried a
/// few times.
fn worker_outcome(applied: crate::ingest::Applied) -> ApplyOutcome {
    let error = || applied.error.clone().unwrap_or_default();
    match applied.outcome {
        RecordOutcome::Unavailable => ApplyOutcome::Unavailable(error()),
        RecordOutcome::Errored | RecordOutcome::Invalid => ApplyOutcome::Failed(error()),
        RecordOutcome::Matched | RecordOutcome::Skipped | RecordOutcome::SchemaEvent => {
            ApplyOutcome::Applied
        }
    }
}

async fn apply_commit(
    state: &AppState,
    did: String,
    cursor: CursorPoint,
    commit: Commit,
) -> ApplyOutcome {
    let event = RecordEvent {
        did,
        collection: commit.collection,
        rkey: commit.rkey,
        action: commit.operation,
        record: commit.record,
        cid: commit.cid,
        rev: commit.rev,
    };
    let uri = || format!("at://{}/{}/{}", event.did, event.collection, event.rkey);
    let outcome = match event.to_source_event(Origin::Live) {
        Some(SourceEvent::Record(mut record)) => {
            record.time_us = Some(cursor.time_us());
            crate::ingest::apply(state, SourceEvent::Record(record)).await
        }
        // An operation the pipeline does not know, judged as before.
        _ => record_handler::handle_record_event(state, &event).await,
    };
    match outcome {
        RecordOutcome::Matched => {
            state
                .telemetry_counters
                .ingest
                .matched
                .fetch_add(1, Ordering::Relaxed);
            ApplyOutcome::Applied
        }
        RecordOutcome::Skipped => {
            state
                .telemetry_counters
                .ingest
                .skipped
                .fetch_add(1, Ordering::Relaxed);
            ApplyOutcome::Applied
        }
        RecordOutcome::SchemaEvent => ApplyOutcome::Applied,
        // The record pipeline logged the error itself (`record.created` or
        // `record.deleted`).
        RecordOutcome::Errored => ApplyOutcome::Failed(format!(
            "{} of {} failed; see its record event log",
            event.action,
            uri()
        )),
        RecordOutcome::Unavailable => ApplyOutcome::Unavailable(format!(
            "the database is unavailable for the {} of {}",
            event.action,
            uri()
        )),
        // Retrying cannot help, so the event is skipped, with one warning.
        RecordOutcome::Invalid => {
            state
                .telemetry_counters
                .ingest
                .skipped
                .fetch_add(1, Ordering::Relaxed);
            let uri = uri();
            let error = invalid_reason(&event);
            tracing::warn!(uri, error, "skipping an invalid jetstream commit");
            log(
                state,
                "jetstream.record_invalid",
                Severity::Warn,
                &event.did,
                serde_json::json!({ "uri": uri, "seq": cursor.seq(), "error": error }),
            )
            .await;
            ApplyOutcome::Applied
        }
    }
}

/// Why the record pipeline found `event` invalid.
fn invalid_reason(event: &RecordEvent) -> String {
    match event.action.as_str() {
        "create" | "update" if event.record.is_none() => {
            format!("the {} carries no record", event.action)
        }
        "create" | "update" | "delete" if event.collection == LEXICON_SCHEMA_COLLECTION => {
            "the lexicon schema could not be applied".to_string()
        }
        "create" | "update" | "delete" => "the database refused the record's data".to_string(),
        action => format!("unknown operation {action:?}"),
    }
}

async fn log(
    state: &AppState,
    event_type: &str,
    severity: Severity,
    did: &str,
    detail: serde_json::Value,
) {
    log_event(
        &state.db,
        EventLog {
            event_type: event_type.to_string(),
            severity,
            actor_did: None,
            subject: Some(did.to_string()),
            detail,
        },
        state.db_backend,
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{adapt_sql, now_rfc3339};
    use crate::ingest::REPO_STATUS_MAX_BYTES;
    use crate::lexicon::{ParsedLexicon, ProcedureAction};
    use std::time::Duration;

    const NSID: &str = "app.test.applied";
    const BASE_US: i64 = 1_779_719_000_000_000;

    /// A migrated SQLite state, plus the `TEST_DATABASE_URL` one when set,
    /// each with `NSID` registered as a record collection.
    async fn states() -> Vec<AppState> {
        let mut states = vec![sqlite_state().await];
        if let Some(state) = crate::test_support::test_state_from_env().await {
            register(&state).await;
            states.push(state);
        }
        states
    }

    async fn sqlite_state() -> AppState {
        let state = crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        );
        register(&state).await;
        state
    }

    async fn register(state: &AppState) {
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
        .expect("parse lexicon");
        state.lexicons.upsert(parsed).await;
    }

    fn unique_did() -> String {
        format!("did:plc:apply{}", uuid::Uuid::new_v4().simple())
    }

    fn at(seq: i64) -> CursorPoint {
        CursorPoint::V2 {
            seq,
            witnessed_us: BASE_US + seq,
        }
    }

    fn rev_at(seq: i64) -> String {
        crate::tid::tid_from_unix_microseconds(BASE_US + seq)
    }

    fn commit(did: &str, seq: i64, rkey: &str, operation: &str) -> Event {
        let record = (operation != "delete").then(|| serde_json::json!({"text": rkey}));
        Event {
            did: did.into(),
            cursor: at(seq),
            kind: EventKind::Commit(Commit {
                rev: Some(rev_at(seq)),
                operation: operation.into(),
                collection: NSID.into(),
                rkey: rkey.into(),
                record,
                cid: None,
            }),
        }
    }

    fn account(did: &str, seq: i64, active: bool, status: Option<&str>) -> Event {
        Event {
            did: did.into(),
            cursor: at(seq),
            kind: EventKind::Account {
                active,
                status: status.map(str::to_string),
            },
        }
    }

    fn sync(did: &str, seq: i64, rev: Option<String>) -> Event {
        Event {
            did: did.into(),
            cursor: at(seq),
            kind: EventKind::Sync { rev },
        }
    }

    async fn apply(state: &AppState, event: Event) {
        assert_eq!(
            apply_event(state, &[NSID.to_string()], event).await,
            ApplyOutcome::Applied
        );
    }

    async fn rkeys(state: &AppState, did: &str) -> Vec<String> {
        let sql = adapt_sql(
            "SELECT rkey FROM happyview_records WHERE did = ? ORDER BY rkey",
            state.db_backend,
        );
        crate::db::query_as::<(String,)>(&sql)
            .bind(did)
            .fetch_all(&state.db)
            .await
            .expect("read records")
            .into_iter()
            .map(|(r,)| r)
            .collect()
    }

    async fn logged(state: &AppState, event_type: &str, did: &str) -> Vec<serde_json::Value> {
        let sql = adapt_sql(
            "SELECT detail FROM happyview_event_logs WHERE event_type = ? AND subject = ?",
            state.db_backend,
        );
        crate::db::query_as::<(String,)>(&sql)
            .bind(event_type)
            .bind(did)
            .fetch_all(&state.db)
            .await
            .expect("read events")
            .into_iter()
            .map(|(d,)| serde_json::from_str(&d).expect("detail json"))
            .collect()
    }

    async fn count(state: &AppState, sql: &str, bind: &str) -> i64 {
        crate::db::query_as::<(i64,)>(&adapt_sql(sql, state.db_backend))
            .bind(bind)
            .fetch_one(&state.db)
            .await
            .expect("count")
            .0
    }

    async fn did_tombstones(state: &AppState, did: &str) -> i64 {
        count(
            state,
            "SELECT COUNT(*) FROM happyview_did_tombstones WHERE did = ?",
            did,
        )
        .await
    }

    async fn insert_local(state: &AppState, did: &str, rkey: &str) {
        let sql = adapt_sql(
            "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at) \
             VALUES (?, ?, ?, ?, '{}', '', NULL, ?)",
            state.db_backend,
        );
        crate::db::query(&sql)
            .bind(format!("at://{did}/{NSID}/{rkey}"))
            .bind(did)
            .bind(NSID)
            .bind(rkey)
            .bind(now_rfc3339())
            .execute(&state.db)
            .await
            .expect("insert local record");
    }

    async fn clean_up(state: &AppState, did: &str) {
        for (sql, value) in [
            (
                "DELETE FROM happyview_records WHERE did = ?",
                did.to_string(),
            ),
            (
                "DELETE FROM happyview_record_tombstones WHERE uri LIKE ?",
                format!("at://{did}/%"),
            ),
            (
                "DELETE FROM happyview_did_tombstones WHERE did = ?",
                did.to_string(),
            ),
            (
                "DELETE FROM happyview_event_logs WHERE subject = ?",
                did.to_string(),
            ),
        ] {
            crate::db::query(&adapt_sql(sql, state.db_backend))
                .bind(value)
                .execute(&state.db)
                .await
                .expect("clean up");
        }
    }

    #[tokio::test]
    async fn commits_go_through_the_record_pipeline_with_their_rev() {
        for state in states().await {
            let did = unique_did();
            apply(&state, commit(&did, 1, "a", "create")).await;
            assert_eq!(rkeys(&state, &did).await, vec!["a"]);
            assert_eq!(
                count(
                    &state,
                    "SELECT COUNT(*) FROM happyview_records WHERE did = ? AND rev IS NOT NULL",
                    &did
                )
                .await,
                1,
                "the commit's rev is stored"
            );
            apply(&state, commit(&did, 2, "a", "delete")).await;
            assert!(rkeys(&state, &did).await.is_empty());
            assert_eq!(
                count(
                    &state,
                    "SELECT COUNT(*) FROM happyview_record_tombstones WHERE uri = ?",
                    &format!("at://{did}/{NSID}/a")
                )
                .await,
                1
            );
            clean_up(&state, &did).await;
        }
    }

    #[tokio::test]
    async fn a_deleted_account_loses_its_network_records_but_not_local_ones() {
        for state in states().await {
            let did = unique_did();
            let other = unique_did();
            apply(&state, commit(&did, 1, "a", "create")).await;
            apply(&state, commit(&did, 2, "b", "create")).await;
            apply(&state, commit(&other, 3, "x", "create")).await;
            insert_local(&state, &did, "local").await;

            apply(&state, account(&did, 10, false, Some("deleted"))).await;

            assert_eq!(
                rkeys(&state, &did).await,
                vec!["local"],
                "{:?}",
                state.db_backend
            );
            assert_eq!(rkeys(&state, &other).await, vec!["x"]);
            assert_eq!(
                count(
                    &state,
                    "SELECT COUNT(*) FROM happyview_did_tombstones WHERE did = ? AND seq = 10",
                    &did
                )
                .await,
                1
            );
            assert_eq!(
                logged(&state, "jetstream.account_deleted", &did).await,
                vec![serde_json::json!({"purged": 2, "seq": 10})]
            );

            // A late copy of an older create cannot bring the account back.
            apply(&state, commit(&did, 5, "c", "create")).await;
            assert_eq!(rkeys(&state, &did).await, vec!["local"]);

            clean_up(&state, &did).await;
            clean_up(&state, &other).await;
        }
    }

    #[tokio::test]
    async fn a_taken_down_or_suspended_account_is_purged_like_a_deleted_one() {
        for state in states().await {
            for status in ["takendown", "suspended"] {
                let did = unique_did();
                apply(&state, commit(&did, 1, "a", "create")).await;
                insert_local(&state, &did, "local").await;
                apply(&state, account(&did, 10, false, Some(status))).await;
                assert_eq!(rkeys(&state, &did).await, vec!["local"], "{status}");
                assert_eq!(
                    logged(&state, "jetstream.account_takendown", &did).await,
                    vec![serde_json::json!({"purged": 1, "seq": 10, "status": status})]
                );
                // An older create replayed later cannot bring it back.
                apply(&state, commit(&did, 5, "b", "create")).await;
                assert_eq!(rkeys(&state, &did).await, vec!["local"], "{status}");
                clean_up(&state, &did).await;
            }
        }
    }

    /// Serve `did`'s DID document from `mock` (the state's PLC), naming
    /// `mock` as its PDS, and answer its `getRepoStatus` with `status`, or
    /// with a 500 when `None`. `calls` is how many status checks to expect.
    async fn mock_pds(
        mock: &wiremock::MockServer,
        did: &str,
        status: Option<serde_json::Value>,
        calls: u64,
    ) {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, ResponseTemplate};

        Mock::given(method("GET"))
            .and(path(format!("/{did}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": did,
                "service": [{
                    "id": "#atproto_pds",
                    "type": "AtprotoPersonalDataServer",
                    "serviceEndpoint": mock.uri(),
                }],
            })))
            .mount(mock)
            .await;
        let response = match status {
            Some(body) => ResponseTemplate::new(200).set_body_json(body),
            None => ResponseTemplate::new(500),
        };
        Mock::given(method("GET"))
            .and(path("/xrpc/com.atproto.sync.getRepoStatus"))
            .and(query_param("did", did))
            .respond_with(response)
            .expect(calls)
            .mount(mock)
            .await;
    }

    /// Rulings FR2 and E1: before purging a deleted account it holds records
    /// for, the instance asks the account's current PDS. Active there means
    /// the relay is relaying a stale host: nothing is purged or tombstoned.
    /// Any other answer, an oversized one, or none, and the purge goes ahead.
    /// Nothing is asked for an account it holds nothing for, nor for a
    /// takedown or suspension, which the account's PDS cannot see.
    #[tokio::test]
    async fn a_deletion_is_skipped_when_the_accounts_pds_says_it_is_active() {
        for mut state in states().await {
            let mock = wiremock::MockServer::start().await;
            state.config.plc_url = mock.uri();

            let migrated = unique_did();
            mock_pds(
                &mock,
                &migrated,
                Some(serde_json::json!({"did": migrated, "active": true})),
                1,
            )
            .await;
            apply(&state, commit(&migrated, 1, "a", "create")).await;
            apply(&state, account(&migrated, 10, false, Some("deleted"))).await;
            assert_eq!(rkeys(&state, &migrated).await, vec!["a"]);
            assert_eq!(did_tombstones(&state, &migrated).await, 0);
            assert_eq!(
                logged(&state, "jetstream.account_status_mismatch", &migrated).await,
                vec![serde_json::json!({"status": "deleted", "pds": mock.uri(), "seq": 10})]
            );
            assert!(
                logged(&state, "jetstream.account_deleted", &migrated)
                    .await
                    .is_empty()
            );

            let gone = unique_did();
            mock_pds(
                &mock,
                &gone,
                Some(serde_json::json!({"did": gone, "active": false, "status": "deleted"})),
                1,
            )
            .await;
            let unreachable = unique_did();
            mock_pds(&mock, &unreachable, None, 1).await;
            let oversized = unique_did();
            mock_pds(
                &mock,
                &oversized,
                Some(serde_json::json!({
                    "did": oversized,
                    "active": true,
                    "padding": "x".repeat(REPO_STATUS_MAX_BYTES),
                })),
                1,
            )
            .await;
            for did in [&gone, &unreachable, &oversized] {
                apply(&state, commit(did, 1, "a", "create")).await;
                apply(&state, account(did, 10, false, Some("deleted"))).await;
                assert!(rkeys(&state, did).await.is_empty(), "{did}");
                assert_eq!(did_tombstones(&state, did).await, 1);
                assert_eq!(
                    logged(&state, "jetstream.account_deleted", did).await,
                    vec![serde_json::json!({"purged": 1, "seq": 10})]
                );
            }

            let mut unasked = Vec::new();
            for status in ["takendown", "suspended"] {
                let did = unique_did();
                mock_pds(
                    &mock,
                    &did,
                    Some(serde_json::json!({"did": did, "active": true})),
                    0,
                )
                .await;
                apply(&state, commit(&did, 1, "a", "create")).await;
                apply(&state, account(&did, 10, false, Some(status))).await;
                assert!(
                    rkeys(&state, &did).await.is_empty(),
                    "a {status} account is purged on the relay's word"
                );
                assert_eq!(did_tombstones(&state, &did).await, 1);
                unasked.push(did);
            }

            let stranger = unique_did();
            mock_pds(
                &mock,
                &stranger,
                Some(serde_json::json!({"did": stranger, "active": true})),
                0,
            )
            .await;
            apply(&state, account(&stranger, 10, false, Some("deleted"))).await;
            assert_eq!(did_tombstones(&state, &stranger).await, 1);
            unasked.push(stranger);

            mock.verify().await;
            for did in [migrated, gone, unreachable, oversized]
                .iter()
                .chain(&unasked)
            {
                clean_up(&state, did).await;
            }
        }
    }

    #[tokio::test]
    async fn a_deactivated_account_keeps_its_records() {
        for state in states().await {
            let did = unique_did();
            apply(&state, commit(&did, 1, "a", "create")).await;
            apply(&state, account(&did, 2, false, Some("deactivated"))).await;
            assert_eq!(rkeys(&state, &did).await, vec!["a"]);
            assert_eq!(
                logged(&state, "jetstream.account_inactive", &did).await,
                vec![serde_json::json!({"status": "deactivated"})]
            );
            apply(&state, account(&did, 3, true, None)).await;
            assert_eq!(rkeys(&state, &did).await, vec!["a"]);
            clean_up(&state, &did).await;
        }
    }

    /// Ruling F6: reactivation lifts the DID tombstone whatever put it
    /// there, so a backfill (or a replay) can bring the account back.
    #[tokio::test]
    async fn an_active_account_event_lifts_its_did_tombstone() {
        for state in states().await {
            for status in ["deleted", "takendown", "suspended"] {
                let did = unique_did();
                apply(&state, commit(&did, 1, "a", "create")).await;
                apply(&state, account(&did, 10, false, Some(status))).await;
                assert_eq!(did_tombstones(&state, &did).await, 1, "{status}");

                apply(&state, account(&did, 11, true, None)).await;
                assert_eq!(did_tombstones(&state, &did).await, 0, "{status}");

                // A write ordered before the old tombstone lands again.
                apply(&state, commit(&did, 5, "b", "create")).await;
                assert_eq!(rkeys(&state, &did).await, vec!["b"], "{status}");
                clean_up(&state, &did).await;
            }
        }
    }

    /// Every account event on the network arrives. One for a repo this
    /// instance holds nothing for changes nothing but the DID tombstone, and
    /// logs nothing.
    #[tokio::test]
    async fn account_events_for_unknown_dids_log_nothing() {
        for state in states().await {
            let did = unique_did();
            apply(&state, account(&did, 1, false, Some("deleted"))).await;
            apply(&state, account(&did, 2, false, Some("suspended"))).await;
            apply(&state, account(&did, 3, false, Some("deactivated"))).await;
            apply(&state, sync(&did, 4, None)).await;
            for event_type in [
                "jetstream.account_deleted",
                "jetstream.account_takendown",
                "jetstream.account_inactive",
                "jetstream.sync_purged",
            ] {
                assert!(
                    logged(&state, event_type, &did).await.is_empty(),
                    "{event_type}"
                );
            }
            assert_eq!(
                did_tombstones(&state, &did).await,
                1,
                "the tombstone still guards against a later replay"
            );
            clean_up(&state, &did).await;
        }
    }

    #[tokio::test]
    async fn a_sync_purges_tracked_records_before_the_replacements_arrive() {
        for state in states().await {
            let did = unique_did();
            apply(&state, commit(&did, 1, "a", "create")).await;
            apply(&state, commit(&did, 2, "b", "create")).await;
            apply(&state, sync(&did, 3, Some(rev_at(3)))).await;
            apply(&state, commit(&did, 4, "c", "create")).await;
            assert_eq!(rkeys(&state, &did).await, vec!["c"]);
            assert_eq!(
                logged(&state, "jetstream.sync_purged", &did).await,
                vec![serde_json::json!({"purged": 2, "rev": rev_at(3)})]
            );
            clean_up(&state, &did).await;
        }
    }

    #[tokio::test]
    async fn identity_events_change_nothing() {
        for state in states().await {
            let did = unique_did();
            apply(&state, commit(&did, 1, "a", "create")).await;
            apply(
                &state,
                Event {
                    did: did.clone(),
                    cursor: at(2),
                    kind: EventKind::Identity {
                        handle: Some("new.example.com".into()),
                    },
                },
            )
            .await;
            assert_eq!(rkeys(&state, &did).await, vec!["a"]);
            clean_up(&state, &did).await;
        }
    }

    /// A database that cannot be reached makes every kind of write event
    /// `Unavailable`, so the worker holds it rather than giving it up.
    #[tokio::test]
    async fn an_unreachable_database_is_unavailable_not_failed() {
        let state = sqlite_state().await;
        state.db.close().await;
        let did = unique_did();
        for event in [
            commit(&did, 1, "a", "create"),
            commit(&did, 2, "a", "delete"),
            account(&did, 3, false, Some("deleted")),
            account(&did, 4, false, Some("takendown")),
            account(&did, 5, false, Some("deactivated")),
            account(&did, 6, true, None),
            sync(&did, 7, None),
        ] {
            let outcome = apply_event(&state, &[NSID.to_string()], event.clone()).await;
            assert!(
                matches!(outcome, ApplyOutcome::Unavailable(_)),
                "{event:?}: {outcome:?}"
            );
        }
    }

    /// Retrying a commit that can never apply would only stall its worker,
    /// so it is skipped with one warning.
    #[tokio::test]
    async fn an_invalid_commit_is_skipped_with_one_warning() {
        for state in states().await {
            let did = unique_did();
            let mut no_body = commit(&did, 1, "a", "create");
            if let EventKind::Commit(commit) = &mut no_body.kind {
                commit.record = None;
            }
            apply(&state, no_body).await;
            let mut unknown = commit(&did, 2, "b", "create");
            if let EventKind::Commit(commit) = &mut unknown.kind {
                commit.operation = "frobnicate".into();
            }
            apply(&state, unknown).await;

            assert!(rkeys(&state, &did).await.is_empty());
            let mut details = logged(&state, "jetstream.record_invalid", &did).await;
            details.sort_by_key(|d| d["seq"].as_i64());
            assert_eq!(
                details,
                vec![
                    serde_json::json!({
                        "uri": format!("at://{did}/{NSID}/a"),
                        "seq": 1,
                        "error": "the create carries no record",
                    }),
                    serde_json::json!({
                        "uri": format!("at://{did}/{NSID}/b"),
                        "seq": 2,
                        "error": "unknown operation \"frobnicate\"",
                    }),
                ]
            );
            clean_up(&state, &did).await;
        }
    }

    #[tokio::test]
    async fn a_sync_keeps_local_writes_and_untracked_collections() {
        const OTHER: &str = "app.test.untracked";
        for state in states().await {
            let did = unique_did();
            apply(&state, commit(&did, 1, "a", "create")).await;
            insert_local(&state, &did, "local").await;
            let sql = adapt_sql(
                "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at) \
                 VALUES (?, ?, ?, 'other', '{}', '', ?, ?)",
                state.db_backend,
            );
            crate::db::query(&sql)
                .bind(format!("at://{did}/{OTHER}/other"))
                .bind(&did)
                .bind(OTHER)
                .bind(now_rfc3339())
                .bind(now_rfc3339())
                .execute(&state.db)
                .await
                .expect("insert an untracked network record");

            apply(&state, sync(&did, 2, Some(rev_at(2)))).await;

            assert_eq!(rkeys(&state, &did).await, vec!["local", "other"]);
            assert_eq!(
                logged(&state, "jetstream.sync_purged", &did).await,
                vec![serde_json::json!({"purged": 1, "rev": rev_at(2)})]
            );
            clean_up(&state, &did).await;
        }
    }

    /// Make every delete of `did`'s records fail with an ordinary (not
    /// unavailable) error. Returns the statement that undoes it.
    async fn refuse_deletes(state: &AppState, did: &str) -> Vec<String> {
        let name = format!("refuse_{}", did.replace([':', '.'], "_"));
        let (create, drop): (Vec<String>, Vec<String>) = match state.db_backend {
            crate::db::DatabaseBackend::Sqlite => (
                vec![format!(
                    "CREATE TRIGGER {name} BEFORE DELETE ON happyview_records_v2 \
                     WHEN OLD.did_id = (SELECT id FROM happyview_dids WHERE did = '{did}') \
                     BEGIN SELECT RAISE(ABORT, 'refused'); END"
                )],
                vec![format!("DROP TRIGGER {name}")],
            ),
            crate::db::DatabaseBackend::Postgres => (
                vec![
                    format!(
                        "CREATE FUNCTION {name}() RETURNS trigger LANGUAGE plpgsql AS $$ \
                         BEGIN IF OLD.did_id = (SELECT id FROM happyview_dids WHERE did = '{did}') \
                         THEN RAISE EXCEPTION 'refused'; END IF; \
                         RETURN OLD; END $$"
                    ),
                    format!(
                        "CREATE TRIGGER {name} BEFORE DELETE ON happyview_records_v2 \
                         FOR EACH ROW EXECUTE FUNCTION {name}()"
                    ),
                ],
                vec![
                    format!("DROP TRIGGER {name} ON happyview_records_v2"),
                    format!("DROP FUNCTION {name}()"),
                ],
            ),
        };
        for sql in create {
            crate::db::query(&sql)
                .execute(&state.db)
                .await
                .expect("create the trigger");
        }
        drop
    }

    /// A purge that keeps failing writes no error event per attempt; giving
    /// it up writes exactly one `jetstream.apply_failed`.
    #[tokio::test]
    async fn a_purge_that_keeps_failing_logs_one_error_when_given_up() {
        use super::super::watermark::WatermarkHandle;
        use super::super::workers::{Job, RetryPolicy, WorkerPool};

        for state in states().await {
            let did = unique_did();
            apply(&state, commit(&did, 1, "a", "create")).await;
            let undo = refuse_deletes(&state, &did).await;

            let outcome = apply_event(&state, &[], account(&did, 2, false, Some("deleted"))).await;
            assert!(matches!(outcome, ApplyOutcome::Failed(_)), "{outcome:?}");

            let (_tx, rx) = tokio::sync::watch::channel(vec![NSID.to_string()]);
            let watermark = Arc::new(WatermarkHandle::new());
            let pool = WorkerPool::spawn(
                1,
                4,
                apply_fn(state.clone(), rx),
                Arc::clone(&watermark),
                RetryPolicy {
                    delays: vec![Duration::from_millis(1); 3],
                    max_unavailable_delay: Duration::from_millis(2),
                },
                give_up_fn(state.clone()),
            );
            for event in [
                account(&did, 3, false, Some("takendown")),
                sync(&did, 4, None),
            ] {
                let ticket = watermark.begin(event.cursor);
                pool.dispatch(Job { ticket, event }).await;
            }
            tokio::time::timeout(Duration::from_secs(30), watermark.wait_idle())
                .await
                .expect("both events are given up");
            drop(pool);

            for event_type in [
                "jetstream.account_deleted",
                "jetstream.account_takendown",
                "jetstream.sync_purged",
            ] {
                assert!(
                    logged(&state, event_type, &did).await.is_empty(),
                    "{event_type}"
                );
            }
            let mut gave_up = logged(&state, "jetstream.apply_failed", &did).await;
            gave_up.sort_by_key(|d| d["seq"].as_i64());
            assert_eq!(gave_up.len(), 2, "{gave_up:?}");
            assert_eq!(gave_up[0]["kind"], "account");
            assert_eq!(gave_up[0]["status"], "takendown");
            assert_eq!(gave_up[0]["seq"], 3);
            assert_eq!(gave_up[1]["kind"], "sync");
            assert_eq!(gave_up[1]["seq"], 4);
            assert_eq!(rkeys(&state, &did).await, vec!["a"]);

            for sql in undo {
                crate::db::query(&sql)
                    .execute(&state.db)
                    .await
                    .expect("drop the trigger");
            }
            clean_up(&state, &did).await;
        }
    }

    #[tokio::test]
    async fn giving_up_logs_apply_failed() {
        for state in states().await {
            let did = unique_did();
            let give_up = give_up_fn(state.clone());
            give_up(commit(&did, 1, "a", "create"), "boom".into()).await;
            give_up(account(&did, 2, false, Some("deleted")), "bang".into()).await;
            give_up(
                Event {
                    did: did.clone(),
                    cursor: CursorPoint::V1 { time_us: BASE_US },
                    kind: EventKind::Sync { rev: None },
                },
                "pop".into(),
            )
            .await;
            let mut details = logged(&state, "jetstream.apply_failed", &did).await;
            details.sort_by_key(|d| d["error"].as_str().map(str::to_string));
            assert_eq!(
                details,
                vec![
                    serde_json::json!({
                        "kind": "account",
                        "status": "deleted",
                        "seq": 2,
                        "error": "bang",
                    }),
                    serde_json::json!({
                        "uri": format!("at://{did}/{NSID}/a"),
                        "seq": 1,
                        "error": "boom",
                    }),
                    serde_json::json!({"kind": "sync", "seq": null, "error": "pop"}),
                ]
            );
            let severity: (String,) = crate::db::query_as(&adapt_sql(
                "SELECT severity FROM happyview_event_logs WHERE event_type = 'jetstream.apply_failed' AND subject = ? LIMIT 1",
                state.db_backend,
            ))
            .bind(&did)
            .fetch_one(&state.db)
            .await
            .expect("severity");
            assert_eq!(severity.0, "error");
            clean_up(&state, &did).await;
        }
    }

    /// The `ApplyFn` reads the tracked collections when a `#sync` needs
    /// them, not before.
    #[tokio::test]
    async fn the_apply_fn_bounds_a_sync_purge_by_the_tracked_collections() {
        let state = sqlite_state().await;
        let (tx, rx) = tokio::sync::watch::channel(Vec::<String>::new());
        let apply = apply_fn(state.clone(), rx);
        let did = unique_did();
        assert_eq!(
            apply(commit(&did, 1, "a", "create")).await,
            ApplyOutcome::Applied
        );
        assert_eq!(apply(sync(&did, 2, None)).await, ApplyOutcome::Applied);
        assert_eq!(
            rkeys(&state, &did).await,
            vec!["a"],
            "nothing is tracked yet"
        );
        tx.send(vec![NSID.to_string()]).expect("send");
        assert_eq!(apply(sync(&did, 3, None)).await, ApplyOutcome::Applied);
        assert!(rkeys(&state, &did).await.is_empty());
    }

    async fn replay(state: &AppState, event: Event) {
        assert_eq!(
            apply_replayed_marker(state, &[NSID.to_string()], event).await,
            ApplyOutcome::Applied
        );
    }

    /// A network row at `rev_at(seq)`, or with no rev (a crawl row) for `None`.
    async fn put(state: &AppState, did: &str, rkey: &str, seq: Option<i64>) {
        // Bound outside the closure: the future `write_network` returns
        // borrows the slice, so it can't point at a temporary made inside.
        let prepared = [crate::record_store::PreparedRecord {
            did: did.into(),
            collection: NSID.into(),
            rkey: rkey.into(),
            record_json: serde_json::json!({ "text": rkey }).to_string(),
            cid: String::new(),
            rev: seq.map(rev_at),
        }];
        crate::db::retry_on_busy(|| state.records.write_network(&state.db, &prepared))
            .await
            .expect("put");
    }

    async fn did_tombstone_rev(state: &AppState, did: &str) -> Option<String> {
        crate::db::query_as::<(String,)>(&adapt_sql(
            "SELECT rev FROM happyview_did_tombstones WHERE did = ?",
            state.db_backend,
        ))
        .bind(did)
        .fetch_optional(&state.db)
        .await
        .expect("read tombstone")
        .map(|(rev,)| rev)
    }

    #[tokio::test]
    async fn a_replayed_takedown_keeps_records_written_after_it() {
        for state in states().await {
            let did = unique_did();
            put(&state, &did, "crawled", None).await;
            put(&state, &did, "older", Some(10)).await;
            put(&state, &did, "newer", Some(30)).await;

            replay(&state, account(&did, 20, false, Some("takendown"))).await;

            assert_eq!(rkeys(&state, &did).await, vec!["newer"]);
            assert_eq!(did_tombstone_rev(&state, &did).await, Some(rev_at(20)));
            let logged = logged(&state, "jetstream.account_takendown", &did).await;
            assert_eq!(logged.len(), 1);
            assert_eq!(logged[0]["replayed"], true);
            // The tombstone still refuses what the takedown superseded...
            put(&state, &did, "late-old", Some(15)).await;
            // ...and admits what came after it.
            put(&state, &did, "late-new", Some(25)).await;
            assert_eq!(rkeys(&state, &did).await, vec!["late-new", "newer"]);
        }
    }

    #[tokio::test]
    async fn a_replayed_sync_keeps_newer_records() {
        for state in states().await {
            let did = unique_did();
            put(&state, &did, "older", Some(10)).await;
            put(&state, &did, "newer", Some(30)).await;
            replay(&state, sync(&did, 25, Some(rev_at(20)))).await;
            assert_eq!(rkeys(&state, &did).await, vec!["newer"]);
            assert_eq!(
                did_tombstone_rev(&state, &did).await,
                None,
                "a sync leaves no tombstone"
            );
        }
    }

    #[tokio::test]
    async fn a_replayed_sync_without_a_rev_acts_as_of_its_own_time() {
        for state in states().await {
            let did = unique_did();
            put(&state, &did, "crawled", None).await;
            put(&state, &did, "older", Some(10)).await;
            put(&state, &did, "newer", Some(30)).await;
            replay(&state, sync(&did, 20, None)).await;
            assert_eq!(rkeys(&state, &did).await, vec!["newer"]);
        }
    }

    #[tokio::test]
    async fn a_replayed_marker_keeps_a_record_at_its_own_rev() {
        // "Before" is strict: a row with the marker's rev is not older.
        for state in states().await {
            let did = unique_did();
            put(&state, &did, "same", Some(20)).await;
            put(&state, &did, "older", Some(19)).await;
            replay(&state, sync(&did, 25, Some(rev_at(20)))).await;
            assert_eq!(rkeys(&state, &did).await, vec!["same"]);
        }
    }

    #[tokio::test]
    async fn a_replayed_reactivation_does_not_lift_a_newer_tombstone() {
        for state in states().await {
            let did = unique_did();
            apply(&state, account(&did, 30, false, Some("takendown"))).await;
            assert_eq!(did_tombstone_rev(&state, &did).await, Some(rev_at(30)));
            replay(&state, account(&did, 20, true, None)).await;
            assert_eq!(
                did_tombstone_rev(&state, &did).await,
                Some(rev_at(30)),
                "older reactivation"
            );
            replay(&state, account(&did, 30, true, None)).await;
            assert_eq!(
                did_tombstone_rev(&state, &did).await,
                Some(rev_at(30)),
                "same-rev reactivation"
            );
            replay(&state, account(&did, 40, true, None)).await;
            assert_eq!(
                did_tombstone_rev(&state, &did).await,
                None,
                "newer reactivation"
            );
        }
    }

    #[tokio::test]
    async fn a_replayed_marker_never_lowers_a_newer_tombstone() {
        for state in states().await {
            let did = unique_did();
            apply(&state, account(&did, 30, false, Some("deleted"))).await;
            replay(&state, account(&did, 20, false, Some("suspended"))).await;
            assert_eq!(did_tombstone_rev(&state, &did).await, Some(rev_at(30)));
        }
    }

    #[tokio::test]
    async fn a_replayed_deactivation_changes_nothing() {
        for state in states().await {
            let did = unique_did();
            put(&state, &did, "kept", Some(10)).await;
            replay(&state, account(&did, 20, false, Some("deactivated"))).await;
            assert_eq!(rkeys(&state, &did).await, vec!["kept"]);
            assert!(
                logged(&state, "jetstream.account_inactive", &did)
                    .await
                    .is_empty()
            );
        }
    }

    #[tokio::test]
    async fn markers_commute_with_the_commits_they_follow() {
        // The fold applies commit sweeps before marker sweeps (P1); the
        // result must match seq order.
        for state in states().await {
            let (seq_order, sweep_order) = (unique_did(), unique_did());
            let events = |did: &str| {
                vec![
                    commit(did, 10, "a", "create"),
                    account(did, 20, false, Some("takendown")),
                    account(did, 30, true, None),
                    commit(did, 40, "b", "create"),
                ]
            };
            for event in events(&seq_order) {
                match event.kind {
                    EventKind::Commit(_) => apply(&state, event).await,
                    _ => replay(&state, event).await,
                }
            }
            let (commits, markers): (Vec<_>, Vec<_>) = events(&sweep_order)
                .into_iter()
                .partition(|e| matches!(e.kind, EventKind::Commit(_)));
            for event in commits {
                apply(&state, event).await;
            }
            for event in markers {
                replay(&state, event).await;
            }
            assert_eq!(rkeys(&state, &seq_order).await, vec!["b"]);
            assert_eq!(rkeys(&state, &sweep_order).await, vec!["b"]);
        }
    }

    #[tokio::test]
    async fn live_markers_still_purge_everything() {
        for state in states().await {
            let did = unique_did();
            put(&state, &did, "newer", Some(30)).await;
            apply(&state, account(&did, 20, false, Some("takendown"))).await;
            assert!(rkeys(&state, &did).await.is_empty());
        }
    }
}
