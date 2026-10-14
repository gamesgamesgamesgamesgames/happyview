use std::collections::BTreeSet;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};
use crate::lexicon_publishing::target::{KIND_INSTANCE, KIND_LINKED_REPO};
use crate::lexicon_publishing::{
    self as publishing, Overview, Pattern, PublishOutcome, RuleView, TargetInfo, store,
};

use super::auth::UserAuth;
use super::permissions::Permission;

const MAX_PUBLISH_BATCH: usize = 500;
const DNS_TIMEOUT: Duration = Duration::from_secs(5);
const DNS_CONCURRENCY: usize = 8;

/// GET /admin/lexicon-publishing
pub(super) async fn overview(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<Overview>, AppError> {
    auth.require(Permission::LexiconsRead).await?;
    Ok(Json(publishing::overview(&state).await?))
}

#[derive(Debug, Serialize)]
pub(super) struct DnsEntry {
    authority: String,
    record_name: String,
    did: Option<String>,
    error: Option<String>,
}

/// GET /admin/lexicon-publishing/dns
pub(super) async fn dns(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<Vec<DnsEntry>>, AppError> {
    auth.require(Permission::LexiconsRead).await?;

    // DNS names are case-insensitive, so two NSIDs differing only in the
    // authority's case share one record.
    let authorities: BTreeSet<String> = publishing::publishable(&state)
        .await?
        .iter()
        .filter_map(|l| happyview_nsid::nsid_authority(&l.id).ok())
        .map(|a| a.to_ascii_lowercase())
        .collect();

    let mut entries: Vec<DnsEntry> = futures_util::stream::iter(authorities)
        .map(|authority| async move {
            let (did, error) =
                match tokio::time::timeout(DNS_TIMEOUT, publishing::lookup_lexicon_did(&authority))
                    .await
                {
                    Ok(Ok(did)) => (did, None),
                    Ok(Err(e)) => (None, Some(e)),
                    Err(_) => (
                        None,
                        Some(format!(
                            "DNS lookup timed out after {} seconds",
                            DNS_TIMEOUT.as_secs()
                        )),
                    ),
                };
            DnsEntry {
                record_name: format!("_lexicon.{authority}"),
                authority,
                did,
                error,
            }
        })
        .buffer_unordered(DNS_CONCURRENCY)
        .collect()
        .await;
    entries.sort_by(|a, b| a.authority.cmp(&b.authority));
    Ok(Json(entries))
}

#[derive(Deserialize)]
pub(super) struct CreateRuleBody {
    pattern: String,
    target_kind: String,
    #[serde(default)]
    linked_repo_id: Option<String>,
    #[serde(default)]
    auto_publish: bool,
}

/// Missing → `None`, `null` → `Some(None)`, a string → `Some(Some(s))`.
fn deserialize_optional_field<'de, D>(d: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::deserialize(d)?))
}

#[derive(Deserialize)]
pub(super) struct UpdateRuleBody {
    #[serde(default)]
    target_kind: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    linked_repo_id: Option<Option<String>>,
    #[serde(default)]
    auto_publish: Option<bool>,
}

/// Checks that a rule could publish to this target now, and returns the
/// linked repo id to store: an instance target never keeps one.
async fn validate_target(
    state: &AppState,
    target_kind: &str,
    linked_repo_id: Option<&str>,
) -> Result<(TargetInfo, Option<String>), AppError> {
    let (target, stored_id) = match target_kind {
        KIND_INSTANCE => (publishing::instance_target(state).await?, None),
        KIND_LINKED_REPO => {
            let id = linked_repo_id.ok_or_else(|| {
                AppError::BadRequest("a linked_repo target needs a linked_repo_id".into())
            })?;
            let grant = crate::linked_repos::db::get(state, id)
                .await?
                .ok_or_else(|| AppError::BadRequest("unknown linked repo".into()))?;
            (
                publishing::linked_repo_target(&grant),
                Some(grant.id.clone()),
            )
        }
        other => {
            return Err(AppError::BadRequest(format!(
                "target_kind must be '{KIND_INSTANCE}' or '{KIND_LINKED_REPO}', not '{other}'"
            )));
        }
    };
    if !target.available {
        return Err(AppError::BadRequest(
            target
                .unavailable_reason
                .clone()
                .unwrap_or_else(|| "this target cannot publish right now".into()),
        ));
    }
    Ok((target, stored_id))
}

async fn rule_view(state: &AppState, rule: store::Rule) -> Result<RuleView, AppError> {
    let grants = crate::linked_repos::db::list(state).await?;
    let instance = publishing::instance_target(state).await?;
    let target = publishing::target_for_rule(state, &rule, &grants, &instance).await;
    Ok(RuleView { rule, target })
}

async fn log_rule_event(state: &AppState, event_type: &str, actor: &str, rule: &store::Rule) {
    log_event(
        &state.db,
        EventLog {
            event_type: event_type.to_string(),
            severity: Severity::Info,
            actor_did: Some(actor.to_string()),
            subject: Some(rule.pattern.clone()),
            detail: json!({
                "rule_id": rule.id,
                "target_kind": rule.target_kind,
                "linked_repo_id": rule.linked_repo_id,
                "auto_publish": rule.auto_publish,
            }),
        },
        state.db_backend,
    )
    .await;
}

/// POST /admin/lexicon-publishing/rules
pub(super) async fn create_rule(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<CreateRuleBody>,
) -> Result<(StatusCode, Json<RuleView>), AppError> {
    auth.require(Permission::LexiconsPublish).await?;

    let pattern = Pattern::parse(&body.pattern).map_err(AppError::BadRequest)?;
    let (_, linked_repo_id) =
        validate_target(&state, &body.target_kind, body.linked_repo_id.as_deref()).await?;

    let rule = store::insert_rule(
        &state,
        &pattern,
        &body.target_kind,
        linked_repo_id.as_deref(),
        body.auto_publish,
        &auth.did,
    )
    .await?;

    log_rule_event(&state, "lexicon_publishing.rule_created", &auth.did, &rule).await;
    publishing::spawn_reconcile(&state);
    Ok((StatusCode::CREATED, Json(rule_view(&state, rule).await?)))
}

/// PATCH /admin/lexicon-publishing/rules/{id}
pub(super) async fn update_rule(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
    Json(body): Json<UpdateRuleBody>,
) -> Result<Json<RuleView>, AppError> {
    auth.require(Permission::LexiconsPublish).await?;

    let not_found = || AppError::NotFound("publishing default not found".into());
    let current = store::get_rule(&state, &id).await?.ok_or_else(not_found)?;

    let target_kind = body
        .target_kind
        .clone()
        .unwrap_or_else(|| current.target_kind.clone());
    let linked_repo_id = match &body.linked_repo_id {
        Some(v) => v.clone(),
        None => current.linked_repo_id.clone(),
    };
    let target_changed = target_kind != current.target_kind
        || (target_kind == KIND_LINKED_REPO && linked_repo_id != current.linked_repo_id);

    // An unchanged target is left alone even when it is unavailable, so a
    // default whose repo went away can still have auto-publish switched off.
    let linked_repo_update = if target_changed {
        let (_, stored) = validate_target(&state, &target_kind, linked_repo_id.as_deref()).await?;
        Some(stored)
    } else {
        None
    };

    let rule = store::update_rule(
        &state,
        &id,
        target_changed.then_some(target_kind.as_str()),
        linked_repo_update.as_ref().map(|v| v.as_deref()),
        body.auto_publish,
    )
    .await?
    .ok_or_else(not_found)?;

    log_rule_event(&state, "lexicon_publishing.rule_updated", &auth.did, &rule).await;
    publishing::spawn_reconcile(&state);
    Ok(Json(rule_view(&state, rule).await?))
}

/// DELETE /admin/lexicon-publishing/rules/{id}
pub(super) async fn delete_rule(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::LexiconsPublish).await?;

    let not_found = || AppError::NotFound("publishing default not found".into());
    let rule = store::get_rule(&state, &id).await?.ok_or_else(not_found)?;
    if !store::delete_rule(&state, &id).await? {
        return Err(not_found());
    }

    log_rule_event(&state, "lexicon_publishing.rule_deleted", &auth.did, &rule).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub(super) struct PublishBody {
    nsids: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct PublishResults {
    results: Vec<PublishOutcome>,
}

/// POST /admin/lexicon-publishing/publish
///
/// Answers 200 with one result per NSID whether or not each succeeded; the
/// failures are per-lexicon facts, not a failure of the request.
pub(super) async fn publish(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<PublishBody>,
) -> Result<Json<PublishResults>, AppError> {
    auth.require(Permission::LexiconsPublish).await?;

    if body.nsids.is_empty() {
        return Err(AppError::BadRequest("nsids must not be empty".into()));
    }
    if body.nsids.len() > MAX_PUBLISH_BATCH {
        return Err(AppError::BadRequest(format!(
            "at most {MAX_PUBLISH_BATCH} nsids can be published at once"
        )));
    }

    let mut results = Vec::with_capacity(body.nsids.len());
    for nsid in &body.nsids {
        results.push(publishing::publish(&state, nsid, Some(&auth.did)).await);
    }
    Ok(Json(PublishResults { results }))
}

#[derive(Deserialize)]
pub(super) struct UnpublishBody {
    nsid: String,
}

/// POST /admin/lexicon-publishing/unpublish
pub(super) async fn unpublish(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<UnpublishBody>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::LexiconsPublish).await?;
    publishing::unpublish(&state, &body.nsid, &auth.did).await?;
    Ok(StatusCode::NO_CONTENT)
}
