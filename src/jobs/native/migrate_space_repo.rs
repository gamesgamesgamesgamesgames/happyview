//! `happyview.migrate-space-repo`: move one permissioned repo to the user's PDS.
//!
//! HappyView's spaces storage is a polyfill for PDSes that cannot host spaces
//! themselves. This job moves a repo off it: replay the repo's records into the
//! user's PDS, verify the handoff, and only then switch the source of truth.
//!
//! The job keeps three properties:
//!
//! - **Nothing is deleted.** The local copy becomes the index, which is also
//!   what makes the switch reversible if a PDS later drops support.
//! - **Every migration is verified.** The PDS's signed commit must both
//!   authenticate *and* agree with a fold over the records we sent. If either
//!   check fails, the repo returns to `polyfill`.
//! - **Consent is the OAuth grant.** Without a session carrying `space:` scope
//!   the job cannot write to the PDS, and it is skipped rather than failed.

use crate::AppState;
use crate::error::AppError;
use crate::spaces::host_mode::{HostMode, can_transition};
use crate::spaces::lthash::{self, LtHashState};
use crate::spaces::types::SpaceRecord;
use crate::spaces::{db, native_client};

use super::super::{Job, db as jobs_db};
use super::NativeOutcome;

pub const JOB_TYPE: &str = "happyview.migrate-space-repo";

/// Records per `applyWrites` call.
///
/// Sized so a large repo is not sent as one request the PDS may refuse, and so
/// progress is visible while the job runs.
const BATCH_SIZE: usize = 50;

struct Input {
    space_id: String,
    author_did: String,
}

fn parse_input(input: &serde_json::Value) -> Option<Input> {
    Some(Input {
        space_id: input.get("space_id")?.as_str()?.to_string(),
        author_did: input.get("author_did")?.as_str()?.to_string(),
    })
}

/// The scope string a stored OAuth session was granted.
///
/// Read from the persisted session rather than from what was *requested* at
/// login: the two differ whenever a user declines part of a consent screen, and
/// acting on the request would enqueue migrations that cannot run.
pub async fn granted_scope(state: &AppState, did: &str) -> Result<Option<String>, AppError> {
    let sql = crate::db::adapt_sql(
        "SELECT session_data FROM happyview_oauth_sessions WHERE did = ?",
        state.db_backend,
    );
    let row: Option<(String,)> = crate::db::query_as(&sql)
        .bind(did)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to read oauth session: {e}")))?;

    let Some((data,)) = row else {
        return Ok(None);
    };
    let parsed: serde_json::Value = serde_json::from_str(&data)
        .map_err(|e| AppError::Internal(format!("oauth session is not valid JSON: {e}")))?;

    Ok(parsed
        .get("token_set")
        .and_then(|t| t.get("scope"))
        .and_then(|s| s.as_str())
        .map(str::to_string))
}

/// Whether a session may write this space's records on the user's behalf.
///
/// Replay creates every record, so this checks create permission for each
/// collection under the space's type.
pub fn scope_permits_migration(
    scope: &str,
    author_did: &str,
    space: &crate::spaces::types::Space,
    collections: &[String],
) -> bool {
    let granted = happyview_scopes::ScopePermissions::parse(scope);
    // An empty repo has no collection to test, so a covering read grant is
    // enough to establish the app was given this space at all.
    if collections.is_empty() {
        return granted.allows_space_for_user(
            author_did,
            &space.type_nsid,
            &space.authority_did,
            &space.skey,
            happyview_scopes::SpaceTarget::Read,
        );
    }
    collections.iter().all(|collection| {
        granted.allows_space_for_user(
            author_did,
            &space.type_nsid,
            &space.authority_did,
            &space.skey,
            happyview_scopes::SpaceTarget::Write {
                action: happyview_scopes::SpaceAction::Create,
                collection,
            },
        )
    })
}

/// Move a repo's mode, refusing transitions the flow does not define.
async fn set_mode(state: &AppState, input: &Input, to: HostMode) -> Result<(), AppError> {
    let mut conn = state
        .db
        .acquire()
        .await
        .map_err(|e| AppError::Internal(format!("failed to acquire connection: {e}")))?;
    let mut repo_state = db::get_or_create_repo_state(
        &mut conn,
        state.db_backend,
        &input.space_id,
        &input.author_did,
    )
    .await?;

    if repo_state.host_mode == to {
        return Ok(());
    }
    if !can_transition(repo_state.host_mode, to) {
        return Err(AppError::Internal(format!(
            "illegal host mode transition {} -> {to}",
            repo_state.host_mode
        )));
    }

    repo_state.host_mode = to;
    db::update_repo_state(&mut *conn, state.db_backend, &repo_state).await
}

fn skipped(reason: &str, detail: &str) -> NativeOutcome {
    // A skip completes the job rather than failing it. An account that has not
    // authorized HappyView is an expected state, and reporting it as an error
    // would send operators after a problem that does not exist.
    NativeOutcome::Completed(serde_json::json!({
        "status": "skipped",
        "reason": reason,
        "detail": detail,
    }))
}

pub async fn run(state: &AppState, job: &Job) -> NativeOutcome {
    let Some(input) = parse_input(&job.input) else {
        return NativeOutcome::Failed("input must carry space_id and author_did".to_string());
    };

    match migrate(state, job, &input).await {
        Ok(outcome) => outcome,
        Err(e) => {
            // Any failure returns the repo to polyfill. Migration never modifies
            // the local copy, so the user's repo keeps working as it did.
            if let Err(revert) = set_mode(state, &input, HostMode::Polyfill).await {
                tracing::error!(
                    space_id = %input.space_id,
                    author_did = %input.author_did,
                    error = %revert,
                    "failed to return repo to polyfill after a failed migration"
                );
            }
            NativeOutcome::Failed(format!("{e}"))
        }
    }
}

async fn migrate(state: &AppState, job: &Job, input: &Input) -> Result<NativeOutcome, AppError> {
    let space = {
        let mut conn = state
            .db
            .acquire()
            .await
            .map_err(|e| AppError::Internal(format!("failed to acquire connection: {e}")))?;
        db::get_space(&mut *conn, state.db_backend, &input.space_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("space {} not found", input.space_id)))?
    };

    // Idempotent: the worker resumes interrupted jobs, and a repo already moved
    // must not be moved again.
    {
        let mut conn = state
            .db
            .acquire()
            .await
            .map_err(|e| AppError::Internal(format!("failed to acquire connection: {e}")))?;
        let repo_state = db::get_or_create_repo_state(
            &mut conn,
            state.db_backend,
            &input.space_id,
            &input.author_did,
        )
        .await?;
        if repo_state.host_mode == HostMode::Native {
            return Ok(NativeOutcome::Completed(serde_json::json!({
                "status": "already-native",
            })));
        }
    }

    let records = db::list_all_space_records(
        &state.db,
        state.db_backend,
        &input.space_id,
        &input.author_did,
    )
    .await?;

    let collections: Vec<String> = {
        let mut c: Vec<String> = records.iter().map(|r| r.collection.clone()).collect();
        c.sort();
        c.dedup();
        c
    };

    // Consent gate. Records written through an API client may belong to a DID
    // with no session here at all.
    let Some(scope) = granted_scope(state, &input.author_did).await? else {
        return Ok(skipped(
            "awaiting_authorization",
            "no OAuth session is held for this account",
        ));
    };
    if !scope_permits_migration(&scope, &input.author_did, &space, &collections) {
        return Ok(skipped(
            "awaiting_authorization",
            "the account's session does not grant space: access for this space",
        ));
    }

    let session = crate::repo::get_oauth_session(state, &input.author_did).await?;

    set_mode(state, input, HostMode::Migrating).await?;

    let space_uri = format!(
        "at://{}/space/{}/{}",
        space.did, space.type_nsid, space.skey
    );

    // Replay in (collection, rkey) order, which is the order
    // `list_all_space_records` returns, so a resumed run repeats the same
    // sequence.
    let total = records.len();
    let mut written = 0usize;
    for (body, chunk) in replay_batches(&space_uri, &input.author_did, &records)
        .iter()
        .zip(records.chunks(BATCH_SIZE))
    {
        let resp = crate::repo::pds::pds_post_json_raw(
            state,
            &session,
            "com.atproto.space.applyWrites",
            body,
        )
        .await?;
        if !resp.status().is_success() {
            let detail = resp.text().await.unwrap_or_default();
            return Err(AppError::Internal(format!(
                "applyWrites rejected a batch: {detail}"
            )));
        }

        written += chunk.len();
        let _ = jobs_db::update_progress(
            state,
            &job.id,
            &serde_json::json!({ "records_written": written, "records_total": total }),
        )
        .await;
    }

    // Verify with two independent checks. Both must pass.
    let commit =
        native_client::get_latest_commit(state, &session, &space_uri, &input.author_did).await?;

    // Authenticity: the commit carries a valid signature from the author's key.
    let key =
        native_client::author_signing_key(&state.http, &state.config.plc_url, &input.author_did)
            .await?;
    crate::spaces::commit::verify_commit(&commit, &space_uri, &input.author_did, &key)?;

    // Agreement: the signed hash describes the records we sent. An authentic
    // commit can still summarise a different record set.
    if commit.hash != expected_commit_hash(&records) {
        return Err(AppError::Internal(format!(
            "the PDS's commit does not describe the {total} records we replayed; \
             leaving this repo on HappyView"
        )));
    }

    // Handoff confirmed. Record where to resume syncing from before flipping,
    // so a native repo is never left without a cursor.
    {
        let mut conn = state
            .db
            .acquire()
            .await
            .map_err(|e| AppError::Internal(format!("failed to acquire connection: {e}")))?;
        let mut repo_state = db::get_or_create_repo_state(
            &mut conn,
            state.db_backend,
            &input.space_id,
            &input.author_did,
        )
        .await?;
        repo_state.sync_cursor = Some(commit.rev.clone());
        db::update_repo_state(&mut *conn, state.db_backend, &repo_state).await?;
    }

    set_mode(state, input, HostMode::Native).await?;

    Ok(NativeOutcome::Completed(serde_json::json!({
        "status": "migrated",
        "records": total,
        "rev": commit.rev,
    })))
}

/// The `applyWrites` bodies that replay a repo's records onto its new host.
///
/// Pure, so interop tests can send the migration's request bodies to real hosts
/// without an OAuth session.
pub fn replay_batches(
    space_uri: &str,
    author_did: &str,
    records: &[SpaceRecord],
) -> Vec<serde_json::Value> {
    records
        .chunks(BATCH_SIZE)
        .map(|chunk| {
            let writes: Vec<serde_json::Value> = chunk
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "$type": "com.atproto.space.applyWrites#create",
                        "collection": r.collection,
                        "rkey": r.rkey,
                        "value": r.record,
                    })
                })
                .collect();
            serde_json::json!({
                "space": space_uri,
                "repo": author_did,
                "writes": writes,
            })
        })
        .collect()
}

/// The set hash a host must sign once it holds `records` and nothing else.
///
/// Folded over the CIDs HappyView stored, not ones the host reports: the check
/// is that the host re-derived the same CID from the same record value.
pub fn expected_commit_hash(records: &[SpaceRecord]) -> [u8; 32] {
    let mut fold = LtHashState::new();
    for r in records {
        fold.add(&lthash::record_element(&r.collection, &r.rkey, &r.cid));
    }
    fold.hash()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spaces::types::{AppAccess, Policy, Space, SpaceConfig};

    const USER: &str = "did:plc:aaaaaaaaaaaaaaaaaaaaaaaa";

    fn a_space() -> Space {
        Space {
            id: "sp1".into(),
            did: USER.into(),
            authority_did: USER.into(),
            creator_did: USER.into(),
            type_nsid: "com.example.forum".into(),
            skey: "main".into(),
            display_name: None,
            description: None,
            read_policy: Policy::MemberList,
            write_policy: Policy::MemberList,
            app_access: AppAccess::Open,
            config: SpaceConfig::default(),
            revision: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn input_requires_both_fields() {
        assert!(parse_input(&serde_json::json!({ "space_id": "s" })).is_none());
        assert!(parse_input(&serde_json::json!({ "author_did": "d" })).is_none());
        assert!(parse_input(&serde_json::json!({ "space_id": "s", "author_did": "d" })).is_some());
    }

    #[test]
    fn a_self_grant_permits_migrating_the_users_own_space() {
        // No `authority`: it defaults to `self`, the form a PDS stores.
        let scope = "space:com.example.forum?collection=com.example.a";
        let collections = vec!["com.example.a".to_string()];
        assert!(scope_permits_migration(
            scope,
            USER,
            &a_space(),
            &collections
        ));
        assert!(!scope_permits_migration(
            scope,
            "did:plc:bbbbbbbbbbbbbbbbbbbbbbbb",
            &a_space(),
            &collections
        ));
    }

    #[test]
    fn a_grant_covering_every_collection_permits_migration() {
        let scope = format!(
            "space:com.example.forum?authority={USER}&collection=com.example.a&collection=com.example.b"
        );
        let collections = vec!["com.example.a".to_string(), "com.example.b".to_string()];
        assert!(scope_permits_migration(
            &scope,
            USER,
            &a_space(),
            &collections
        ));
    }

    #[test]
    fn a_grant_missing_one_collection_does_not_permit_migration() {
        // Partial replay would leave the PDS holding a subset, which
        // verification rejects. Skipping first avoids the wasted writes.
        let scope = format!("space:com.example.forum?authority={USER}&collection=com.example.a");
        let collections = vec!["com.example.a".to_string(), "com.example.b".to_string()];
        assert!(!scope_permits_migration(
            &scope,
            USER,
            &a_space(),
            &collections
        ));
    }

    #[test]
    fn a_read_only_grant_does_not_permit_migration() {
        let scope = format!(
            "space:com.example.forum?authority={USER}&collection=com.example.a&action=read"
        );
        let collections = vec!["com.example.a".to_string()];
        assert!(!scope_permits_migration(
            &scope,
            USER,
            &a_space(),
            &collections
        ));
    }

    #[test]
    fn an_empty_repo_needs_only_a_covering_grant() {
        let scope = format!("space:com.example.forum?authority={USER}");
        assert!(scope_permits_migration(&scope, USER, &a_space(), &[]));

        let other = format!("space:com.example.other?authority={USER}");
        assert!(!scope_permits_migration(&other, USER, &a_space(), &[]));
    }

    #[test]
    fn a_grant_for_another_authority_does_not_permit_migration() {
        let scope = "space:com.example.forum?authority=did:plc:bbbbbbbbbbbbbbbbbbbbbbbb&collection=com.example.a";
        let collections = vec!["com.example.a".to_string()];
        assert!(!scope_permits_migration(
            scope,
            USER,
            &a_space(),
            &collections
        ));
    }
}
