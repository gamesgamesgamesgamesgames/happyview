//! `happyview.reconcile-collection`: find the records this instance stores
//! for a collection that its accounts' PDSes no longer hold, and delete them.
//!
//! A delete that arrives while a collection isn't tracked never reaches the
//! index, so the record stays served after its owner deleted it (HAP-245).
//! This checks each account's stored rows against its PDS and deletes each
//! one the PDS no longer has. A delete hook still has its say: a
//! keep-on-delete hook keeps the record, and so does a hook that fails,
//! because this job infers the delete rather than hearing it from the
//! network.
//!
//! It deletes nothing it can't be sure of:
//!
//! - The stored rows are read before the PDS is asked, so a record created
//!   meanwhile is never a candidate, and each candidate is confirmed missing
//!   with `getRecord` before anything is done. Only a `RecordNotFound`
//!   answer confirms a miss; a bare 404 is what a proxy gives too.
//! - A delete removes the row only while it still has the CID and rev it
//!   had when it was read, so a record recreated at the same key meanwhile
//!   stays.
//! - Only a complete, all-2xx listing counts. A failed page, a missing or
//!   deactivated repo, or a PDS that can't be reached skips the account.
//! - A listing that is empty, or misses most of what is stored, skips the
//!   account as suspicious: a migrated account's old PDS, or a broken one,
//!   can answer 200 with nothing. An apply can act on it for named accounts.
//! - The PDS is resolved from the account's DID document, never a stored
//!   endpoint, so a migrated account is checked against where it lives now.
//!
//! A record whose stored CID differs from its PDS's is reported, never acted
//! on. A dry run (the default) reports and changes nothing.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time::Instant;

use crate::AppState;
use crate::event_log::{EventLog, Severity, log_event};
use crate::lua::{RecordEventPayload, RecordHookOutcome};

use super::super::{Job, db as jobs_db};
use super::NativeOutcome;

pub const JOB_TYPE: &str = "happyview.reconcile-collection";

/// PDS requests per second, by default and at most.
pub const DEFAULT_REQUESTS_PER_SECOND: u32 = 5;
pub const MAX_REQUESTS_PER_SECOND: u32 = 20;

/// Entries a report lists in full; the totals count them all.
const MISSING_LISTED: usize = 1000;
const STALE_LISTED: usize = 100;

/// Tries of one rate-limited request, and the longest wait between two.
const RATE_LIMIT_TRIES: u32 = 5;
const MAX_RATE_LIMIT_WAIT: Duration = Duration::from_secs(120);

/// Accounts read from the index at a time.
const DIDS_PAGE: i64 = 500;

/// A listing missing at least this many stored rows, and more than half of
/// them, is suspicious.
const SUSPICIOUS_MISSES: usize = 10;

/// What to reconcile.
#[derive(Clone, Debug, Deserialize)]
pub struct Options {
    pub collection: String,
    /// Report only. On unless asked otherwise.
    #[serde(default = "dry_run_default")]
    pub dry_run: bool,
    /// Only these accounts; every account holding rows of the collection
    /// when absent.
    #[serde(default)]
    pub dids: Option<Vec<String>>,
    #[serde(default)]
    pub pds_requests_per_second: Option<u32>,
    /// Accounts whose suspicious listing is acted on anyway.
    #[serde(default)]
    pub allow_suspicious: Vec<String>,
}

fn dry_run_default() -> bool {
    true
}

/// What a run found, and in an apply, did.
#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub collection: String,
    pub dry_run: bool,
    /// Accounts whose rows were compared with their PDS.
    pub dids_checked: usize,
    pub dids_skipped: Vec<Skipped>,
    /// Stored records confirmed missing from their PDS (the first
    /// [`MISSING_LISTED`]).
    pub missing: Vec<Missing>,
    pub missing_total: usize,
    /// Candidates `getRecord` didn't confirm missing, and left alone.
    pub unconfirmed_total: usize,
    /// In an apply: missing records deleted, those a delete hook kept, those
    /// kept because the hook failed, those changed or removed since they
    /// were read, and those whose delete failed.
    pub deleted: usize,
    pub kept_by_hook: usize,
    pub hook_failed: usize,
    pub changed_since_check: usize,
    pub failed: usize,
    /// Records whose stored CID isn't the PDS's (the first
    /// [`STALE_LISTED`]). Reported only.
    pub stale_cid: Vec<StaleCid>,
    pub stale_cid_total: usize,
    /// Set when the job was paused or cancelled before it finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Skipped {
    pub did: String,
    /// `pds_unresolved`, `listing_failed`, `repo_unavailable`,
    /// `rate_limited`, `suspicious_listing` or `store_read_failed`.
    pub reason: &'static str,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct Missing {
    pub uri: String,
    pub cid: Option<String>,
    /// `missing` in a dry run; in an apply `deleted`, `kept_by_hook`,
    /// `hook_failed`, `changed_since_check` or `failed`.
    pub outcome: &'static str,
}

#[derive(Debug, Serialize)]
pub struct StaleCid {
    pub uri: String,
    pub stored_cid: Option<String>,
    pub pds_cid: String,
}

pub async fn run(state: &AppState, job: &Job) -> NativeOutcome {
    let options: Options = match serde_json::from_value(job.input.clone()) {
        Ok(options) => options,
        Err(e) => return NativeOutcome::Failed(format!("invalid input: {e}")),
    };
    match reconcile(state, &options, Some(&job.id)).await {
        Ok(report) => NativeOutcome::Completed(serde_json::to_value(report).unwrap_or_default()),
        Err(e) => NativeOutcome::Failed(e),
    }
}

/// Why `collection` can't be reconciled here, or `None` when it can.
pub fn refusal(state: &AppState, collection: &str) -> Option<String> {
    if collection.trim().is_empty() {
        return Some("collection is required".into());
    }
    if collection == crate::record_handler::LEXICON_SCHEMA_COLLECTION {
        return Some(format!(
            "'{collection}' records are lexicon schemas this instance tracks, not indexed records, so they cannot be reconciled"
        ));
    }
    if state.records.external_route(collection).is_some() {
        return Some(format!(
            "'{collection}' is served from a shared record store, so this instance cannot reconcile its records: a stale record there belongs to every instance reading that store. Reconcile it on the store."
        ));
    }
    (!state.lexicons.is_indexed(collection)).then(|| {
        format!(
            "'{collection}' is not an indexed record collection here, so nothing would apply its deletes. Index it first."
        )
    })
}

/// Reconcile `options.collection`. With `job_id`, reports progress to that
/// job and stops when it is paused or cancelled.
pub async fn reconcile(
    state: &AppState,
    options: &Options,
    job_id: Option<&str>,
) -> Result<Report, String> {
    if let Some(reason) = refusal(state, &options.collection) {
        return Err(reason);
    }
    let per_second = options
        .pds_requests_per_second
        .unwrap_or(DEFAULT_REQUESTS_PER_SECOND)
        .clamp(1, MAX_REQUESTS_PER_SECOND);
    let mut pacer = Pacer::new(per_second);
    let mut report = Report {
        collection: options.collection.clone(),
        dry_run: options.dry_run,
        ..Report::default()
    };
    let dids = match &options.dids {
        Some(dids) => {
            let mut dids = dids.clone();
            dids.sort();
            dids.dedup();
            dids
        }
        None => held_dids(state, &options.collection).await?,
    };
    let total = dids.len();
    let mut done = 0;
    for did in &dids {
        if stop_asked(state, job_id, &mut report).await {
            break;
        }
        check(state, &mut pacer, options, job_id, did, &mut report).await;
        if report.stopped.is_some() {
            break;
        }
        done += 1;
        if let Some(id) = job_id {
            let _ = jobs_db::update_progress(
                state,
                id,
                &serde_json::json!({
                    "dids_done": done,
                    "dids_total": total,
                    "missing": report.missing_total,
                    "deleted": report.deleted,
                }),
            )
            .await;
        }
    }
    // A stopped job's result is not kept, so what it already did goes in
    // its progress.
    if let (Some(id), Some(_)) = (job_id, &report.stopped) {
        let _ = jobs_db::update_progress(
            state,
            id,
            &serde_json::json!({
                "dids_done": done,
                "dids_total": total,
                "missing": report.missing_total,
                "deleted": report.deleted,
                "report": &report,
            }),
        )
        .await;
    }
    Ok(report)
}

/// Whether the job has been paused or cancelled; notes it in `report`.
async fn stop_asked(state: &AppState, job_id: Option<&str>, report: &mut Report) -> bool {
    let Some(id) = job_id else {
        return false;
    };
    match jobs_db::should_stop(state, id).await {
        Some(reason) => {
            report.stopped = Some(reason.to_string());
            true
        }
        None => false,
    }
}

/// Every account holding network rows of `collection`.
async fn held_dids(state: &AppState, collection: &str) -> Result<Vec<String>, String> {
    let collections = [collection.to_string()];
    let mut dids = Vec::new();
    loop {
        let page = state
            .records
            .held_dids_page(
                &state.db,
                &collections,
                dids.last().map(String::as_str),
                DIDS_PAGE,
            )
            .await
            .map_err(|e| format!("could not list the collection's accounts: {e}"))?;
        let short = (page.len() as i64) < DIDS_PAGE;
        dids.extend(page);
        if short {
            return Ok(dids);
        }
    }
}

/// One account: compare, confirm, and in an apply, delete.
async fn check(
    state: &AppState,
    pacer: &mut Pacer,
    options: &Options,
    job_id: Option<&str>,
    did: &str,
    report: &mut Report,
) {
    let collection = &options.collection;
    let skip = |report: &mut Report, reason: &'static str, detail: String| {
        report.dids_skipped.push(Skipped {
            did: did.to_string(),
            reason,
            detail,
        });
    };
    // Read before the PDS is asked: a record created after this can't be a
    // candidate.
    let stored = match state.records.network_keys_of(did, collection).await {
        Ok(stored) => stored,
        Err(e) => return skip(report, "store_read_failed", e.to_string()),
    };
    if stored.is_empty() {
        return;
    }
    let pds =
        match crate::profile::resolve_pds_endpoint_once(&state.http, &state.config.plc_url, did)
            .await
        {
            Ok(pds) => pds,
            Err(e) => return skip(report, "pds_unresolved", e.message),
        };
    let listing = match list_records(state, pacer, &pds, did, collection).await {
        Ok(listing) => listing,
        Err(Unusable { reason, detail }) => return skip(report, reason, detail),
    };

    let mut misses = Vec::new();
    for (rkey, cid, rev) in &stored {
        match listing.get(rkey) {
            Some(pds_cid) => {
                if cid.as_deref() != Some(pds_cid.as_str()) {
                    report.stale_cid_total += 1;
                    if report.stale_cid.len() < STALE_LISTED {
                        report.stale_cid.push(StaleCid {
                            uri: uri(did, collection, rkey),
                            stored_cid: cid.clone(),
                            pds_cid: pds_cid.clone(),
                        });
                    }
                }
            }
            None => misses.push((rkey, cid, rev)),
        }
    }
    if misses.is_empty() {
        report.dids_checked += 1;
        return;
    }
    let suspicious = listing.is_empty()
        || (misses.len() >= SUSPICIOUS_MISSES && misses.len() * 2 > stored.len());
    if suspicious && !options.allow_suspicious.iter().any(|d| d == did) {
        return skip(
            report,
            "suspicious_listing",
            format!(
                "the PDS lists {} of the {} records stored for this account; name it in allow_suspicious to act on it",
                stored.len() - misses.len(),
                stored.len()
            ),
        );
    }
    report.dids_checked += 1;

    for (rkey, cid, rev) in misses {
        if stop_asked(state, job_id, report).await {
            return;
        }
        match confirm_missing(state, pacer, &pds, did, collection, rkey).await {
            Confirmed::Missing => {}
            Confirmed::Present | Confirmed::Unknown => {
                report.unconfirmed_total += 1;
                continue;
            }
        }
        let at = uri(did, collection, rkey);
        report.missing_total += 1;
        let outcome = if options.dry_run {
            "missing"
        } else {
            delete(state, job_id, &at, did, collection, rkey, cid, rev).await
        };
        match outcome {
            "deleted" => report.deleted += 1,
            "kept_by_hook" => report.kept_by_hook += 1,
            "hook_failed" => report.hook_failed += 1,
            "changed_since_check" => report.changed_since_check += 1,
            "failed" => report.failed += 1,
            _ => {}
        }
        if report.missing.len() < MISSING_LISTED {
            report.missing.push(Missing {
                uri: at,
                cid: cid.clone(),
                outcome,
            });
        }
    }
}

/// Delete one confirmed-missing record, after its delete hook, while it
/// still has the `cid` and `rev` it was read with. Says what happened, as
/// [`Missing::outcome`] names it. Every delete is logged, whatever the
/// event-logging setting, so what an apply removed can be found later.
#[allow(clippy::too_many_arguments)]
async fn delete(
    state: &AppState,
    job_id: Option<&str>,
    at: &str,
    did: &str,
    collection: &str,
    rkey: &str,
    cid: &Option<String>,
    rev: &Option<String>,
) -> &'static str {
    let body = match state.records.get(at).await {
        Ok(Some(row)) => serde_json::from_str::<serde_json::Value>(&row.record).ok(),
        Ok(None) => return "changed_since_check",
        Err(e) => {
            tracing::warn!(uri = %at, error = %e, "reconcile could not read a record to delete");
            return "failed";
        }
    };
    let hook = crate::lua::scripts::run_record_event_script_strict(
        state,
        RecordEventPayload {
            nsid: collection,
            action: "delete",
            uri: at,
            did,
            rkey,
            record: body.as_ref(),
        },
    )
    .await;
    let job = job_id.unwrap_or(JOB_TYPE);
    match hook {
        Ok(RecordHookOutcome::Skip) => return "kept_by_hook",
        Ok(_) => {}
        Err(error) => {
            reconcile_log(
                state,
                "record.delete_hook_failed",
                Severity::Warn,
                at,
                job,
                cid,
                Some(&error),
            )
            .await;
            return "hook_failed";
        }
    }
    let now = crate::db::now_rfc3339();
    let deleted = crate::db::with_change_reason(
        Some("backfill".to_string()),
        crate::db::retry_on_busy(|| {
            state.records.delete_network_if_unchanged(
                &state.db,
                at,
                did,
                collection,
                cid.as_deref(),
                rev.as_deref(),
                &now,
            )
        }),
    )
    .await;
    match deleted {
        Ok(true) => {
            reconcile_log(state, "record.deleted", Severity::Info, at, job, cid, None).await;
            "deleted"
        }
        Ok(false) => "changed_since_check",
        Err(e) => {
            let error = e.to_string();
            tracing::warn!(uri = %at, %error, "reconcile could not delete a record");
            reconcile_log(
                state,
                "record.deleted",
                Severity::Error,
                at,
                job,
                cid,
                Some(&error),
            )
            .await;
            "failed"
        }
    }
}

async fn reconcile_log(
    state: &AppState,
    event_type: &str,
    severity: Severity,
    at: &str,
    job_id: &str,
    cid: &Option<String>,
    error: Option<&str>,
) {
    log_event(
        &state.db,
        EventLog {
            event_type: event_type.to_string(),
            severity,
            actor_did: None,
            subject: Some(at.to_string()),
            detail: serde_json::json!({
                "origin": JOB_TYPE,
                "job_id": job_id,
                "cid": cid,
                "error": error,
            }),
        },
        state.db_backend,
    )
    .await;
}

fn uri(did: &str, collection: &str, rkey: &str) -> String {
    format!("at://{did}/{collection}/{rkey}")
}

/// Why an account's listing can't be used.
struct Unusable {
    reason: &'static str,
    detail: String,
}

#[derive(Deserialize)]
struct ListRecordsPage {
    records: Vec<ListedRecord>,
    cursor: Option<String>,
}

#[derive(Deserialize)]
struct ListedRecord {
    uri: String,
    cid: String,
}

/// Every record `did`'s PDS lists in `collection`: rkey to CID. Complete or
/// an error, never partial.
async fn list_records(
    state: &AppState,
    pacer: &mut Pacer,
    pds: &str,
    did: &str,
    collection: &str,
) -> Result<HashMap<String, String>, Unusable> {
    let base = pds.trim_end_matches('/');
    let mut listing = HashMap::new();
    let mut cursor: Option<String> = None;
    let mut seen_cursors = HashSet::new();
    loop {
        let mut url = format!(
            "{base}/xrpc/com.atproto.repo.listRecords?repo={}&collection={}&limit=100",
            urlencoding::encode(did),
            urlencoding::encode(collection)
        );
        if let Some(c) = &cursor {
            url.push_str("&cursor=");
            url.push_str(&urlencoding::encode(c));
        }
        let (status, body) = get(state, pacer, &url).await?;
        if !status.is_success() {
            return Err(listing_error(status, &body));
        }
        let page: ListRecordsPage = serde_json::from_str(&body).map_err(|e| Unusable {
            reason: "listing_failed",
            detail: format!("invalid listRecords response: {e}"),
        })?;
        let empty = page.records.is_empty();
        for record in page.records {
            if let Some(rkey) = record.uri.rsplit('/').next() {
                listing.insert(rkey.to_string(), record.cid);
            }
        }
        match page.cursor {
            // A PDS that pages on must have listed something; an empty page
            // that claims more can't be trusted to have shown everything.
            Some(_) if empty => {
                return Err(Unusable {
                    reason: "listing_failed",
                    detail: "listRecords answered an empty page with a cursor".into(),
                });
            }
            Some(next) => {
                if !seen_cursors.insert(next.clone()) {
                    return Err(Unusable {
                        reason: "listing_failed",
                        detail: "listRecords repeated a cursor".into(),
                    });
                }
                cursor = Some(next);
            }
            None => return Ok(listing),
        }
    }
}

/// A failed listing: a missing, deactivated or taken-down repo is the
/// account's state, which this job leaves to identity events.
fn listing_error(status: reqwest::StatusCode, body: &str) -> Unusable {
    let parsed: Option<serde_json::Value> = serde_json::from_str(body).ok();
    let error = parsed
        .as_ref()
        .and_then(|b| b.get("error"))
        .and_then(|e| e.as_str())
        .unwrap_or_default();
    let message = parsed
        .as_ref()
        .and_then(|b| b.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or_default();
    let unavailable = matches!(
        error,
        "RepoNotFound" | "RepoDeactivated" | "RepoTakendown" | "RepoSuspended"
    ) || message.contains("Could not find repo");
    Unusable {
        reason: if unavailable {
            "repo_unavailable"
        } else {
            "listing_failed"
        },
        detail: format!("listRecords answered {status}: {}", excerpt(body)),
    }
}

enum Confirmed {
    Missing,
    Present,
    Unknown,
}

/// Whether `getRecord` says the record is gone: a 400 or 404 whose JSON
/// names `RecordNotFound`. A bare 404 is what a proxy or a PDS that doesn't
/// route the method answers too, so it, like anything else, leaves the
/// record alone.
async fn confirm_missing(
    state: &AppState,
    pacer: &mut Pacer,
    pds: &str,
    did: &str,
    collection: &str,
    rkey: &str,
) -> Confirmed {
    let url = format!(
        "{}/xrpc/com.atproto.repo.getRecord?repo={}&collection={}&rkey={}",
        pds.trim_end_matches('/'),
        urlencoding::encode(did),
        urlencoding::encode(collection),
        urlencoding::encode(rkey)
    );
    match get(state, pacer, &url).await {
        Ok((status, body)) => getrecord_answer(status, &body),
        Err(_) => Confirmed::Unknown,
    }
}

fn getrecord_answer(status: reqwest::StatusCode, body: &str) -> Confirmed {
    if status.is_success() {
        return Confirmed::Present;
    }
    if status != reqwest::StatusCode::BAD_REQUEST && status != reqwest::StatusCode::NOT_FOUND {
        return Confirmed::Unknown;
    }
    let error = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|b| b.get("error").and_then(|e| e.as_str()).map(str::to_string));
    if error.as_deref() == Some("RecordNotFound") {
        Confirmed::Missing
    } else {
        Confirmed::Unknown
    }
}

/// One paced GET, retried on a 429 after the wait the PDS asks for.
async fn get(
    state: &AppState,
    pacer: &mut Pacer,
    url: &str,
) -> Result<(reqwest::StatusCode, String), Unusable> {
    let mut tries = 0;
    loop {
        pacer.wait().await;
        let response = state.http.get(url).send().await.map_err(|e| Unusable {
            reason: "listing_failed",
            detail: e.to_string(),
        })?;
        let status = response.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            tries += 1;
            if tries >= RATE_LIMIT_TRIES {
                return Err(Unusable {
                    reason: "rate_limited",
                    detail: format!("still rate limited after {tries} tries"),
                });
            }
            let wait = rate_limit_wait(response.headers(), tries);
            tokio::time::sleep(wait).await;
            continue;
        }
        let body = response.text().await.map_err(|e| Unusable {
            reason: "listing_failed",
            detail: e.to_string(),
        })?;
        return Ok((status, body));
    }
}

/// How long a 429 asks to wait: until `RateLimit-Reset` (a Unix time), else
/// `Retry-After` seconds, else a doubling backoff; at most
/// [`MAX_RATE_LIMIT_WAIT`].
fn rate_limit_wait(headers: &reqwest::header::HeaderMap, tries: u32) -> Duration {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let wait = header("ratelimit-reset")
        .map(|reset| Duration::from_secs(reset.saturating_sub(now)))
        .or_else(|| header("retry-after").map(Duration::from_secs))
        .unwrap_or_else(|| Duration::from_secs(1 << tries.min(6)));
    wait.min(MAX_RATE_LIMIT_WAIT)
}

fn excerpt(body: &str) -> String {
    body.chars().take(200).collect()
}

/// Spaces requests at most `per_second` a second.
struct Pacer {
    gap: Duration,
    next: Instant,
}

impl Pacer {
    fn new(per_second: u32) -> Self {
        Self {
            gap: Duration::from_secs(1) / per_second.max(1),
            next: Instant::now(),
        }
    }

    async fn wait(&mut self) {
        tokio::time::sleep_until(self.next).await;
        self.next = Instant::now().max(self.next) + self.gap;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rate_limit_waits_as_asked_and_no_longer_than_the_cap() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after", "3".parse().unwrap());
        assert_eq!(rate_limit_wait(&headers, 1), Duration::from_secs(3));
        headers.insert("retry-after", "100000".parse().unwrap());
        assert_eq!(rate_limit_wait(&headers, 1), MAX_RATE_LIMIT_WAIT);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        headers.insert("ratelimit-reset", (now + 5).to_string().parse().unwrap());
        let wait = rate_limit_wait(&headers, 1);
        assert!(
            wait <= Duration::from_secs(5) && wait >= Duration::from_secs(4),
            "{wait:?}"
        );
        assert_eq!(
            rate_limit_wait(&reqwest::header::HeaderMap::new(), 2),
            Duration::from_secs(4)
        );
    }

    #[test]
    fn a_missing_or_deactivated_repo_is_unavailable_and_anything_else_failed() {
        let bad = reqwest::StatusCode::BAD_REQUEST;
        for body in [
            r#"{"error":"InvalidRequest","message":"Could not find repo: did:plc:x"}"#,
            r#"{"error":"RepoDeactivated","message":"deactivated"}"#,
            r#"{"error":"RepoTakendown"}"#,
        ] {
            assert_eq!(
                listing_error(bad, body).reason,
                "repo_unavailable",
                "{body}"
            );
        }
        assert_eq!(
            listing_error(reqwest::StatusCode::BAD_GATEWAY, "upstream").reason,
            "listing_failed"
        );
    }

    #[test]
    fn only_record_not_found_confirms_a_miss() {
        use reqwest::StatusCode;
        let not_found = r#"{"error":"RecordNotFound","message":"Could not locate record"}"#;
        for status in [StatusCode::BAD_REQUEST, StatusCode::NOT_FOUND] {
            assert!(matches!(
                getrecord_answer(status, not_found),
                Confirmed::Missing
            ));
        }
        for (status, body) in [
            (StatusCode::NOT_FOUND, ""),
            (StatusCode::NOT_FOUND, "<html>404 Not Found</html>"),
            (StatusCode::NOT_FOUND, r#"{"error":"MethodNotImplemented"}"#),
            (StatusCode::BAD_REQUEST, r#"{"error":"InvalidRequest"}"#),
            (StatusCode::BAD_GATEWAY, not_found),
        ] {
            assert!(
                matches!(getrecord_answer(status, body), Confirmed::Unknown),
                "{status} {body}"
            );
        }
        assert!(matches!(
            getrecord_answer(StatusCode::OK, "{}"),
            Confirmed::Present
        ));
    }

    #[test]
    fn input_defaults_to_a_dry_run() {
        let options: Options =
            serde_json::from_value(serde_json::json!({ "collection": "app.test.post" })).unwrap();
        assert!(options.dry_run);
        assert!(options.allow_suspicious.is_empty());
    }
}
