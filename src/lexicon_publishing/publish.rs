use std::future::Future;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::AppState;
use crate::catalog::{Layer, LexiconListing};
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};
use crate::linked_repos::types::LinkedRepo;

use super::store::{self, Rule};
use super::target::{self, KIND_INSTANCE, TargetInfo};
use super::{PublishStatus, SCHEMA_COLLECTION, best_match, record_cid, schema_record};

const SIGN_IN_AGAIN: &str = "the attached account needs to sign in again";
const WITHDRAWN: &str = "unpublished by hand, so auto-publish leaves it alone";

#[derive(Debug, Clone, Serialize)]
pub struct PublishOutcome {
    pub nsid: String,
    pub ok: bool,
    pub uri: Option<String>,
    pub cid: Option<String>,
    pub error: Option<String>,
}

impl PublishOutcome {
    fn failed(nsid: &str, error: String) -> Self {
        PublishOutcome {
            nsid: nsid.to_string(),
            ok: false,
            uri: None,
            cid: None,
            error: Some(error),
        }
    }
}

/// Held across every publish and unpublish, from reading the rule to storing
/// the result, so a manual publish and a reconcile run never write one record
/// at the same time or store their results out of order.
static WRITE: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

static RECONCILE: LazyLock<SingleFlight> = LazyLock::new(SingleFlight::default);

/// The operator-facing text of an error, without the variant's prefix.
fn message(e: AppError) -> String {
    match e {
        AppError::Auth(m)
        | AppError::BadGateway(m)
        | AppError::BadRequest(m)
        | AppError::Conflict(m)
        | AppError::Forbidden(m)
        | AppError::Internal(m)
        | AppError::NotFound(m) => m,
        other => other.to_string(),
    }
}

/// Only lexicons HappyView owns: a network lexicon was fetched from someone
/// else's repo and is theirs to publish.
pub async fn publishable(state: &AppState) -> Result<Vec<LexiconListing>, AppError> {
    let all = state.lexicons.list(&state.db, state.db_backend).await?;
    Ok(all
        .into_iter()
        .filter(|l| matches!(l.layer, Layer::Manual | Layer::Project))
        .collect())
}

pub(super) fn parsed_rules(rules: Vec<Rule>) -> Vec<(Rule, super::Pattern)> {
    rules
        .into_iter()
        .map(|r| {
            let p = r.parsed();
            (r, p)
        })
        .collect()
}

pub(super) fn rule_for<'a>(nsid: &str, rules: &'a [(Rule, super::Pattern)]) -> Option<&'a Rule> {
    best_match(nsid, rules, |(_, p)| p).map(|(r, _)| r)
}

enum Refusal {
    /// Not an attempt at all, so nothing is recorded against the NSID.
    NotPublishable,
    Failed(String),
}

impl From<AppError> for Refusal {
    fn from(e: AppError) -> Self {
        Refusal::Failed(message(e))
    }
}

/// `actor` is `None` for auto-publish, which leaves a withdrawn lexicon alone.
pub async fn publish(state: &AppState, nsid: &str, actor: Option<&str>) -> PublishOutcome {
    let _write = WRITE.lock().await;

    // Reconcile picks what is due before taking the lock, so an unpublish can
    // land between that choice and this write; the mark is read again here.
    if actor.is_none() {
        match store::get_publication(state, nsid).await {
            Ok(Some(p)) if p.withdrawn_at.is_some() => {
                return PublishOutcome::failed(nsid, WITHDRAWN.into());
            }
            Ok(_) => {}
            Err(e) => return PublishOutcome::failed(nsid, message(e)),
        }
    }

    let error = match write(state, nsid).await {
        Ok((did, uri, cid)) => {
            if let Err(e) = store::record_success(state, nsid, &did, &uri, &cid).await {
                return PublishOutcome::failed(
                    nsid,
                    format!("published to {uri} but could not record it: {}", message(e)),
                );
            }
            log_event(
                &state.db,
                EventLog {
                    event_type: "lexicon.published".to_string(),
                    severity: Severity::Info,
                    actor_did: actor.map(str::to_string),
                    subject: Some(nsid.to_string()),
                    detail: json!({ "did": did, "uri": uri, "cid": cid }),
                },
                state.db_backend,
            )
            .await;
            return PublishOutcome {
                nsid: nsid.to_string(),
                ok: true,
                uri: Some(uri),
                cid: Some(cid),
                error: None,
            };
        }
        Err(Refusal::NotPublishable) => {
            return PublishOutcome::failed(nsid, "not a lexicon HappyView can publish".into());
        }
        Err(Refusal::Failed(error)) => error,
    };

    if let Err(e) = store::record_failure(state, nsid, &error).await {
        tracing::warn!(nsid, "failed to record lexicon publish failure: {e}");
    }
    log_event(
        &state.db,
        EventLog {
            event_type: "lexicon.publish_failed".to_string(),
            severity: Severity::Warn,
            actor_did: actor.map(str::to_string),
            subject: Some(nsid.to_string()),
            detail: json!({ "error": error }),
        },
        state.db_backend,
    )
    .await;
    PublishOutcome::failed(nsid, error)
}

/// Returns `(did, uri, cid)` as the PDS reported them.
async fn write(state: &AppState, nsid: &str) -> Result<(String, String, String), Refusal> {
    let listing = publishable(state)
        .await?
        .into_iter()
        .find(|l| l.id == nsid)
        .ok_or(Refusal::NotPublishable)?;

    let rules = parsed_rules(store::list_rules(state).await?);
    let rule = rule_for(nsid, &rules)
        .ok_or_else(|| Refusal::Failed(format!("no publishing default covers {nsid}")))?;

    let linked = target::LinkedRepoTargets::load(state).await?;
    let instance = target::instance_target(state).await?;
    let target = target::target_for_rule(state, rule, &linked, &instance).await;
    let Some(did) = target.writable_did().map(str::to_string) else {
        return Err(Refusal::Failed(
            target
                .unavailable_reason
                .unwrap_or_else(|| "the publishing target is unavailable".into()),
        ));
    };

    let Some(local_cid) = record_cid(&listing.lexicon_json) else {
        return Err(Refusal::Failed(
            "this lexicon cannot be encoded as a record".into(),
        ));
    };
    let record = schema_record(&listing.lexicon_json);

    let (uri, cid) = if target.kind == KIND_INSTANCE {
        write_instance(state, &did, nsid, record).await?
    } else {
        let grant = find_grant(&linked.grants, &target)?;
        crate::linked_repos::pds::put_record(state, grant, SCHEMA_COLLECTION, nsid, record, None)
            .await?
    };
    // Status compares the stored CID against this local one, so a PDS that
    // hashes the record differently leaves the lexicon outdated and
    // republished at every trigger; this is the only place that shows why.
    if cid != local_cid {
        tracing::warn!(
            nsid,
            pds_cid = %cid,
            local_cid = %local_cid,
            "the PDS reported a different CID for a published lexicon than HappyView computed"
        );
    }
    Ok((did, uri, cid))
}

fn find_grant<'a>(
    grants: &'a [LinkedRepo],
    target: &TargetInfo,
) -> Result<&'a LinkedRepo, Refusal> {
    grants
        .iter()
        .find(|g| Some(&g.id) == target.linked_repo_id.as_ref())
        .ok_or_else(|| Refusal::Failed(target::REMOVED_LINKED_REPO.into()))
}

/// An expired attached-account session is a credential HappyView holds that
/// an operator has to renew, so it is a 4xx; any other PDS failure is the
/// PDS's.
async fn instance_call(
    state: &AppState,
    did: &str,
    method: &str,
    body: &Value,
) -> Result<Value, AppError> {
    let session = crate::repo::session::get_oauth_session(state, did)
        .await
        .map_err(|e| match e {
            AppError::Auth(_) => AppError::Auth(SIGN_IN_AGAIN.to_string()),
            other => other,
        })?;
    crate::linked_repos::pds::post_json(&session, method, body)
        .await
        .map_err(|e| {
            if crate::setup::is_pds_session_expired(&e) {
                AppError::Auth(SIGN_IN_AGAIN.to_string())
            } else {
                AppError::BadGateway(message(e))
            }
        })
}

async fn write_instance(
    state: &AppState,
    did: &str,
    nsid: &str,
    record: Value,
) -> Result<(String, String), Refusal> {
    let body = json!({
        "repo": did,
        "collection": SCHEMA_COLLECTION,
        "rkey": nsid,
        "record": record,
    });
    let out = instance_call(state, did, "com.atproto.repo.putRecord", &body).await?;
    Ok(crate::linked_repos::pds::extract_uri_cid(
        &out,
        "putRecord",
    )?)
}

pub async fn unpublish(state: &AppState, nsid: &str, actor: &str) -> Result<(), AppError> {
    let _write = WRITE.lock().await;

    let not_published = || AppError::NotFound(format!("{nsid} is not published"));
    let publication = store::get_publication(state, nsid)
        .await?
        .ok_or_else(not_published)?;
    let did = publication.did.clone().ok_or_else(not_published)?;

    let attached = crate::service_identity::get_identity(&state.db, state.db_backend)
        .await?
        .and_then(|i| i.attached_account_did);

    if attached.as_deref() == Some(did.as_str()) {
        let body = json!({ "repo": did, "collection": SCHEMA_COLLECTION, "rkey": nsid });
        instance_call(state, &did, "com.atproto.repo.deleteRecord", &body).await?;
    } else if let Some(grant) = crate::linked_repos::db::get_by_did(state, &did).await? {
        crate::linked_repos::pds::delete_record(state, &grant, SCHEMA_COLLECTION, nsid).await?;
    } else {
        return Err(AppError::Conflict(format!(
            "no linked repo or attached account holds {did} any more; delete the record from \
             that account directly"
        )));
    }

    store::record_withdrawn(state, nsid).await?;
    log_event(
        &state.db,
        EventLog {
            event_type: "lexicon.unpublished".to_string(),
            severity: Severity::Info,
            actor_did: Some(actor.to_string()),
            subject: Some(nsid.to_string()),
            detail: json!({ "did": did, "uri": publication.uri }),
        },
        state.db_backend,
    )
    .await;
    Ok(())
}

/// Runs work so that at most one run is in progress, and every trigger is
/// followed by a run that started after it.
///
/// A trigger that finds a run in progress only marks the work dirty and
/// returns; the runner keeps going while the mark is set. Waiting on the lock
/// instead would queue one task per trigger to redo the same work, and simply
/// returning would lose a change that landed after the runner read its state.
/// The runner checks the mark once more after releasing the lock, because a
/// trigger can arrive between its last check and the release.
#[derive(Default)]
struct SingleFlight {
    lock: Mutex<()>,
    dirty: AtomicBool,
}

impl SingleFlight {
    async fn run<F, Fut>(&self, mut work: F)
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = ()>,
    {
        self.dirty.store(true, Ordering::SeqCst);
        loop {
            let Ok(guard) = self.lock.try_lock() else {
                return;
            };
            while self.dirty.swap(false, Ordering::SeqCst) {
                work().await;
            }
            drop(guard);
            if !self.dirty.load(Ordering::SeqCst) {
                return;
            }
        }
    }
}

/// Publishes every lexicon whose rule auto-publishes and whose record is
/// missing, elsewhere, or stale. A withdrawn lexicon is missing on purpose and
/// is left alone.
pub async fn reconcile(state: AppState) {
    RECONCILE.run(|| reconcile_once(&state)).await;
}

pub fn spawn_reconcile(state: &AppState) {
    tokio::spawn(reconcile(state.clone()));
}

async fn reconcile_once(state: &AppState) {
    let due = match due_for_publish(state).await {
        Ok(due) => due,
        Err(e) => {
            tracing::warn!("lexicon publishing reconcile could not read its state: {e}");
            return;
        }
    };
    for nsid in due {
        let outcome = publish(state, &nsid, None).await;
        match outcome.error.as_deref() {
            None | Some(WITHDRAWN) => {}
            Some(error) => tracing::warn!(nsid, "auto-publish failed: {error}"),
        }
    }
}

async fn due_for_publish(state: &AppState) -> Result<Vec<String>, AppError> {
    let rules = parsed_rules(store::list_rules(state).await?);
    if !rules.iter().any(|(r, _)| r.auto_publish) {
        return Ok(Vec::new());
    }
    let linked = target::LinkedRepoTargets::load(state).await?;
    let instance = target::instance_target(state).await?;
    let views = super::overview::lexicon_views(state, &rules, &linked, &instance).await?;
    Ok(views
        .into_iter()
        .filter(|v| {
            v.auto_publish
                && matches!(
                    v.status,
                    PublishStatus::Unpublished | PublishStatus::Moved | PublishStatus::Outdated
                )
        })
        .map(|v| v.nsid)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use tokio::sync::Notify;

    #[derive(Default)]
    struct Probe {
        running: AtomicUsize,
        max_running: AtomicUsize,
        runs: AtomicUsize,
        started: Notify,
        release: Notify,
    }

    async fn work(probe: Arc<Probe>) {
        let now = probe.running.fetch_add(1, Ordering::SeqCst) + 1;
        probe.max_running.fetch_max(now, Ordering::SeqCst);
        let run = probe.runs.fetch_add(1, Ordering::SeqCst);
        if run == 0 {
            probe.started.notify_one();
            probe.release.notified().await;
        }
        probe.running.fetch_sub(1, Ordering::SeqCst);
    }

    #[tokio::test]
    async fn a_trigger_during_a_run_causes_one_more_run_and_never_a_parallel_one() {
        let flight = Arc::new(SingleFlight::default());
        let probe = Arc::new(Probe::default());

        let first = {
            let (flight, probe) = (flight.clone(), probe.clone());
            tokio::spawn(async move { flight.run(|| work(probe.clone())).await })
        };
        probe.started.notified().await;

        // Both arrive while the first run is in progress, so they coalesce.
        for _ in 0..2 {
            let p = probe.clone();
            flight.run(|| work(p.clone())).await;
        }
        assert_eq!(probe.runs.load(Ordering::SeqCst), 1);

        probe.release.notify_one();
        first.await.unwrap();

        assert_eq!(probe.runs.load(Ordering::SeqCst), 2);
        assert_eq!(probe.max_running.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_idle_trigger_runs_once() {
        let flight = SingleFlight::default();
        let probe = Arc::new(Probe::default());
        probe.release.notify_one();
        flight.run(|| work(probe.clone())).await;
        assert_eq!(probe.runs.load(Ordering::SeqCst), 1);
        assert!(!flight.dirty.load(Ordering::SeqCst));
    }

    #[test]
    fn message_drops_the_variant_prefix() {
        assert_eq!(message(AppError::Auth("x".into())), "x");
        assert_eq!(message(AppError::Internal("y".into())), "y");
    }
}
