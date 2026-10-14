use serde::Serialize;

use crate::AppState;
use crate::error::AppError;
use crate::linked_repos::scope::{self, RepoAction};
use crate::linked_repos::types::{LinkedRepo, STATUS_ACTIVE, STATUS_NEEDS_REAUTH};
use crate::service_identity::{self, IdentityMode};

use super::SCHEMA_COLLECTION;
use super::store::Rule;

pub const KIND_INSTANCE: &str = "instance";
pub const KIND_LINKED_REPO: &str = "linked_repo";

pub const REMOVED_LINKED_REPO: &str = "the linked repo this default used was removed";

#[derive(Debug, Clone, Serialize)]
pub struct TargetInfo {
    pub kind: String,
    pub linked_repo_id: Option<String>,
    pub did: Option<String>,
    pub handle: Option<String>,
    pub available: bool,
    pub unavailable_reason: Option<String>,
    pub can_unpublish: bool,
}

impl TargetInfo {
    /// The DID a publish would write to, or `None` when it cannot write now.
    pub fn writable_did(&self) -> Option<&str> {
        if self.available {
            self.did.as_deref()
        } else {
            None
        }
    }

    fn unavailable(kind: &str, reason: impl Into<String>) -> Self {
        TargetInfo {
            kind: kind.to_string(),
            linked_repo_id: None,
            did: None,
            handle: None,
            available: false,
            unavailable_reason: Some(reason.into()),
            can_unpublish: false,
        }
    }
}

/// Only an attached account has a repo; did:web and did:plc identities are a
/// DID with service entries and nowhere to write a record.
pub async fn instance_target(state: &AppState) -> Result<TargetInfo, AppError> {
    let identity = service_identity::get_identity(&state.db, state.db_backend).await?;
    let reason = match identity {
        None => "the instance identity is not set up".to_string(),
        Some(identity) => match (identity.mode, identity.attached_account_did) {
            (IdentityMode::AttachAccount, Some(did)) => {
                return Ok(TargetInfo {
                    kind: KIND_INSTANCE.to_string(),
                    linked_repo_id: None,
                    did: Some(did),
                    handle: None,
                    available: true,
                    unavailable_reason: None,
                    can_unpublish: true,
                });
            }
            (IdentityMode::AttachAccount, None) => {
                "the instance identity has no attached account".to_string()
            }
            (IdentityMode::DidWeb, _) => {
                "the instance identity is did:web and has no repo".to_string()
            }
            (IdentityMode::DidPlc, _) => {
                "the instance identity is did:plc and has no repo".to_string()
            }
            (IdentityMode::NotExposed, _) => {
                "the instance identity is not exposed and has no repo".to_string()
            }
        },
    };
    Ok(TargetInfo::unavailable(KIND_INSTANCE, reason))
}

/// `putRecord` is an upsert, so publishing needs both `create` and `update`.
pub fn linked_repo_target(grant: &LinkedRepo) -> TargetInfo {
    let scopes = scope::parse(&grant.scopes);
    let allows = |action| scope::allows_repo(&scopes, SCHEMA_COLLECTION, action);

    let reason = if grant.did.is_none() {
        Some("this linked repo has not been authorized yet".to_string())
    } else if grant.status == STATUS_NEEDS_REAUTH {
        Some("this linked repo needs to be reauthorized".to_string())
    } else if grant.status != STATUS_ACTIVE {
        Some(format!("this linked repo is {}", grant.status))
    } else if !(allows(RepoAction::Create) && allows(RepoAction::Update)) {
        Some(format!(
            "this linked repo's grant cannot write {SCHEMA_COLLECTION} records; add it again \
             with repo:{SCHEMA_COLLECTION}?action=create&action=update"
        ))
    } else {
        None
    };

    TargetInfo {
        kind: KIND_LINKED_REPO.to_string(),
        linked_repo_id: Some(grant.id.clone()),
        did: grant.did.clone(),
        handle: grant.handle.clone(),
        available: reason.is_none(),
        can_unpublish: reason.is_none() && allows(RepoAction::Delete),
        unavailable_reason: reason,
    }
}

pub async fn list_targets(state: &AppState) -> Result<Vec<TargetInfo>, AppError> {
    let mut targets = vec![instance_target(state).await?];
    targets.extend(
        crate::linked_repos::db::list(state)
            .await?
            .iter()
            .map(linked_repo_target),
    );
    Ok(targets)
}

/// A rule whose linked repo is gone reports that and never falls back to a
/// broader rule: the operator chose that account for these lexicons, and
/// publishing elsewhere would put them somewhere nobody picked.
pub async fn target_for_rule(
    _state: &AppState,
    rule: &Rule,
    grants: &[LinkedRepo],
    instance: &TargetInfo,
) -> TargetInfo {
    match rule.target_kind.as_str() {
        KIND_INSTANCE => instance.clone(),
        KIND_LINKED_REPO => {
            let grant = rule
                .linked_repo_id
                .as_deref()
                .and_then(|id| grants.iter().find(|g| g.id == id));
            match grant {
                Some(grant) => linked_repo_target(grant),
                None => TargetInfo {
                    linked_repo_id: rule.linked_repo_id.clone(),
                    ..TargetInfo::unavailable(KIND_LINKED_REPO, REMOVED_LINKED_REPO)
                },
            }
        }
        other => TargetInfo::unavailable(other, format!("unknown target kind {other}")),
    }
}
