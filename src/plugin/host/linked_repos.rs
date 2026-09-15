//! Writing to a repo an admin has linked to this instance, from a library
//! plugin. Delegates to `linked_repos::pds` for the actual PDS calls, so
//! scope pre-checks, session restoration and reauth handling stay one path
//! shared with the admin API and the Lua `linked_repos` global.

use serde_json::Value;

use crate::AppState;
use crate::error::AppError;
use crate::linked_repos::types::LinkedRepo;
use crate::linked_repos::{db, pds};

use happyview_plugin_sdk::wire::{
    LinkedRepoBlobUpload, LinkedRepoCall, LinkedRepoInfo, LinkedRepoRecordCreate,
    LinkedRepoRecordDelete, LinkedRepoRecordPut, RecordRef,
};

#[derive(Debug, thiserror::Error)]
pub enum LinkedRepoError {
    #[error("{0}")]
    NotLinked(String),
    #[error("{0}")]
    Scope(String),
    #[error("{0}")]
    NeedsReauth(String),
    #[error("{0}")]
    Pds(String),
    #[error("{0}")]
    Database(String),
}

impl LinkedRepoError {
    /// The envelope code a guest sees. A missing grant, an out-of-scope
    /// write, a dead session, and a PDS refusal are four different problems
    /// with four different fixes, so none of them share a code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotLinked(_) => "NOT_LINKED",
            Self::Scope(_) => "SCOPE",
            Self::NeedsReauth(_) => "NEEDS_REAUTH",
            Self::Pds(_) => "PDS_ERROR",
            Self::Database(_) => "HOST_ERROR",
        }
    }
}

/// `linked_repos::pds`'s write functions report a missing scope as
/// `Forbidden` and a dead or reauth-needing session as `Auth` — both already
/// carry the operator-facing message `require_*_scope`/`session_for`
/// produce, so it survives unchanged. Everything else reaching this far
/// already spoke to the PDS (or tried to), so it is reported as such rather
/// than invented as something more specific.
impl From<AppError> for LinkedRepoError {
    fn from(e: AppError) -> Self {
        match e {
            AppError::Forbidden(msg) => Self::Scope(msg),
            AppError::Auth(msg) => Self::NeedsReauth(msg),
            other => Self::Pds(other.to_string()),
        }
    }
}

impl From<LinkedRepo> for LinkedRepoInfo {
    fn from(grant: LinkedRepo) -> Self {
        Self {
            id: grant.id,
            did: grant.did,
            handle: grant.handle,
            reason: grant.reason,
            status: grant.status,
            scopes: grant.scopes,
        }
    }
}

pub async fn list(state: &AppState) -> Result<Vec<LinkedRepoInfo>, LinkedRepoError> {
    let grants = db::list(state)
        .await
        .map_err(|e| LinkedRepoError::Database(e.to_string()))?;
    Ok(grants.into_iter().map(Into::into).collect())
}

/// A DID with no grant at all is a different fact from one whose grant lacks
/// scope or needs reauth — a script can only fix the latter two by asking an
/// admin to change something that already exists.
async fn grant_for(state: &AppState, did: &str) -> Result<LinkedRepo, LinkedRepoError> {
    db::get_by_did(state, did)
        .await
        .map_err(|e| LinkedRepoError::Database(e.to_string()))?
        .ok_or_else(|| LinkedRepoError::NotLinked(format!("no linked repo grant for {did}")))
}

pub async fn create_record(
    state: &AppState,
    spec: LinkedRepoRecordCreate,
) -> Result<RecordRef, LinkedRepoError> {
    let grant = grant_for(state, &spec.did).await?;
    let (uri, cid) = pds::create_record(
        state,
        &grant,
        &spec.collection,
        spec.rkey.as_deref(),
        spec.record,
    )
    .await?;
    Ok(RecordRef { uri, cid })
}

pub async fn put_record(
    state: &AppState,
    spec: LinkedRepoRecordPut,
) -> Result<RecordRef, LinkedRepoError> {
    let grant = grant_for(state, &spec.did).await?;
    let (uri, cid) = pds::put_record(
        state,
        &grant,
        &spec.collection,
        &spec.rkey,
        spec.record,
        spec.swap_cid.as_deref(),
    )
    .await?;
    Ok(RecordRef { uri, cid })
}

pub async fn delete_record(
    state: &AppState,
    spec: LinkedRepoRecordDelete,
) -> Result<(), LinkedRepoError> {
    let grant = grant_for(state, &spec.did).await?;
    pds::delete_record(state, &grant, &spec.collection, &spec.rkey).await?;
    Ok(())
}

pub async fn upload_blob(
    state: &AppState,
    spec: LinkedRepoBlobUpload,
) -> Result<Value, LinkedRepoError> {
    let grant = grant_for(state, &spec.did).await?;
    let result = pds::upload_blob(state, &grant, &spec.mime_type, spec.bytes).await?;
    Ok(result)
}

pub async fn call(state: &AppState, spec: LinkedRepoCall) -> Result<Value, LinkedRepoError> {
    let grant = grant_for(state, &spec.did).await?;
    let result = pds::call(state, &grant, &spec.method, spec.params, spec.input).await?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing here exercises a real PDS call, so no session or record table
    /// needs seeding: every test either stops at the grant lookup or at
    /// `session_for`'s status check, before any network write is attempted.
    async fn seeded_state() -> AppState {
        let pool = crate::test_support::migrated_memory_pool().await;
        crate::test_support::test_state_with_pool(pool)
    }

    /// Create a grant, bind it to `did`, and optionally push it into
    /// `needs_reauth` — the same sequence a stale linked-repo session goes
    /// through in production.
    async fn seed_grant(
        state: &AppState,
        did: &str,
        scopes: &str,
        needs_reauth: bool,
    ) -> LinkedRepo {
        let grant = db::create(state, None, None, None, scopes, "admin")
            .await
            .expect("create grant");
        db::bind_did(state, &grant.id, did, None)
            .await
            .expect("bind did");
        if needs_reauth {
            db::mark_needs_reauth(state, &grant.id, "test reauth")
                .await
                .expect("mark needs_reauth");
        }
        db::get_by_did(state, did)
            .await
            .expect("reload grant")
            .expect("grant exists")
    }

    #[tokio::test]
    async fn list_returns_a_seeded_grant() {
        let state = seeded_state().await;
        seed_grant(&state, "did:plc:seeded", "repo:*", false).await;

        let grants = list(&state).await.expect("list should succeed");
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].did.as_deref(), Some("did:plc:seeded"));
        assert_eq!(grants[0].scopes, "repo:*");
    }

    #[tokio::test]
    async fn grant_for_an_unknown_did_is_not_linked() {
        let state = seeded_state().await;
        let err = grant_for(&state, "did:plc:nobody").await.unwrap_err();
        assert!(matches!(err, LinkedRepoError::NotLinked(_)), "{err}");
        assert_eq!(err.code(), "NOT_LINKED");
    }

    #[tokio::test]
    async fn create_record_without_the_collection_scope_is_a_scope_error() {
        let state = seeded_state().await;
        seed_grant(&state, "did:plc:scoped", "repo:com.example.other", false).await;

        let err = create_record(
            &state,
            LinkedRepoRecordCreate {
                did: "did:plc:scoped".into(),
                collection: "com.example.note".into(),
                rkey: None,
                record: serde_json::json!({}),
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(err, LinkedRepoError::Scope(_)), "{err}");
        assert_eq!(err.code(), "SCOPE");
        assert!(err.to_string().contains("com.example.note"), "{err}");
    }

    #[tokio::test]
    async fn create_record_on_a_needs_reauth_grant_needs_reauth() {
        let state = seeded_state().await;
        seed_grant(&state, "did:plc:stale", "repo:*", true).await;

        let err = create_record(
            &state,
            LinkedRepoRecordCreate {
                did: "did:plc:stale".into(),
                collection: "com.example.note".into(),
                rkey: None,
                record: serde_json::json!({}),
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(err, LinkedRepoError::NeedsReauth(_)), "{err}");
        assert_eq!(err.code(), "NEEDS_REAUTH");
    }
}
