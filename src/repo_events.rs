//! Identity, account and sync events handed to scripts: the `identity:*`,
//! `account:*` and `repo:*` triggers.
//!
//! These events arrive for every repo on the network, so a trigger fires only
//! for a repo this instance indexes (one it holds records for). Cheap checks
//! come first: whether any script is bound at all is read from the catalog
//! in memory, and whether a DID is indexed is cached, so an instance with no
//! such scripts does no extra work per event.
//!
//! A marker replayed from the archive (`as_of_us` set) never fires: it is
//! history, not news.
//!
//! There is no create trigger. The first event HappyView sees for a DID is
//! not its creation, so nothing could fire honestly on one.

use std::sync::Arc;

use dashmap::DashMap;
use serde_json::{Value, json};

use crate::AppState;
use crate::db::adapt_sql;
use crate::lua::scripts::{ParsedTrigger, TriggerKind};

/// How long a DID found indexed is remembered.
const HELD_TTL_SECONDS: i64 = 300;
/// How long a DID found not indexed is remembered: briefly, because an
/// account's first identity and account events often arrive just before its
/// first records.
const NOT_HELD_TTL_SECONDS: i64 = 15;
/// The most DIDs remembered at once; past it, the cache starts over.
const HELD_CAPACITY: usize = 100_000;

/// Whether each recently asked-about DID is one this instance indexes.
#[derive(Default)]
pub struct HeldCache {
    held: DashMap<String, (bool, i64)>,
}

impl HeldCache {
    fn get(&self, did: &str, now: i64) -> Option<bool> {
        self.held
            .get(did)
            .filter(|entry| entry.1 > now)
            .map(|entry| entry.0)
    }

    /// Forget what was found for `did`, so the next event looks again.
    pub fn forget(&self, did: &str) {
        self.held.remove(did);
    }

    fn put(&self, did: &str, held: bool, now: i64) {
        if self.held.len() >= HELD_CAPACITY && !self.held.contains_key(did) {
            self.held.clear();
        }
        let ttl = if held {
            HELD_TTL_SECONDS
        } else {
            NOT_HELD_TTL_SECONDS
        };
        self.held.insert(did.to_string(), (held, now + ttl));
    }
}

/// Whether this instance indexes `did`: it holds records for it, or did
/// until HappyView purged them for a suspension, takedown, deletion or
/// resync (a DID tombstone, or the state recorded here), so a reinstated or
/// resynced account still counts. A lookup that fails reads as not indexed,
/// so a database hiccup never fires a script for a stranger.
async fn indexed(state: &AppState, cache: &Arc<HeldCache>, did: &str) -> bool {
    let now = chrono::Utc::now().timestamp();
    if let Some(held) = cache.get(did, now) {
        return held;
    }
    let held = state
        .records
        .holds_network_records(did, None)
        .await
        .unwrap_or(false)
        || known_here(state, did).await;
    cache.put(did, held, now);
    held
}

/// Whether HappyView keeps a DID tombstone or recorded state for `did`.
async fn known_here(state: &AppState, did: &str) -> bool {
    let row: Result<Option<(String,)>, _> = crate::db::query_as(&adapt_sql(
        "SELECT did FROM happyview_did_tombstones WHERE did = ? \
         UNION ALL SELECT did FROM happyview_repo_event_state WHERE did = ? LIMIT 1",
        state.db_backend,
    ))
    .bind(did)
    .bind(did)
    .fetch_optional(&state.db)
    .await;
    matches!(row, Ok(Some(_)))
}

/// Forget an account's recorded state, once it is deleted.
async fn forget(state: &AppState, did: &str) {
    let result = crate::db::query(&adapt_sql(
        "DELETE FROM happyview_repo_event_state WHERE did = ?",
        state.db_backend,
    ))
    .bind(did)
    .execute(&state.db)
    .await;
    if let Err(e) = result {
        tracing::warn!(did, error = %e, "could not forget a deleted account's state");
    }
}

/// What HappyView last recorded of an account.
#[derive(Debug, Default, Clone, PartialEq)]
struct Known {
    handle: Option<String>,
    active: Option<bool>,
    status: Option<String>,
}

async fn known(state: &AppState, did: &str) -> Known {
    let row: Option<(Option<String>, Option<i64>, Option<String>)> =
        crate::db::query_as(&adapt_sql(
            "SELECT handle, active, status FROM happyview_repo_event_state WHERE did = ?",
            state.db_backend,
        ))
        .bind(did)
        .fetch_optional(&state.db)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(did, error = %e, "could not read an account's last state");
            None
        });
    row.map(|(handle, active, status)| Known {
        handle,
        active: active.map(|a| a != 0),
        status,
    })
    .unwrap_or_default()
}

async fn remember_handle(state: &AppState, did: &str, handle: Option<&str>) {
    let result = crate::db::query(&adapt_sql(
        "INSERT INTO happyview_repo_event_state (did, handle, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT (did) DO UPDATE SET handle = excluded.handle, updated_at = excluded.updated_at",
        state.db_backend,
    ))
    .bind(did)
    .bind(handle)
    .bind(crate::db::now_rfc3339())
    .execute(&state.db)
    .await;
    if let Err(e) = result {
        tracing::warn!(did, error = %e, "could not record an account's handle");
    }
}

async fn remember_status(state: &AppState, did: &str, active: bool, status: Option<&str>) {
    let result = crate::db::query(&adapt_sql(
        "INSERT INTO happyview_repo_event_state (did, active, status, updated_at) VALUES (?, ?, ?, ?) \
         ON CONFLICT (did) DO UPDATE SET active = excluded.active, status = excluded.status, \
         updated_at = excluded.updated_at",
        state.db_backend,
    ))
    .bind(did)
    .bind(i64::from(active))
    .bind(status)
    .bind(crate::db::now_rfc3339())
    .execute(&state.db)
    .await;
    if let Err(e) = result {
        tracing::warn!(did, error = %e, "could not record an account's status");
    }
}

/// The event's time as RFC 3339, or now when the source gave none.
fn time_of(time_us: Option<i64>) -> String {
    time_us
        .and_then(chrono::DateTime::from_timestamp_micros)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

fn trigger(kind: TriggerKind, event: &str) -> ParsedTrigger {
    ParsedTrigger::new(kind, event)
}

/// The triggers among `candidates` that have a script bound.
async fn bound(state: &AppState, candidates: Vec<ParsedTrigger>) -> Vec<ParsedTrigger> {
    let mut out = Vec::new();
    for t in candidates {
        if crate::lua::scripts::has_repo_event_script(state, &t).await {
            out.push(t);
        }
    }
    out
}

/// An `#identity` event: `identity:update`.
pub async fn identity(
    state: &AppState,
    did: &str,
    handle: Option<&str>,
    as_of_us: Option<i64>,
    time_us: Option<i64>,
) {
    if as_of_us.is_some() {
        return;
    }
    let triggers = bound(state, vec![trigger(TriggerKind::Identity, "update")]).await;
    if triggers.is_empty() || !indexed(state, &state.repo_event_held, did).await {
        return;
    }
    let previous = known(state, did).await;
    remember_handle(state, did, handle).await;
    let input = json!({
        "did": did,
        "handle": handle,
        "previous_handle": previous.handle,
        "time": time_of(time_us),
    });
    run(state, &triggers, did, &input).await;
}

/// Which `account:*` triggers an `#account` event fires, given what was
/// known before it: `account:update` always, and the one naming what changed.
/// A specific trigger fires on a change, or when nothing was known.
fn account_events(active: bool, status: Option<&str>, previous: &Known) -> Vec<&'static str> {
    let mut events = vec!["update"];
    let changed = previous.active != Some(active) || previous.status.as_deref() != status;
    match (active, status) {
        (true, _) if previous.active != Some(true) => events.push("activate"),
        (false, Some("deleted")) => events.push("delete"),
        (false, Some("suspended" | "takendown")) if changed => events.push("suspend"),
        (false, Some("deactivated")) if changed => events.push("deactivate"),
        _ => {}
    }
    events
}

/// An `#account` event: `account:update`, and `account:activate`,
/// `account:deactivate`, `account:suspend` or `account:delete` as it says.
/// Called before HappyView acts on the event, so `account:delete` runs before
/// the account's records are purged.
pub async fn account(
    state: &AppState,
    did: &str,
    active: bool,
    status: Option<&str>,
    as_of_us: Option<i64>,
    time_us: Option<i64>,
) {
    if as_of_us.is_some() {
        return;
    }
    let all = ["update", "activate", "deactivate", "suspend", "delete"]
        .map(|e| trigger(TriggerKind::Account, e))
        .to_vec();
    if bound(state, all).await.is_empty() || !indexed(state, &state.repo_event_held, did).await {
        return;
    }
    let previous = known(state, did).await;
    remember_status(state, did, active, status).await;
    let input = json!({
        "did": did,
        "active": active,
        "status": status,
        "previous_active": previous.active,
        "previous_status": previous.status,
        "time": time_of(time_us),
    });
    let fired = account_events(active, status, &previous)
        .into_iter()
        .map(|e| trigger(TriggerKind::Account, e))
        .collect();
    let triggers = bound(state, fired).await;
    run(state, &triggers, did, &input).await;
    // A deleted account's handle and status aren't kept once its scripts
    // have heard of the deletion.
    if !active && status == Some("deleted") {
        forget(state, did).await;
    }
}

/// A `#sync` event: the `repo:sync` scripts it will run, decided before
/// HappyView's resync purge, which removes the records that make the
/// account count as indexed. Empty for a replayed marker.
pub async fn sync_triggers(
    state: &AppState,
    did: &str,
    as_of_us: Option<i64>,
) -> Vec<ParsedTrigger> {
    if as_of_us.is_some() {
        return Vec::new();
    }
    let triggers = bound(state, vec![trigger(TriggerKind::Repo, "sync")]).await;
    if triggers.is_empty() || !indexed(state, &state.repo_event_held, did).await {
        return Vec::new();
    }
    triggers
}

/// Run `triggers` (from [`sync_triggers`]) for a `#sync` event, after the
/// resync purge.
pub async fn sync(
    state: &AppState,
    triggers: &[ParsedTrigger],
    did: &str,
    rev: Option<&str>,
    time_us: Option<i64>,
) {
    if triggers.is_empty() {
        return;
    }
    let input = json!({ "did": did, "rev": rev, "time": time_of(time_us) });
    run(state, triggers, did, &input).await;
}

async fn run(state: &AppState, triggers: &[ParsedTrigger], did: &str, input: &Value) {
    for t in triggers {
        crate::lua::scripts::run_repo_event_script(state, t, did, input).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn was(active: Option<bool>, status: Option<&str>) -> Known {
        Known {
            handle: None,
            active,
            status: status.map(str::to_string),
        }
    }

    #[test]
    fn each_account_event_fires_update_and_what_changed() {
        let unknown = Known::default();
        assert_eq!(account_events(true, None, &unknown), ["update", "activate"]);
        assert_eq!(
            account_events(true, None, &was(Some(true), None)),
            ["update"],
            "already active"
        );
        assert_eq!(
            account_events(true, None, &was(Some(false), Some("deactivated"))),
            ["update", "activate"]
        );
        assert_eq!(
            account_events(false, Some("deactivated"), &was(Some(true), None)),
            ["update", "deactivate"]
        );
        assert_eq!(
            account_events(
                false,
                Some("deactivated"),
                &was(Some(false), Some("deactivated"))
            ),
            ["update"],
            "already deactivated"
        );
        for status in ["suspended", "takendown"] {
            assert_eq!(
                account_events(false, Some(status), &unknown),
                ["update", "suspend"]
            );
        }
        assert_eq!(
            account_events(
                false,
                Some("takendown"),
                &was(Some(false), Some("suspended"))
            ),
            ["update", "suspend"],
            "a suspension becoming a takedown is a change"
        );
        // A deletion is always news, even repeated.
        assert_eq!(
            account_events(false, Some("deleted"), &was(Some(false), Some("deleted"))),
            ["update", "delete"]
        );
        // Another inactive status fires only the update.
        assert_eq!(
            account_events(false, Some("desynchronized"), &unknown),
            ["update"]
        );
    }

    #[test]
    fn the_held_cache_expires_and_is_bounded() {
        let cache = HeldCache::default();
        cache.put("did:plc:a", true, 0);
        assert_eq!(cache.get("did:plc:a", 10), Some(true));
        assert_eq!(cache.get("did:plc:a", HELD_TTL_SECONDS + 1), None);
        // Not indexed is remembered only briefly.
        cache.put("did:plc:b", false, 0);
        assert_eq!(
            cache.get("did:plc:b", NOT_HELD_TTL_SECONDS - 1),
            Some(false)
        );
        assert_eq!(cache.get("did:plc:b", NOT_HELD_TTL_SECONDS + 1), None);
        for i in 0..HELD_CAPACITY {
            cache.put(&format!("did:plc:{i}"), false, 0);
        }
        cache.put("did:plc:new", true, 0);
        assert!(cache.held.len() <= HELD_CAPACITY);
        assert_eq!(cache.get("did:plc:new", 1), Some(true));
    }

    #[test]
    fn an_event_time_is_rfc3339() {
        assert_eq!(
            time_of(Some(1_700_000_000_000_000)),
            "2023-11-14T22:13:20.000000Z"
        );
        assert!(time_of(None).ends_with('Z'));
    }
}
