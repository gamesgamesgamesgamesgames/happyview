use std::collections::HashMap;

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

pub const REMOVED_LINKED_REPO: &str = "the linked repo this rule used was removed";

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

    /// Keeps whether a linked repo can be written, and drops which account it
    /// is, for a caller allowed to see lexicons but not linked repos.
    pub fn redact_linked_repo(&mut self) {
        if self.kind == KIND_LINKED_REPO {
            self.linked_repo_id = None;
            self.did = None;
            self.handle = None;
            self.unavailable_reason = None;
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
                let session = attached_session_scope(state, &did).await?;
                return Ok(attached_account_target(did, session));
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

/// What the stored session for `did` says about its scope: `None` when there
/// is no session, `Some(None)` when the session records no scope.
async fn attached_session_scope(
    state: &AppState,
    did: &str,
) -> Result<Option<Option<String>>, AppError> {
    let data = crate::auth::oauth_store::stored_session_json(
        &state.db,
        state.db_backend,
        crate::auth::oauth_store::SESSIONS_TABLE,
        did,
        state.config.token_encryption_key.as_ref(),
    )
    .await
    .map_err(|e| AppError::Internal(format!("failed to read the attached session: {e}")))?;
    Ok(data.map(|data| crate::linked_repos::db::session_scope(&data)))
}

/// The attached account signs in through the dashboard's own OAuth client, so
/// whatever it was last signed in with is what publishing has to work with: a
/// plain dashboard login by the same account replaces the session with one
/// that cannot write records.
///
/// A session that records no scope is judged available, since nothing says it
/// cannot write; the PDS decides, and its refusal names the fix.
pub fn attached_account_target(did: String, session: Option<Option<String>>) -> TargetInfo {
    let granted = match session {
        None => {
            return TargetInfo::unavailable(
                KIND_INSTANCE,
                "the attached account needs to sign in again from Service Identity",
            );
        }
        Some(granted) => granted,
    };
    let scopes = granted.as_deref().map(scope::parse);
    let allows = |action| {
        scopes
            .as_deref()
            .is_none_or(|s| scope::allows_repo(s, SCHEMA_COLLECTION, action))
    };
    if !(allows(RepoAction::Create) && allows(RepoAction::Update)) {
        return TargetInfo::unavailable(
            KIND_INSTANCE,
            "the attached account hasn't granted permission to write lexicon records. \
             Re-authenticate it from Service Identity",
        );
    }
    TargetInfo {
        kind: KIND_INSTANCE.to_string(),
        linked_repo_id: None,
        did: Some(did),
        handle: None,
        available: true,
        unavailable_reason: None,
        can_unpublish: allows(RepoAction::Delete),
    }
}

/// Every linked repo beside the scope its PDS granted, loaded once so a view
/// judging many rules reads the session table once.
pub struct LinkedRepoTargets {
    pub grants: Vec<LinkedRepo>,
    granted_scopes: HashMap<String, String>,
}

impl LinkedRepoTargets {
    pub async fn load(state: &AppState) -> Result<Self, AppError> {
        Ok(LinkedRepoTargets {
            grants: crate::linked_repos::db::list(state).await?,
            granted_scopes: crate::linked_repos::db::granted_scopes(state).await?,
        })
    }

    pub fn target(&self, grant: &LinkedRepo) -> TargetInfo {
        let granted = grant
            .did
            .as_deref()
            .and_then(|did| self.granted_scopes.get(did))
            .map(String::as_str);
        linked_repo_target(grant, granted)
    }

    pub fn find(&self, id: &str) -> Option<&LinkedRepo> {
        self.grants.iter().find(|g| g.id == id)
    }

    pub fn targets(&self) -> impl Iterator<Item = TargetInfo> + '_ {
        self.grants.iter().map(|g| self.target(g))
    }
}

/// `putRecord` is an upsert, so publishing needs both `create` and `update`.
///
/// `granted_scope` is what the PDS approved, which can be less than the grant
/// asked for; it decides when known, and the requested scopes stand in when
/// it is not.
pub fn linked_repo_target(grant: &LinkedRepo, granted_scope: Option<&str>) -> TargetInfo {
    let requested = scope::parse(&grant.scopes);
    let granted = granted_scope.map(scope::parse);
    let effective = granted.as_deref().unwrap_or(&requested);
    let allows = |scopes: &[String], action| scope::allows_repo(scopes, SCHEMA_COLLECTION, action);
    let writes = |scopes: &[String]| {
        allows(scopes, RepoAction::Create) && allows(scopes, RepoAction::Update)
    };

    let reason = if grant.did.is_none() {
        Some("this linked repo has not been authorized yet".to_string())
    } else if grant.status == STATUS_NEEDS_REAUTH {
        Some("this linked repo needs to be reauthorized".to_string())
    } else if grant.status != STATUS_ACTIVE {
        Some(format!("this linked repo is {}", grant.status))
    } else if !writes(&requested) {
        Some(
            "this linked repo doesn't have permission to write lexicon records. Remove it and \
             link it again with the Lexicon publishing preset"
                .to_string(),
        )
    } else if !writes(effective) {
        Some(
            "this linked repo's PDS granted less access than requested. Try linking it again later"
                .to_string(),
        )
    } else {
        None
    };

    TargetInfo {
        kind: KIND_LINKED_REPO.to_string(),
        linked_repo_id: Some(grant.id.clone()),
        did: grant.did.clone(),
        handle: grant.handle.clone(),
        available: reason.is_none(),
        can_unpublish: reason.is_none() && allows(effective, RepoAction::Delete),
        unavailable_reason: reason,
    }
}

pub async fn list_targets(state: &AppState) -> Result<Vec<TargetInfo>, AppError> {
    let mut targets = vec![instance_target(state).await?];
    targets.extend(LinkedRepoTargets::load(state).await?.targets());
    Ok(targets)
}

/// A rule whose linked repo is gone reports that and never falls back to a
/// broader rule: the operator chose that account for these lexicons, and
/// publishing elsewhere would put them somewhere nobody picked.
pub async fn target_for_rule(
    _state: &AppState,
    rule: &Rule,
    linked: &LinkedRepoTargets,
    instance: &TargetInfo,
) -> TargetInfo {
    match rule.target_kind.as_str() {
        KIND_INSTANCE => instance.clone(),
        KIND_LINKED_REPO => {
            match rule
                .linked_repo_id
                .as_deref()
                .and_then(|id| linked.find(id))
            {
                Some(grant) => linked.target(grant),
                None => TargetInfo {
                    linked_repo_id: rule.linked_repo_id.clone(),
                    ..TargetInfo::unavailable(KIND_LINKED_REPO, REMOVED_LINKED_REPO)
                },
            }
        }
        other => TargetInfo::unavailable(other, format!("unknown target kind {other}")),
    }
}
