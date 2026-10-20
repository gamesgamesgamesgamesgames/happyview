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
use happyview_provider::{
    Action, Host, IngestOutcome, LEXICON_SCHEMA_COLLECTION, Origin, ProviderEvent, RecordEvent,
    RecordKey, Severity, SourceEvent,
};
use tokio::sync::watch;

use super::event::{Commit, CursorPoint, Event, EventKind};
use super::workers::{ApplyFn, ApplyOutcome, GiveUpFn};

/// The worker's `ApplyFn`. `collections` is the registered record
/// collections; only a `#sync` reads them, so only a `#sync` clones them.
pub fn apply_fn(host: Arc<dyn Host>, collections: watch::Receiver<Vec<String>>) -> ApplyFn {
    Arc::new(move |event: Event| {
        let host = Arc::clone(&host);
        let tracked = match event.kind {
            EventKind::Sync { .. } => collections.borrow().clone(),
            _ => Vec::new(),
        };
        async move { apply_event(&*host, &tracked, event).await }.boxed()
    })
}

/// The worker's `GiveUpFn`: logs `jetstream.apply_failed` for an event that
/// kept failing, with the record's URI (or the event's kind, and an account
/// event's status), its seq and the last error. It is the one error event a
/// failed event writes; the attempts before it are only traced.
pub fn give_up_fn(host: Arc<dyn Host>) -> GiveUpFn {
    Arc::new(move |event: Event, error: String| {
        let host = Arc::clone(&host);
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
                &*host,
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
pub async fn apply_event(host: &dyn Host, tracked: &[String], event: Event) -> ApplyOutcome {
    let Event { did, cursor, kind } = event;
    match kind {
        EventKind::Commit(commit) => apply_commit(host, did, cursor, commit).await,
        kind => apply_marker(host, tracked, did, cursor, kind, None).await,
    }
}

/// Apply an account or sync marker replayed from the archive. It acts as of
/// its own time (plan ruling P2): it purges only rows older than itself and
/// never lifts or lowers a newer tombstone, so applying it after the commits
/// of the same range leaves what seq order would, and live writes made since
/// survive. `tracked` scopes a `#sync` purge, as for live.
pub async fn apply_replayed_marker(
    host: &dyn Host,
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
            apply_marker(host, tracked, did, cursor, kind, as_of_us).await
        }
    }
}

/// The ingest event for an account, sync or identity event seen at `cursor`.
/// `sync_scope` is the collections a `#sync` purges (the registered record
/// collections live, a job's collections in replay). An account purge is not
/// scoped: it removes everything held for the account, live or replayed.
pub(crate) fn marker_event(
    did: String,
    cursor: CursorPoint,
    kind: EventKind,
    as_of_us: Option<i64>,
    sync_scope: &[String],
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
            collections: None,
        },
        EventKind::Sync { rev } => SourceEvent::Sync {
            did,
            rev,
            as_of_us,
            seq,
            time_us,
            collections: Some(sync_scope.to_vec()),
        },
        EventKind::Identity { handle } => SourceEvent::Identity { did, handle },
        EventKind::Commit(_) => unreachable!("a commit is not a marker"),
    }
}

async fn apply_marker(
    host: &dyn Host,
    tracked: &[String],
    did: String,
    cursor: CursorPoint,
    kind: EventKind,
    as_of_us: Option<i64>,
) -> ApplyOutcome {
    let event = marker_event(did, cursor, kind, as_of_us, tracked);
    worker_outcome(host.ingest_with_error(event).await)
}

/// What the worker makes of an account, sync or identity event's outcome
/// and the error behind it: `Unavailable` is held until it applies, any
/// other failure is retried a few times.
fn worker_outcome((outcome, error): (IngestOutcome, Option<String>)) -> ApplyOutcome {
    let error = || error.clone().unwrap_or_default();
    match outcome {
        IngestOutcome::Unavailable => ApplyOutcome::Unavailable(error()),
        IngestOutcome::Errored | IngestOutcome::Invalid => ApplyOutcome::Failed(error()),
        IngestOutcome::Matched | IngestOutcome::Skipped | IngestOutcome::SchemaEvent => {
            ApplyOutcome::Applied
        }
    }
}

async fn apply_commit(
    host: &dyn Host,
    did: String,
    cursor: CursorPoint,
    commit: Commit,
) -> ApplyOutcome {
    let uri = || format!("at://{}/{}/{}", did, commit.collection, commit.rkey);
    let outcome = match Action::parse(&commit.operation) {
        Some(action) => {
            host.ingest(SourceEvent::Record(RecordEvent {
                key: RecordKey {
                    did: did.clone(),
                    collection: commit.collection.clone(),
                    rkey: commit.rkey.clone(),
                },
                action,
                cid: commit.cid.clone(),
                rev: commit.rev.clone(),
                body: commit.record.clone(),
                time_us: Some(cursor.time_us()),
                origin: Origin::Live,
                stored: false,
            }))
            .await
        }
        // An operation the pipeline does not know, judged as before.
        None => unknown_operation(host, &commit.collection),
    };
    match outcome {
        IngestOutcome::Matched => {
            host.counters().matched.fetch_add(1, Ordering::Relaxed);
            ApplyOutcome::Applied
        }
        IngestOutcome::Skipped => {
            host.counters().skipped.fetch_add(1, Ordering::Relaxed);
            ApplyOutcome::Applied
        }
        IngestOutcome::SchemaEvent => ApplyOutcome::Applied,
        // The record pipeline logged the error itself (`record.created` or
        // `record.deleted`).
        IngestOutcome::Errored => ApplyOutcome::Failed(format!(
            "{} of {} failed; see its record event log",
            commit.operation,
            uri()
        )),
        IngestOutcome::Unavailable => ApplyOutcome::Unavailable(format!(
            "the database is unavailable for the {} of {}",
            commit.operation,
            uri()
        )),
        // Retrying cannot help, so the event is skipped, with one warning.
        IngestOutcome::Invalid => {
            host.counters().skipped.fetch_add(1, Ordering::Relaxed);
            let uri = uri();
            let error = invalid_reason(&commit);
            tracing::warn!(uri, error, "skipping an invalid jetstream commit");
            log(
                host,
                "jetstream.record_invalid",
                Severity::Warn,
                &did,
                serde_json::json!({ "uri": uri, "seq": cursor.seq(), "error": error }),
            )
            .await;
            ApplyOutcome::Applied
        }
    }
}

/// A commit whose operation the pipeline does not know: as HappyView judged
/// one before the pipeline existed, `Skipped` for a collection it does not
/// track and `Invalid` for one it does. A lexicon schema commit is a schema
/// event the host has nothing to apply from.
fn unknown_operation(host: &dyn Host, collection: &str) -> IngestOutcome {
    if collection == LEXICON_SCHEMA_COLLECTION {
        return IngestOutcome::SchemaEvent;
    }
    if host
        .tracked_collections()
        .borrow()
        .iter()
        .any(|tracked| tracked == collection)
    {
        IngestOutcome::Invalid
    } else {
        tracing::debug!(collection, "skipping record for untracked collection");
        IngestOutcome::Skipped
    }
}

/// Why the record pipeline found `commit` invalid.
fn invalid_reason(commit: &Commit) -> String {
    match commit.operation.as_str() {
        "create" | "update" if commit.record.is_none() => {
            format!("the {} carries no record", commit.operation)
        }
        "create" | "update" | "delete" if commit.collection == LEXICON_SCHEMA_COLLECTION => {
            "the lexicon schema could not be applied".to_string()
        }
        "create" | "update" | "delete" => "the database refused the record's data".to_string(),
        action => format!("unknown operation {action:?}"),
    }
}

async fn log(
    host: &dyn Host,
    event_type: &str,
    severity: Severity,
    did: &str,
    detail: serde_json::Value,
) {
    host.log_event(ProviderEvent {
        event_type: event_type.to_string(),
        severity,
        actor_did: None,
        subject: Some(did.to_string()),
        detail,
    })
    .await;
}
