//! Values that cross the HappyView ↔ provider boundary.

use serde::{Deserialize, Serialize};

/// The contract version. A provider built for another version is refused at boot.
pub const CONTRACT_VERSION: u32 = 1;

/// The collection whose commits carry network lexicon schemas. Every source
/// must deliver its commits whatever the tracked list says.
pub const LEXICON_SCHEMA_COLLECTION: &str = "com.atproto.lexicon.schema";

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RecordKey {
    pub did: String,
    pub collection: String,
    pub rkey: String,
}

impl RecordKey {
    pub fn uri(&self) -> String {
        format!("at://{}/{}/{}", self.did, self.collection, self.rkey)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Create,
    Update,
    Delete,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Create => "create",
            Action::Update => "update",
            Action::Delete => "delete",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "create" => Some(Action::Create),
            "update" => Some(Action::Update),
            "delete" => Some(Action::Delete),
            _ => None,
        }
    }
}

/// Where an event came from. The pipeline acts on it: a `Backfill` event's
/// write is tagged with the change reason `backfill` (and a batch of them is
/// written through the backfill pool, `backfill_db`), a CID mismatch in one
/// is also traced as a warning, and a `Retry`
/// is a dead-letter retry, whose skip removes the version its fail-open
/// delivery stored and whose claimed delta applies only to the version the
/// dead letter was made from. Logging and telemetry read it too.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    Live,
    Backfill { job_id: String },
    Retry,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecordEvent {
    pub key: RecordKey,
    pub action: Action,
    pub cid: Option<String>,
    /// The repo commit's rev, when the source has it. Orders deliveries.
    pub rev: Option<String>,
    /// The record body. For a delete, the last known body when the source
    /// has it, so a delete hook can see what it is deleting.
    pub body: Option<serde_json::Value>,
    pub time_us: Option<i64>,
    pub origin: Origin,
    /// The body already lives in the external storage that claims this
    /// collection. The host runs hooks and writes only the hook's delta.
    pub stored: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SourceEvent {
    Record(RecordEvent),
    /// `as_of_us` set: act as of that time and never remove newer records
    /// (a replayed marker). `None`: act now.
    Account {
        did: String,
        active: bool,
        status: Option<String>,
        as_of_us: Option<i64>,
        /// The source's sequence number for the event, when it has one.
        /// Logged and kept on a DID tombstone.
        seq: Option<i64>,
        /// When the source saw the event. Orders a DID tombstone against
        /// commit revs; `None` means now.
        time_us: Option<i64>,
        /// The collections a purge removes records from. `None`: every
        /// record the instance holds for the account (a live purge).
        /// `Some(list)`: only those, as a backfill job scoped to them does.
        collections: Option<Vec<String>>,
    },
    Identity {
        did: String,
        handle: Option<String>,
    },
    Sync {
        did: String,
        rev: Option<String>,
        as_of_us: Option<i64>,
        /// As for `Account`.
        seq: Option<i64>,
        /// As for `Account`.
        time_us: Option<i64>,
        /// The collections the resync purges. `None`: every tracked record
        /// collection (a live `#sync`). `Some(list)`: only those, as a
        /// backfill job scoped to them does.
        collections: Option<Vec<String>>,
    },
}

/// What the host did with one event. Same variants as HappyView's
/// `RecordOutcome`, which is now an alias for this type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum IngestOutcome {
    Matched,
    Skipped,
    SchemaEvent,
    Errored,
    /// The database could not be reached; the same event should apply later.
    Unavailable,
    /// The event can never apply as it stands; retrying cannot help.
    Invalid,
}

/// What the host did with a batch: one outcome per event, in order, and
/// whether each event changed stored state (`false` for an identical
/// repeat, a refused write or anything not stored).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BatchOutcome {
    pub outcomes: Vec<IngestOutcome>,
    pub changed: Vec<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackfillKind {
    Manual,
    Automatic,
    /// Jetstream reported `CursorTooOld`; replay from `after_seq`.
    Gap {
        after_seq: i64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackfillRequest {
    pub collection: Option<String>,
    pub dids: Vec<String>,
    /// `auto`, `replay`, `crawl` or a source-specific value; `None` = the default.
    pub source: Option<String>,
    pub actor: String,
    pub kind: BackfillKind,
}

#[derive(Debug, thiserror::Error)]
pub enum BackfillError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Internal(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
    Error,
}

/// One row for the admin event log.
#[derive(Clone, Debug)]
pub struct ProviderEvent {
    pub event_type: String,
    pub severity: Severity,
    pub actor_did: Option<String>,
    pub subject: Option<String>,
    pub detail: serde_json::Value,
}

#[derive(Clone, Debug)]
pub struct InstanceInfo {
    pub plc_url: String,
    pub relay_url: String,
    pub public_url: String,
    /// The `User-Agent` HappyView sends, for a provider's own HTTP clients.
    pub user_agent: String,
}

/// Why a lookup failed, classified as a backfill classifies a fetch, so a
/// source can tell a failure worth retrying (after `retry_after`, when the
/// host was told one) from a final one.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{}", .0.message)]
pub struct ResolveError(pub crate::failure::BackfillFailure);

impl ResolveError {
    /// A failure with no more to it than `message`: never worth retrying.
    pub fn new(message: impl Into<String>) -> Self {
        Self(crate::failure::BackfillFailure {
            kind: crate::failure::BackfillErrorKind::Other,
            message: message.into(),
            retry_after: None,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("contract version {found} is not {expected}")]
    Version { found: u32, expected: u32 },
    #[error("{0}")]
    Other(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_key_renders_an_at_uri() {
        let key = RecordKey {
            did: "did:plc:abc".into(),
            collection: "app.example.thing".into(),
            rkey: "3k".into(),
        };
        assert_eq!(key.uri(), "at://did:plc:abc/app.example.thing/3k");
    }

    #[test]
    fn action_parse_round_trips_as_str() {
        for action in [Action::Create, Action::Update, Action::Delete] {
            assert_eq!(Action::parse(action.as_str()), Some(action));
        }
        assert_eq!(Action::parse("replace"), None);
    }

    #[test]
    fn severity_serializes_like_the_event_log() {
        assert_eq!(serde_json::to_string(&Severity::Warn).unwrap(), "\"warn\"");
    }
}

/// What a backfill job's records cost in hooks so far, as HappyView measured
/// them ([`crate::Host::job_hook_stats`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct JobHookStats {
    /// Records the job delivered that reached a hook decision, whether or not
    /// a hook was set for them.
    pub done: u64,
    /// Records whose hook failed at least once, retries included.
    pub failed: u64,
    /// Records whose hook ran out of time at least once.
    pub timeouts: u64,
    /// Records whose hook was given up on and dead-lettered.
    pub dead_lettered: u64,
    /// Records not stored: the hook skipped them or their CID did not match.
    pub skipped: u64,
    /// `done` by what was decided: `proceed`, `replace`, `skip`,
    /// `cid_mismatch`.
    pub outcomes: std::collections::BTreeMap<String, u64>,
    /// Records per second over about the last 30 seconds; the average until
    /// a second has passed.
    pub rate_per_s: f64,
    /// Records per second since the job's first record.
    pub avg_rate_per_s: f64,
    /// Seconds left, from the job's reported progress against its total.
    /// `None` without a total, or before progress has moved.
    pub eta_s: Option<f64>,
    pub record_ms_p50: Option<f64>,
    /// The hook's own time: its guest code plus the host calls it made.
    pub script_ms_p50: Option<f64>,
    /// Script time over record time, from 0 to 1.
    pub script_share: Option<f64>,
    /// Per stage, the records it covered and their mean. A stage whose name
    /// ends in `_skipped` is a count, and its mean is `None`.
    pub stages: std::collections::BTreeMap<String, StageStats>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StageStats {
    pub n: u64,
    pub ms_mean: Option<f64>,
}
