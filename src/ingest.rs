//! The one ingest pipeline. Every record source (live Jetstream, the crawl,
//! archive replay, a dead-letter retry) hands its events here: the
//! tracked-collection check, the CID check, the record hook, the write, label
//! fetches, `com.atproto.lexicon.schema` handling, deletes, and account and
//! sync purges with their tombstones.
//!
//! Ingest counters (`IngestCounters`) are not touched here: a source counts
//! what it received, matched and skipped itself (ruling F1).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::Value;

pub use happyview_provider::{
    Action, BatchOutcome, IngestOutcome, Origin, RecordEvent, RecordKey, SourceEvent,
};
use happyview_provider::{Delta, RecordStorage};

use crate::AppState;
use crate::db::now_rfc3339;
use crate::event_log::{EventLog, Severity, log_event};
use crate::lexicon::LexiconType;
use crate::lua::{RecordEventPayload, RecordHookOutcome, ResolvedScript};
use crate::record_handler::LEXICON_SCHEMA_COLLECTION;
use crate::record_store::PreparedRecord;

/// How long asking an account's PDS for its status may take before a purge
/// goes ahead on the relay's word.
const ACCOUNT_STATUS_TIMEOUT: Duration = Duration::from_secs(5);
/// The largest `getRepoStatus` answer read; a real one is under 200 bytes.
pub(crate) const REPO_STATUS_MAX_BYTES: usize = 64 << 10;

/// What the record hook decided to store for one create or update.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum HookedBody {
    Store(Value),
    /// The hook returned nothing; logged (when verbose) as `record.skipped`.
    Skip,
    /// The body does not hash to its claimed CID; logged as
    /// `record.cid_mismatch`.
    CidMismatch,
}

/// An outcome, plus the database error behind a failed account or sync event
/// or a failed batch write, for a caller that reports it (the Jetstream
/// worker, the crawl, the replay fold).
pub(crate) struct Applied {
    pub outcome: IngestOutcome,
    pub error: Option<String>,
}

impl Applied {
    fn ok(outcome: IngestOutcome) -> Self {
        Self {
            outcome,
            error: None,
        }
    }

    /// `Unavailable` when `e` means the database could not be reached,
    /// otherwise `Errored`.
    fn failed(e: &sqlx::Error) -> Self {
        Self {
            outcome: if crate::db::is_unavailable(e) {
                IngestOutcome::Unavailable
            } else {
                IngestOutcome::Errored
            },
            error: Some(e.to_string()),
        }
    }
}

/// What the record hook decided for one create or update, before it
/// becomes a body to store (local) or a delta (claimed collection).
enum Decision {
    Proceed,
    Replace(Value),
    Skip,
    CidMismatch,
}

/// Where a record's hook outcome comes from.
enum Hook {
    /// Run the record hook (fail-open, dead-lettered on failure).
    Run,
    /// Already run once by a dead-letter retry.
    Given(RecordHookOutcome),
}

/// Apply one event.
pub async fn apply(state: &AppState, event: SourceEvent) -> IngestOutcome {
    apply_in(state, event).await.outcome
}

/// Apply events in order. Runs of consecutive creates and updates are hooked
/// one by one and written in one transaction; any other event ends the run
/// and goes through [`apply`]. One outcome per event, in order.
pub async fn apply_batch(state: &AppState, events: Vec<SourceEvent>) -> BatchOutcome {
    apply_batch_in(state, events).await.0
}

/// [`apply`], with the error text behind a failed account or sync event.
pub(crate) async fn apply_in(state: &AppState, event: SourceEvent) -> Applied {
    match event {
        SourceEvent::Record(ev) => Applied::ok(apply_record(state, &ev, Hook::Run).await),
        SourceEvent::Account {
            did,
            active,
            status,
            as_of_us,
            seq,
            time_us,
            collections,
        } => {
            account_event(
                state,
                &did,
                active,
                status.as_deref(),
                as_of_us,
                seq,
                time_us,
                collections.as_deref(),
            )
            .await
        }
        SourceEvent::Sync {
            did,
            rev,
            as_of_us,
            seq: _,
            time_us: _,
            collections,
        } => {
            let tracked = match collections {
                Some(collections) => collections,
                None => state.collections_tx.borrow().clone(),
            };
            // A replayed sync acts as of its own rev, or its own time when
            // it carries none (P2).
            let before = as_of_us.map(|t| rev.clone().unwrap_or_else(|| tid(t)));
            crate::db::with_change_reason(
                Some("purge:sync".to_string()),
                repo_resynced(state, &did, rev.as_deref(), &tracked, before.as_deref()),
            )
            .await
        }
        SourceEvent::Identity { did, handle } => {
            // HappyView caches no DID documents or handles, so there is
            // nothing to refresh.
            tracing::debug!(did, ?handle, "jetstream identity event");
            Applied::ok(IngestOutcome::Skipped)
        }
    }
}

/// [`apply_batch`], with the error text of a failed write. A failed write
/// stops the batch: that event's run and every event after it take the
/// failure's outcome, so nothing after a failure applies out of order.
pub(crate) async fn apply_batch_in(
    state: &AppState,
    events: Vec<SourceEvent>,
) -> (BatchOutcome, Option<String>) {
    let total = events.len();
    let mut out = BatchOutcome {
        outcomes: Vec::with_capacity(total),
        changed: Vec::with_capacity(total),
    };
    let mut run: Vec<(usize, RecordEvent, PreparedRecord)> = Vec::new();
    let mut tracked: HashMap<String, bool> = HashMap::new();

    for event in events {
        // A claimed collection's events are written as deltas, one by one,
        // through `apply_in`.
        if let SourceEvent::Record(ev) = &event
            && matches!(ev.action, Action::Create | Action::Update)
            && ev.key.collection != LEXICON_SCHEMA_COLLECTION
            && state.records.external_route(&ev.key.collection).is_none()
            && let Some(body) = &ev.body
        {
            let is_tracked = match tracked.get(&ev.key.collection) {
                Some(known) => *known,
                None => {
                    let known = is_tracked(state, &ev.key.collection).await;
                    tracked.insert(ev.key.collection.clone(), known);
                    known
                }
            };
            if is_tracked {
                let hooked = hook_body(state, ev, body).await;
                let SourceEvent::Record(ev) = event else {
                    unreachable!()
                };
                match hooked {
                    HookedBody::Store(body) => {
                        let prepared = prepare(&ev, &body);
                        run.push((out.outcomes.len(), ev, prepared));
                        // `changed` is settled when the run is written.
                        out.outcomes.push(IngestOutcome::Matched);
                        out.changed.push(false);
                    }
                    HookedBody::Skip | HookedBody::CidMismatch => {
                        out.outcomes.push(IngestOutcome::Skipped);
                        out.changed.push(false);
                    }
                }
                continue;
            }
        }
        if let Err((outcome, error)) = write_run(state, &mut run, &mut out).await {
            fill_failed(&mut out, total, outcome);
            return (out, Some(error));
        }
        let applied = apply_in(state, event).await;
        out.outcomes.push(applied.outcome);
        out.changed.push(false);
    }
    if let Err((outcome, error)) = write_run(state, &mut run, &mut out).await {
        fill_failed(&mut out, total, outcome);
        return (out, Some(error));
    }
    (out, None)
}

/// Give the not-yet-applied tail of a batch the failure's outcome.
fn fill_failed(out: &mut BatchOutcome, total: usize, outcome: IngestOutcome) {
    out.outcomes.resize(total, outcome);
    out.changed.resize(total, false);
}

/// Write a run of hooked creates and updates in one transaction, settling
/// their placeholder outcomes in `out` (each run entry carries its slot). On
/// failure `out` is cut back to the run's first slot and the failure's
/// outcome and error are returned for the caller to fill the tail with.
async fn write_run(
    state: &AppState,
    run: &mut Vec<(usize, RecordEvent, PreparedRecord)>,
    out: &mut BatchOutcome,
) -> Result<(), (IngestOutcome, String)> {
    if run.is_empty() {
        return Ok(());
    }
    let run = std::mem::take(run);
    let backfill = matches!(run[0].1.origin, Origin::Backfill { .. });
    let pool = if backfill {
        &state.backfill_db
    } else {
        &state.db
    };
    let prepared: Vec<PreparedRecord> = run.iter().map(|(_, _, p)| p.clone()).collect();
    let written = crate::db::with_change_reason(
        backfill.then(|| "backfill".to_string()),
        crate::db::retry_on_busy(|| state.records.write_network(pool, &prepared)),
    )
    .await;
    let changed = match written {
        Ok(changed) => changed,
        Err(e) => {
            tracing::warn!(
                records = prepared.len(),
                "failed to write a batch of records: {e}"
            );
            out.outcomes.truncate(run[0].0);
            out.changed.truncate(run[0].0);
            return Err((failed_write_outcome(&e), e.to_string()));
        }
    };
    let mut changed: HashSet<String> = changed.into_iter().map(|c| c.uri).collect();

    // A URI written twice in the run counts as changed once, on its last
    // copy (the one stored).
    let mut fresh = Vec::new();
    for (slot, ev, _) in run.iter().rev() {
        let uri = ev.key.uri();
        if changed.remove(&uri) {
            out.changed[*slot] = true;
            fresh.push((ev, uri));
        }
    }
    fresh.reverse();
    let verbose = state.verbose_event_logging.load(Ordering::Relaxed);
    let mut uris = Vec::with_capacity(fresh.len());
    for (ev, uri) in fresh {
        if verbose {
            log_record(state, "record.created", Severity::Info, ev, &uri, None).await;
        }
        uris.push(uri);
    }
    fetch_labels(state, &uris).await;
    Ok(())
}

/// A dead-letter retry: run the dead letter's script once, then apply the
/// event with what it returned. `Err` with the script's error when it still
/// fails; nothing is applied or dead-lettered again then.
pub(crate) async fn apply_retry(
    state: &AppState,
    script: &ResolvedScript,
    ev: &RecordEvent,
) -> Result<IngestOutcome, String> {
    let uri = ev.key.uri();
    let hook = crate::lua::run_record_event_once(
        state,
        script,
        RecordEventPayload {
            nsid: &ev.key.collection,
            action: ev.action.as_str(),
            uri: &uri,
            did: &ev.key.did,
            rkey: &ev.key.rkey,
            record: ev.body.as_ref(),
        },
    )
    .await?;
    Ok(apply_record(state, ev, Hook::Given(hook)).await)
}

/// Whether `collection` is a registered record-type lexicon.
pub(crate) async fn is_tracked(state: &AppState, collection: &str) -> bool {
    state
        .lexicons
        .get(collection)
        .await
        .is_some_and(|lex| lex.lexicon_type == LexiconType::Record)
}

/// Run the record hook for one create/update and say what to store. The CID
/// check comes first: a body that does not hash to its claimed CID is never
/// hooked or stored. An event without a CID skips the check.
pub(crate) async fn hook_body(state: &AppState, ev: &RecordEvent, body: &Value) -> HookedBody {
    hook_body_with(state, ev, body, Hook::Run).await
}

async fn hook_body_with(
    state: &AppState,
    ev: &RecordEvent,
    body: &Value,
    hook: Hook,
) -> HookedBody {
    match hook_decision(state, ev, body, hook).await {
        Decision::Proceed => HookedBody::Store(body.clone()),
        Decision::Replace(v) => HookedBody::Store(v),
        Decision::Skip => HookedBody::Skip,
        Decision::CidMismatch => HookedBody::CidMismatch,
    }
}

async fn hook_decision(state: &AppState, ev: &RecordEvent, body: &Value, hook: Hook) -> Decision {
    let uri = ev.key.uri();
    let cid = ev.cid.as_deref().unwrap_or_default();

    // Reject records whose claimed CID doesn't match their content
    // (security review L9). A hostile source (a PDS reached by crawl is
    // attacker-controllable through the DID document) could otherwise store
    // a record under a mismatched CID. `Skipped` (no/unencodable CID)
    // proceeds unchanged.
    if crate::cid_verify::verify_record_cid(cid, body) == crate::cid_verify::CidCheck::Mismatch {
        if matches!(ev.origin, Origin::Backfill { .. }) {
            tracing::warn!(
                collection = ev.key.collection,
                did = ev.key.did,
                rkey = ev.key.rkey,
                claimed_cid = %cid,
                "record content does not match claimed CID, skipping"
            );
        }
        log_event(
            &state.db,
            EventLog {
                event_type: "record.cid_mismatch".to_string(),
                severity: Severity::Warn,
                actor_did: None,
                subject: Some(uri.clone()),
                detail: serde_json::json!({
                    "collection": ev.key.collection,
                    "did": ev.key.did,
                    "rkey": ev.key.rkey,
                    "claimed_cid": cid,
                    "reason": "record content does not match claimed CID",
                }),
            },
            state.db_backend,
        )
        .await;
        return Decision::CidMismatch;
    }

    // Run record-event script (if any) before storing. The script's
    // return value determines what gets written:
    //   Skip → skip indexing entirely
    //   Replace(record) → upsert with that record body
    //   Proceed → upsert with the record as it arrived
    // The dispatcher cascades `record.<action>:<nsid>` →
    // `record.index:<nsid>`; failures are dead-lettered fail-open.
    let outcome = match hook {
        Hook::Run => {
            crate::lua::run_record_event_script(
                state,
                RecordEventPayload {
                    nsid: &ev.key.collection,
                    action: ev.action.as_str(),
                    uri: &uri,
                    did: &ev.key.did,
                    rkey: &ev.key.rkey,
                    record: Some(body),
                },
            )
            .await
        }
        Hook::Given(outcome) => outcome,
    };
    match outcome {
        RecordHookOutcome::Skip => {
            log_skipped(state, ev, &uri).await;
            Decision::Skip
        }
        RecordHookOutcome::Replace(v) => Decision::Replace(v),
        RecordHookOutcome::Proceed => Decision::Proceed,
    }
}

/// `record.skipped`, gated like `record.created`/`record.deleted`: a
/// filtering script skips far more records than it keeps, so logging every
/// skip writes a row per discarded firehose record.
async fn log_skipped(state: &AppState, ev: &RecordEvent, uri: &str) {
    if state.verbose_event_logging.load(Ordering::Relaxed) {
        log_event(
            &state.db,
            EventLog {
                event_type: "record.skipped".to_string(),
                severity: Severity::Info,
                actor_did: None,
                subject: Some(uri.to_string()),
                detail: serde_json::json!({
                    "collection": ev.key.collection,
                    "did": ev.key.did,
                    "rkey": ev.key.rkey,
                    "reason": "script returned nil",
                }),
            },
            state.db_backend,
        )
        .await;
    }
}

/// `record.created` / `record.deleted` for `ev`, with the error when there
/// is one.
async fn log_record(
    state: &AppState,
    event_type: &str,
    severity: Severity,
    ev: &RecordEvent,
    uri: &str,
    error: Option<&sqlx::Error>,
) {
    let mut detail = serde_json::json!({
        "collection": ev.key.collection,
        "did": ev.key.did,
        "rkey": ev.key.rkey,
    });
    if let Some(e) = error {
        detail["error"] = e.to_string().into();
    }
    log_event(
        &state.db,
        EventLog {
            event_type: event_type.to_string(),
            severity,
            actor_did: None,
            subject: Some(uri.to_string()),
            detail,
        },
        state.db_backend,
    )
    .await;
}

fn prepare(ev: &RecordEvent, body: &Value) -> PreparedRecord {
    PreparedRecord {
        did: ev.key.did.clone(),
        collection: ev.key.collection.clone(),
        rkey: ev.key.rkey.clone(),
        record_json: serde_json::to_string(body).unwrap_or_default(),
        cid: ev.cid.clone().unwrap_or_default(),
        rev: ev.rev.clone(),
    }
}

/// Fetch labels for freshly written records, when any labeler is subscribed.
async fn fetch_labels(state: &AppState, uris: &[String]) {
    if uris.is_empty() || !crate::labeler::has_active_subscriptions(state).await {
        return;
    }
    let shared = Arc::new(state.clone());
    for uri in uris {
        crate::labeler::backfill_labels_for_uri(Arc::clone(&shared), uri.clone());
    }
}

/// One record event: upsert or delete the record, run its hook, or handle a
/// lexicon schema event.
async fn apply_record(state: &AppState, ev: &RecordEvent, hook: Hook) -> IngestOutcome {
    let db = &state.db;
    let uri = ev.key.uri();

    // Handle lexicon schema events for tracked network lexicons.
    if ev.key.collection == LEXICON_SCHEMA_COLLECTION {
        return lexicon_schema_event(
            state,
            &ev.key.did,
            &ev.key.rkey,
            ev.action.as_str(),
            ev.body.as_ref(),
        )
        .await;
    }

    // Skip records whose collection is not tracked by a registered record-type lexicon.
    if !is_tracked(state, &ev.key.collection).await {
        tracing::debug!(
            collection = %ev.key.collection,
            "skipping record for untracked collection"
        );
        return IngestOutcome::Skipped;
    }

    if let Some(ext) = state.records.external_route(&ev.key.collection) {
        return apply_claimed(state, ext.as_ref(), ev, hook).await;
    }

    let reason = matches!(ev.origin, Origin::Backfill { .. }).then(|| "backfill".to_string());
    match ev.action {
        Action::Create | Action::Update => {
            let Some(rec) = &ev.body else {
                return IngestOutcome::Invalid;
            };
            let rec_to_store = match hook_body_with(state, ev, rec, hook).await {
                HookedBody::Store(v) => v,
                // The delivery this retries failed open and stored the body
                // as it arrived; the hook now says it should not be there.
                HookedBody::Skip if ev.origin == Origin::Retry => {
                    return remove_failed_open(state, ev, rec, &uri).await;
                }
                HookedBody::Skip | HookedBody::CidMismatch => return IngestOutcome::Skipped,
            };

            let prepared = prepare(ev, &rec_to_store);
            let written = crate::db::with_change_reason(
                reason,
                crate::db::retry_on_busy(|| {
                    state
                        .records
                        .write_network(db, std::slice::from_ref(&prepared))
                }),
            )
            .await
            .map(|changed| !changed.is_empty());
            match written {
                Ok(changed) => {
                    // An identical redelivery changed nothing, so there is
                    // nothing new to log or to fetch labels for.
                    if changed {
                        if state.verbose_event_logging.load(Ordering::Relaxed) {
                            log_record(state, "record.created", Severity::Info, ev, &uri, None)
                                .await;
                        }
                        if crate::labeler::has_active_subscriptions(state).await {
                            crate::labeler::backfill_labels_for_uri(
                                Arc::new(state.clone()),
                                uri.clone(),
                            );
                        }
                    }
                    IngestOutcome::Matched
                }
                Err(e) => {
                    tracing::warn!(uri = %uri, "failed to upsert record: {e}");
                    log_record(state, "record.created", Severity::Error, ev, &uri, Some(&e)).await;
                    failed_write_outcome(&e)
                }
            }
        }
        Action::Delete => {
            // Run record-event script (if any) before deleting. Only a
            // script that actually ran and returned `nil` aborts the
            // delete — no script, or a dead-lettered one, proceeds.
            let hook_result = delete_hook(state, ev, &uri, hook).await;
            if hook_result == RecordHookOutcome::Skip {
                // Gated for the same reason as the create path.
                log_skipped(state, ev, &uri).await;
                return IngestOutcome::Skipped;
            }

            let now = now_rfc3339();
            let backfill_running = state.source.backfill_running(&ev.key.collection).await;
            let deleted = crate::db::with_change_reason(
                reason,
                crate::db::retry_on_busy(|| {
                    state.records.delete_network(
                        db,
                        &uri,
                        &ev.key.did,
                        &ev.key.collection,
                        ev.rev.as_deref(),
                        &now,
                        backfill_running,
                    )
                }),
            )
            .await;
            match deleted {
                Ok(_) => {
                    if state.verbose_event_logging.load(Ordering::Relaxed) {
                        log_record(state, "record.deleted", Severity::Info, ev, &uri, None).await;
                    }
                    IngestOutcome::Matched
                }
                Err(e) => {
                    tracing::warn!(uri = %uri, "failed to delete record: {e}");
                    log_record(state, "record.deleted", Severity::Error, ev, &uri, Some(&e)).await;
                    failed_write_outcome(&e)
                }
            }
        }
    }
}

/// The delete hook's outcome for `ev`.
async fn delete_hook(
    state: &AppState,
    ev: &RecordEvent,
    uri: &str,
    hook: Hook,
) -> RecordHookOutcome {
    match hook {
        Hook::Run => {
            crate::lua::run_record_event_script(
                state,
                RecordEventPayload {
                    nsid: &ev.key.collection,
                    action: "delete",
                    uri,
                    did: &ev.key.did,
                    rkey: &ev.key.rkey,
                    record: ev.body.as_ref(),
                },
            )
            .await
        }
        Hook::Given(outcome) => outcome,
    }
}

/// A record event for a collection an external storage claims. Its body is
/// already in that storage (`stored`); the hook runs as for a local record
/// and only its result is written, as a delta on this instance's view:
/// Proceed, Replace or Hide (a skip). A delete clears the delta; its hook
/// runs, but cannot keep a record the storage no longer has. An event that is
/// not `stored` came from somewhere other than the claiming store's feed and
/// is refused, so nothing for a claimed collection is written locally.
async fn apply_claimed(
    state: &AppState,
    ext: &dyn RecordStorage,
    ev: &RecordEvent,
    hook: Hook,
) -> IngestOutcome {
    let uri = ev.key.uri();
    if !ev.stored {
        tracing::warn!(
            uri = %uri,
            "a record of a collection stored outside this instance arrived from another source"
        );
        log_record(
            state,
            "record.unstored_claimed",
            Severity::Warn,
            ev,
            &uri,
            None,
        )
        .await;
        return IngestOutcome::Invalid;
    }
    let (delta, outcome) = match ev.action {
        Action::Create | Action::Update => {
            let Some(body) = &ev.body else {
                return IngestOutcome::Invalid;
            };
            match hook_decision(state, ev, body, hook).await {
                Decision::Proceed => (Delta::Proceed, IngestOutcome::Matched),
                Decision::Replace(replaced) => (
                    Delta::Replace {
                        // A retried event carries no CID: name the
                        // replaced body by its own.
                        cid: ev.cid.clone().unwrap_or_else(|| {
                            crate::cid_verify::compute_record_cid(&replaced)
                                .map(|cid| cid.to_string())
                                .unwrap_or_default()
                        }),
                        rev: ev.rev.clone(),
                        body: serde_json::to_string(&replaced).unwrap_or_default(),
                    },
                    IngestOutcome::Matched,
                ),
                Decision::Skip | Decision::CidMismatch => (Delta::Hide, IngestOutcome::Skipped),
            }
        }
        Action::Delete => {
            if delete_hook(state, ev, &uri, hook).await == RecordHookOutcome::Skip
                && state.verbose_event_logging.load(Ordering::Relaxed)
            {
                log_event(
                    &state.db,
                    EventLog {
                        event_type: "record.skip_ignored".to_string(),
                        severity: Severity::Info,
                        actor_did: None,
                        subject: Some(uri.clone()),
                        detail: serde_json::json!({
                            "collection": ev.key.collection,
                            "did": ev.key.did,
                            "rkey": ev.key.rkey,
                            "reason": "script returned nil for a record deleted from the store that holds it",
                        }),
                    },
                    state.db_backend,
                )
                .await;
            }
            (Delta::Proceed, IngestOutcome::Matched)
        }
    };
    let event_type = match ev.action {
        Action::Delete => "record.deleted",
        Action::Create | Action::Update => "record.created",
    };
    // A dead-letter retry carries no rev. Its delta applies only to the
    // version the dead letter was made from: while the claiming store still
    // holds that body, and at that version's rev, so the store's own rev
    // check guards it too. A version that changed or went since is left
    // alone, as the local path's `delete_network_if_body` leaves it.
    let mut for_rev = ev.rev.clone();
    if ev.origin == Origin::Retry && ev.action != Action::Delete {
        let route = ext.read_route(&ev.key.collection);
        match crate::record_store::RecordStore::stored_version_on(&route, &uri).await {
            Ok(Some((stored, rev)))
                if serde_json::from_str::<Value>(&stored).ok().as_ref() == ev.body.as_ref() =>
            {
                for_rev = rev;
            }
            Ok(_) => {
                tracing::debug!(uri = %uri, "a retried delta's record changed since; dropped");
                return outcome;
            }
            Err(e) => {
                tracing::warn!(uri = %uri, "failed to read a retried record: {e}");
                log_record(state, event_type, Severity::Error, ev, &uri, Some(&e)).await;
                return if ext.unavailable(&e) {
                    IngestOutcome::Unavailable
                } else {
                    failed_write_outcome(&e)
                };
            }
        }
    }
    match ext.apply_delta(&ev.key, delta, for_rev.as_deref()).await {
        Ok(()) => {
            if outcome == IngestOutcome::Matched && ev.action != Action::Delete {
                fetch_labels(state, std::slice::from_ref(&uri)).await;
            }
            outcome
        }
        Err(e) => {
            tracing::warn!(uri = %uri, "failed to apply a record's delta: {e}");
            log_record(state, event_type, Severity::Error, ev, &uri, Some(&e)).await;
            if ext.unavailable(&e) {
                IngestOutcome::Unavailable
            } else {
                failed_write_outcome(&e)
            }
        }
    }
}

/// A dead-letter retry whose hook now skips its create or update: remove the
/// row its failed-open delivery stored, while that row still holds the
/// dead letter's body (a newer write since is left alone). `Skipped`, or a
/// failed delete's outcome.
async fn remove_failed_open(
    state: &AppState,
    ev: &RecordEvent,
    body: &Value,
    uri: &str,
) -> IngestOutcome {
    let now = now_rfc3339();
    let removed = crate::db::retry_on_busy(|| {
        state.records.delete_network_if_body(
            &state.db,
            uri,
            &ev.key.did,
            &ev.key.collection,
            body,
            &now,
        )
    })
    .await;
    match removed {
        Ok(removed) => {
            if removed && state.verbose_event_logging.load(Ordering::Relaxed) {
                log_record(state, "record.deleted", Severity::Info, ev, uri, None).await;
            }
            IngestOutcome::Skipped
        }
        Err(e) => {
            tracing::warn!(uri = %uri, "failed to delete record: {e}");
            log_record(state, "record.deleted", Severity::Error, ev, uri, Some(&e)).await;
            failed_write_outcome(&e)
        }
    }
}

/// `Unavailable` when `e` means the database could not be reached, `Invalid`
/// when it refused the data itself, otherwise `Errored`.
pub(crate) fn failed_write_outcome(e: &sqlx::Error) -> IngestOutcome {
    if crate::db::is_unavailable(e) {
        IngestOutcome::Unavailable
    } else if crate::db::is_invalid_data(e) {
        IngestOutcome::Invalid
    } else {
        IngestOutcome::Errored
    }
}

/// Handle a `com.atproto.lexicon.schema` record event for tracked network
/// lexicons: `nsid` is the record's rkey, `action` its operation as the
/// source spelled it. `SchemaEvent` when it was applied or is not one
/// HappyView tracks; a database error is classified like a record write's.
pub(crate) async fn lexicon_schema_event(
    state: &AppState,
    did: &str,
    nsid: &str,
    action: &str,
    body: Option<&Value>,
) -> IngestOutcome {
    let lexicons = &state.lexicons;

    // Only a tracked network lexicon, and only from the authority it resolved to.
    let Some(tracked) = lexicons.get_network(nsid).await else {
        return IngestOutcome::SchemaEvent;
    };
    if tracked.authority_did.as_deref() != Some(did) {
        return IngestOutcome::SchemaEvent;
    }

    let failed = |e: crate::catalog::NetworkWriteError| match e {
        crate::catalog::NetworkWriteError::Invalid(message) => {
            tracing::warn!(nsid, "failed to parse lexicon schema event: {message}");
            IngestOutcome::Invalid
        }
        // Memory is left alone too, so it and the table still agree. The
        // outcome says whether the caller should retry this event (the
        // Jetstream worker does); nothing else will.
        crate::catalog::NetworkWriteError::Db(e) => {
            tracing::warn!(nsid, error = %e, "failed to apply lexicon schema event");
            failed_write_outcome(&e)
        }
    };

    match action {
        "create" | "update" => {
            let Some(rec) = body else {
                return IngestOutcome::Invalid;
            };
            match lexicons
                .refresh_network(&state.db, state.db_backend, nsid, rec.clone())
                .await
            {
                Ok(Some(_)) => {
                    tracing::info!(nsid, "updated network lexicon from network event");
                    if lexicons
                        .get_network(nsid)
                        .await
                        .is_some_and(|l| l.parsed.lexicon_type == LexiconType::Record)
                    {
                        let _ = state
                            .collections_tx
                            .send_replace(lexicons.get_record_collections().await);
                    }
                }
                Ok(None) => {}
                Err(e) => return failed(e),
            }
        }
        "delete" => match lexicons
            .remove_network(&state.db, state.db_backend, nsid)
            .await
        {
            Ok(true) => {
                tracing::info!(nsid, "removed network lexicon from network delete event");
                let _ = state
                    .collections_tx
                    .send_replace(lexicons.get_record_collections().await);
            }
            Ok(false) => {}
            Err(e) => return failed(e),
        },
        _ => return IngestOutcome::Invalid,
    }
    IngestOutcome::SchemaEvent
}

// ---------------------------------------------------------------------------
// Account, sync and identity events
// ---------------------------------------------------------------------------
//
// With no `kinds` filter, account, identity and sync events arrive for every
// repo on the network, not only those this instance holds, so nothing here
// logs unless it found or changed records for the DID.

fn tid(time_us: i64) -> String {
    crate::tid::tid_from_unix_microseconds(time_us)
}

async fn log(state: &AppState, event_type: &str, severity: Severity, did: &str, detail: Value) {
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

/// An account event. `as_of_us` set: a marker replayed from the archive,
/// acting as of its own time (plan ruling P2): it purges only rows older than
/// itself and never lifts or lowers a newer tombstone, so applying it after
/// the commits of the same range leaves what seq order would, and live
/// writes made since survive. `None`: the live path.
#[allow(clippy::too_many_arguments)]
async fn account_event(
    state: &AppState,
    did: &str,
    active: bool,
    status: Option<&str>,
    as_of_us: Option<i64>,
    seq: Option<i64>,
    time_us: Option<i64>,
    collections: Option<&[String]>,
) -> Applied {
    let before = as_of_us.map(tid);
    // The DID tombstone's rev: when the source saw the event.
    let witnessed = time_us
        .or(as_of_us)
        .unwrap_or_else(|| chrono::Utc::now().timestamp_micros());
    match (active, status) {
        (false, Some("deleted")) => {
            crate::db::with_change_reason(
                Some("purge:deleted".to_string()),
                account_purged(
                    state,
                    did,
                    seq,
                    witnessed,
                    "jetstream.account_deleted",
                    None,
                    before.as_deref(),
                    collections,
                ),
            )
            .await
        }
        (false, Some(status @ ("takendown" | "suspended"))) => {
            crate::db::with_change_reason(
                Some(format!("purge:{status}")),
                account_purged(
                    state,
                    did,
                    seq,
                    witnessed,
                    "jetstream.account_takendown",
                    Some(status),
                    before.as_deref(),
                    collections,
                ),
            )
            .await
        }
        // Historical deactivations aren't news.
        (false, _) if as_of_us.is_some() => Applied::ok(IngestOutcome::Matched),
        (false, status) => account_inactive(state, did, status).await,
        (true, _) => account_active(state, did, before.as_deref()).await,
    }
}

/// `active=false` with `status=deleted`, `takendown` or `suspended` (Decision
/// A1: a takedown is treated like a deletion; a reinstated account's records
/// come back only through a new backfill). Write a DID tombstone first, so a
/// backfill write racing the purge is refused, then purge the DID's network
/// records; local writes awaiting their echo stay. Account events carry no
/// repo rev, so the tombstone's rev is a TID of when the source witnessed the
/// event.
///
/// A deletion is what the emitting host says, and a relay can still be
/// relaying a host the account has migrated away from. So for a deletion
/// (`status` `None`) of an account this instance holds records for, its
/// current PDS is asked first (rulings FR2 and E1): if that says the account
/// is active, nothing is purged or tombstoned, and
/// `jetstream.account_status_mismatch` is logged. If the PDS cannot be asked,
/// the relay is trusted. A takedown or suspension is purged on the relay's
/// word: the relay may have imposed it, and the account's PDS cannot see it.
///
/// The tombstone's transaction holds the DID's lock, and `purge_did_records`
/// takes it again per batch, so that transaction commits before the purge
/// starts.
#[allow(clippy::too_many_arguments)]
async fn account_purged(
    state: &AppState,
    did: &str,
    seq: Option<i64>,
    witnessed_us: i64,
    event_type: &str,
    status: Option<&str>,
    before: Option<&str>,
    collections: Option<&[String]>,
) -> Applied {
    let backend = state.db_backend;
    let deleted = status.is_none();
    let held = deleted
        && match holds_records(state, did).await {
            Ok(held) => held,
            Err(e) => {
                tracing::warn!(did, error = %e, "failed to check an account's records");
                return Applied::failed(&e);
            }
        };
    if held && let Some(pds) = active_at_pds(state, did).await {
        let status = "deleted";
        tracing::warn!(
            did,
            status,
            pds,
            "the account's PDS says it is active; not purging"
        );
        log(
            state,
            "jetstream.account_status_mismatch",
            Severity::Warn,
            did,
            serde_json::json!({ "status": status, "pds": pds, "seq": seq }),
        )
        .await;
        return Applied::ok(IngestOutcome::Matched);
    }
    let rev = tid(witnessed_us);
    let now = now_rfc3339();
    let (rev, now) = (rev.as_str(), now.as_str());
    let tombstoned = crate::db::retry_on_busy(|| async move {
        let mut tx = crate::db::begin_write(&state.db, backend).await?;
        crate::tombstones::write_did_tombstone(&mut tx, backend, did, rev, seq, now).await?;
        tx.commit().await
    })
    .await;
    let purged = match tombstoned {
        Ok(()) => match before {
            None => crate::tombstones::purge_did_records(&state.records, did, collections).await,
            Some(before) => {
                crate::tombstones::purge_did_records_before(
                    &state.records,
                    did,
                    collections,
                    before,
                )
                .await
            }
        },
        Err(e) => Err(e),
    };
    match purged {
        Ok(0) => Applied::ok(IngestOutcome::Matched),
        Ok(purged) => {
            let mut detail = serde_json::json!({ "purged": purged, "seq": seq });
            if let Some(status) = status {
                detail["status"] = status.into();
            }
            if before.is_some() {
                detail["replayed"] = true.into();
            }
            log(state, event_type, Severity::Info, did, detail).await;
            Applied::ok(IngestOutcome::Matched)
        }
        // The worker retries the event; `give_up_fn` logs it if it never
        // applies.
        Err(e) => {
            tracing::warn!(did, error = %e, "failed to purge an account's records");
            Applied::failed(&e)
        }
    }
}

/// Whether this instance holds any network records for `did`.
async fn holds_records(state: &AppState, did: &str) -> Result<bool, sqlx::Error> {
    state.records.holds_network_records(did, None).await
}

#[derive(serde::Deserialize)]
struct RepoStatus {
    active: bool,
}

/// The endpoint of `did`'s current PDS when `com.atproto.sync.getRepoStatus`
/// there says the account is active. `None` when it says otherwise, or when
/// it cannot be asked within `ACCOUNT_STATUS_TIMEOUT`.
async fn active_at_pds(state: &AppState, did: &str) -> Option<String> {
    let check = async {
        let pds =
            crate::profile::resolve_pds_endpoint_once(&state.http, &state.config.plc_url, did)
                .await
                .map_err(|e| e.message)?;
        let url = format!(
            "{}/xrpc/com.atproto.sync.getRepoStatus",
            pds.trim_end_matches('/')
        );
        let response = state
            .http
            .get(&url)
            .query(&[("did", did)])
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            return Err(format!("getRepoStatus answered {}", response.status()));
        }
        let body = happyview_provider::http::read_capped(response, REPO_STATUS_MAX_BYTES).await?;
        let status: RepoStatus = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
        Ok((pds, status.active))
    };
    match tokio::time::timeout(ACCOUNT_STATUS_TIMEOUT, check).await {
        Ok(Ok((pds, active))) => active.then_some(pds),
        Ok(Err(error)) => {
            tracing::debug!(
                did,
                error,
                "could not ask the account's PDS; trusting the relay"
            );
            None
        }
        Err(_) => {
            tracing::debug!(
                did,
                "the account's PDS did not answer in time; trusting the relay"
            );
            None
        }
    }
}

/// Decision A1: a deactivated, desynchronized or throttled account keeps its
/// records (takedowns and suspensions are purged above). Logged when this
/// instance holds any.
async fn account_inactive(state: &AppState, did: &str, status: Option<&str>) -> Applied {
    let held = match holds_records(state, did).await {
        Ok(held) => held,
        Err(e) => {
            tracing::warn!(did, error = %e, "failed to check an inactive account's records");
            return Applied::failed(&e);
        }
    };
    if held {
        log(
            state,
            "jetstream.account_inactive",
            Severity::Info,
            did,
            serde_json::json!({ "status": status }),
        )
        .await;
    }
    Applied::ok(IngestOutcome::Matched)
}

/// `active=true`. Ruling F6: lift the DID's tombstone, whatever event left
/// it, so a reinstated account can be backfilled again. Nearly every account
/// on the network has none, so look before taking the lock.
async fn account_active(state: &AppState, did: &str, before: Option<&str>) -> Applied {
    let backend = state.db_backend;
    let found = crate::tombstones::did_tombstone_rev(&state.db, backend, did).await;
    let lifted = match found {
        Ok(None) => return Applied::ok(IngestOutcome::Matched),
        // A replayed reactivation older than the tombstone lifts nothing.
        // TIDs are fixed-width and base32-sortable, so this is the order
        // `rev_compare` gives.
        Ok(Some(stored)) if before.is_some_and(|before| stored.as_str() >= before) => {
            return Applied::ok(IngestOutcome::Matched);
        }
        Ok(Some(_)) => {
            // The rev check repeats in the DELETE, so a newer tombstone
            // written since the look above survives too.
            crate::db::retry_on_busy(|| async move {
                let mut tx = crate::db::begin_write(&state.db, backend).await?;
                crate::tombstones::lift_did_tombstone(&mut tx, backend, did, before).await?;
                tx.commit().await
            })
            .await
        }
        Err(e) => Err(e),
    };
    match lifted {
        Ok(()) => {
            tracing::info!(did, "lifted the DID tombstone of a reactivated account");
            Applied::ok(IngestOutcome::Matched)
        }
        Err(e) => {
            tracing::warn!(did, error = %e, "failed to lift a reactivated account's tombstone");
            Applied::failed(&e)
        }
    }
}

/// `#sync`: the repo diverged, and its authoritative records follow as
/// creates on this same worker. Drop what is held for the DID in tracked
/// collections so records missing from the replacement set do not linger.
async fn repo_resynced(
    state: &AppState,
    did: &str,
    rev: Option<&str>,
    tracked: &[String],
    before: Option<&str>,
) -> Applied {
    let collections: Vec<String> = tracked
        .iter()
        .filter(|c| c.as_str() != LEXICON_SCHEMA_COLLECTION)
        .cloned()
        .collect();
    let collections = Some(collections.as_slice());
    let purged = match before {
        None => crate::tombstones::purge_did_records(&state.records, did, collections).await,
        Some(before) => {
            crate::tombstones::purge_did_records_before(&state.records, did, collections, before)
                .await
        }
    };
    match purged {
        Ok(0) => Applied::ok(IngestOutcome::Matched),
        Ok(purged) => {
            let mut detail = serde_json::json!({ "purged": purged, "rev": rev });
            if before.is_some() {
                detail["replayed"] = true.into();
            }
            log(state, "jetstream.sync_purged", Severity::Info, did, detail).await;
            Applied::ok(IngestOutcome::Matched)
        }
        // As for an account purge, `give_up_fn` logs it if it never applies.
        Err(e) => {
            tracing::warn!(did, error = %e, "failed to purge a resynced repo");
            Applied::failed(&e)
        }
    }
}
