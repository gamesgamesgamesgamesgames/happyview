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
    let data = crate::auth::oauth_store::stored_session_json(
        &state.db,
        state.db_backend,
        crate::auth::oauth_store::SESSIONS_TABLE,
        did,
        state.config.token_encryption_key.as_ref(),
    )
    .await
    .map_err(|e| AppError::Internal(format!("failed to read oauth session: {e}")))?;

    let Some(data) = data else {
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
        Err(e) => failed(state, job, &input, e).await,
    }
}

/// The outcome of a run that failed. A failure returns the repo to
/// polyfill: migration never modifies the local copy, so the user's repo
/// keeps working as it did. Unless the run's job was taken over while it
/// failed (a run stuck in a call to the PDS across the takeover): the run
/// that took it may have moved the repo on already, and the mode is its.
async fn failed(state: &AppState, job: &Job, input: &Input, e: AppError) -> NativeOutcome {
    if jobs_db::should_stop(state, &job.id).await != Some("lost")
        && let Err(revert) = set_mode(state, input, HostMode::Polyfill).await
    {
        tracing::error!(
            space_id = %input.space_id,
            author_did = %input.author_did,
            error = %revert,
            "failed to return repo to polyfill after a failed migration"
        );
    }
    NativeOutcome::Failed(format!("{e}"))
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
    let batches = replay_batches(&space_uri, &input.author_did, &records)
        .into_iter()
        .zip(records.chunks(BATCH_SIZE).map(<[SpaceRecord]>::len));
    let replayed = replay(state, job, batches, total, |body| {
        let session = &session;
        async move {
            let resp = crate::repo::pds::pds_post_json_raw(
                state,
                session,
                "com.atproto.space.applyWrites",
                &body,
            )
            .await?;
            if !resp.status().is_success() {
                let detail = resp.text().await.unwrap_or_default();
                return Err(AppError::Internal(format!(
                    "applyWrites rejected a batch: {detail}"
                )));
            }
            Ok(())
        }
    })
    .await?;
    if let Replayed::Stopped { reason, written } = replayed {
        return Ok(stopped(state, input, reason, written, total).await);
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

    // Handoff confirmed.
    if let Some(reason) = hand_off(state, job, input, &commit.rev).await? {
        return Ok(stopped(state, input, reason, total, total).await);
    }

    Ok(NativeOutcome::Completed(serde_json::json!({
        "status": "migrated",
        "records": total,
        "rev": commit.rev,
    })))
}

/// Record where to resume syncing from, then make the PDS the repo's source
/// of truth. The cursor goes first, so a native repo is never left without
/// one. A run asked to stop, or whose job was taken over, while the handoff
/// was being checked writes neither, and answers why.
async fn hand_off(
    state: &AppState,
    job: &Job,
    input: &Input,
    rev: &str,
) -> Result<Option<&'static str>, AppError> {
    if let Some(reason) = jobs_db::should_stop(state, &job.id).await {
        return Ok(Some(reason));
    }
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
        repo_state.sync_cursor = Some(rev.to_string());
        db::update_repo_state(&mut *conn, state.db_backend, &repo_state).await?;
    }

    set_mode(state, input, HostMode::Native).await?;
    Ok(None)
}

/// How far a replay got.
#[derive(Debug, PartialEq)]
enum Replayed {
    All,
    /// The run was asked to stop, or lost its job, after `written` records.
    Stopped {
        reason: &'static str,
        written: usize,
    },
}

/// Send each `(body, records in it)` batch in order with `send`, recording
/// progress after each, and asking before each whether the run is to stop: a
/// paused or cancelled job, or a run whose job was taken over, sends no more.
async fn replay<F, Fut>(
    state: &AppState,
    job: &Job,
    batches: impl IntoIterator<Item = (serde_json::Value, usize)>,
    total: usize,
    mut send: F,
) -> Result<Replayed, AppError>
where
    F: FnMut(serde_json::Value) -> Fut,
    Fut: std::future::Future<Output = Result<(), AppError>>,
{
    let mut written = 0usize;
    for (body, records) in batches {
        if let Some(reason) = jobs_db::should_stop(state, &job.id).await {
            return Ok(Replayed::Stopped { reason, written });
        }
        send(body).await?;
        written += records;
        let _ = jobs_db::update_progress(
            state,
            &job.id,
            &serde_json::json!({ "records_written": written, "records_total": total }),
        )
        .await;
    }
    Ok(Replayed::All)
}

/// The outcome of a run that stopped before the handoff. Paused or
/// cancelled, the repo goes back to `polyfill`, as after a failure, since the
/// PDS holds only part of it; a resumed job replays from the start. A run
/// whose job was taken over leaves the mode to the run that took it.
async fn stopped(
    state: &AppState,
    input: &Input,
    reason: &'static str,
    written: usize,
    total: usize,
) -> NativeOutcome {
    if reason != "lost"
        && let Err(e) = set_mode(state, input, HostMode::Polyfill).await
    {
        tracing::error!(
            space_id = %input.space_id,
            author_did = %input.author_did,
            error = %e,
            "failed to return repo to polyfill after a stopped migration"
        );
    }
    NativeOutcome::Completed(serde_json::json!({
        "status": "stopped",
        "stopped": reason,
        "records_written": written,
        "records_total": total,
    }))
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

    /// A job to migrate `USER`'s repo in space `sp1`, and the state it runs
    /// on, with the repo mid-migration.
    async fn migrating() -> (AppState, Job, Input) {
        let state = crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        );
        let id = jobs_db::create_job(
            &state,
            JOB_TYPE,
            &serde_json::json!({ "space_id": "sp1", "author_did": USER }),
            USER,
            false,
            None,
            None,
        )
        .await
        .unwrap();
        let job = jobs_db::get_job(&state, &id).await.unwrap().unwrap();
        db::create_space(&state.db, state.db_backend, &a_space())
            .await
            .unwrap();
        let input = Input {
            space_id: "sp1".into(),
            author_did: USER.into(),
        };
        set_mode(&state, &input, HostMode::Migrating).await.unwrap();
        (state, job, input)
    }

    async fn ask(state: &AppState, job: &Job, status: &str) {
        crate::db::query("UPDATE happyview_jobs SET status = ? WHERE id = ?")
            .bind(status)
            .bind(&job.id)
            .execute(&state.db)
            .await
            .unwrap();
    }

    async fn repo(state: &AppState, input: &Input) -> crate::spaces::types::RepoState {
        let mut conn = state.db.acquire().await.unwrap();
        db::get_or_create_repo_state(
            &mut conn,
            state.db_backend,
            &input.space_id,
            &input.author_did,
        )
        .await
        .unwrap()
    }

    /// A pause or cancel asked mid-replay sends no batch after it.
    #[tokio::test]
    async fn a_replay_asked_to_stop_sends_no_more_batches() {
        for asked in ["cancelling", "pausing"] {
            let (state, job, _) = migrating().await;
            let sent = std::cell::Cell::new(0);
            let batches = (0..4).map(|i| (serde_json::json!({ "batch": i }), 50));
            let replayed = replay(&state, &job, batches, 200, |_| {
                sent.set(sent.get() + 1);
                let now = sent.get();
                let (state, job) = (&state, &job);
                async move {
                    if now == 2 {
                        ask(state, job, asked).await;
                    }
                    Ok(())
                }
            })
            .await
            .unwrap();

            assert_eq!(sent.get(), 2, "{asked}");
            assert_eq!(
                replayed,
                Replayed::Stopped {
                    reason: asked,
                    written: 100,
                }
            );
        }
    }

    /// A run asked to stop after the handoff was checked neither records a
    /// cursor nor makes the PDS the source of truth.
    #[tokio::test]
    async fn a_handoff_asked_to_stop_does_not_flip_the_repo() {
        let (state, job, input) = migrating().await;
        ask(&state, &job, "cancelling").await;

        let stopped_for = hand_off(&state, &job, &input, "rev1").await.unwrap();

        assert_eq!(stopped_for, Some("cancelling"));
        let repo = repo(&state, &input).await;
        assert_eq!(repo.host_mode, HostMode::Migrating);
        assert_eq!(repo.sync_cursor, None);
    }

    #[tokio::test]
    async fn a_handoff_not_asked_to_stop_flips_the_repo() {
        let (state, job, input) = migrating().await;

        assert_eq!(hand_off(&state, &job, &input, "rev1").await.unwrap(), None);
        let repo = repo(&state, &input).await;
        assert_eq!(repo.host_mode, HostMode::Native);
        assert_eq!(repo.sync_cursor.as_deref(), Some("rev1"));
    }

    /// Paused or cancelled, the repo goes back to polyfill; a run whose job
    /// was taken over leaves it to the run that took it.
    #[tokio::test]
    async fn a_stopped_migration_returns_the_repo_unless_its_job_was_taken_over() {
        for (reason, mode) in [
            ("cancelling", HostMode::Polyfill),
            ("pausing", HostMode::Polyfill),
            ("lost", HostMode::Migrating),
        ] {
            let (state, _, input) = migrating().await;
            let out = stopped(&state, &input, reason, 50, 200).await;
            assert!(
                matches!(&out, NativeOutcome::Completed(v) if v["stopped"] == reason),
                "{reason}"
            );
            assert_eq!(repo(&state, &input).await.host_mode, mode, "{reason}");
        }
    }

    /// A failed run returns the repo to polyfill, unless its job was taken
    /// over: the run that took it may have finished the move, and a stale
    /// run failing late must not undo it.
    #[tokio::test]
    async fn a_failed_run_returns_the_repo_unless_its_job_was_taken_over() {
        use super::super::super::held::{Held, Lease, run_under};

        for (case, taken_over, mode) in [
            ("its own", false, HostMode::Polyfill),
            ("taken over", true, HostMode::Native),
        ] {
            let (state, job, input) = migrating().await;
            let claimed = jobs_db::hold_job(&state, &job.id, "here")
                .await
                .unwrap()
                .unwrap();
            // The repo as the run that took the job over left it.
            if taken_over {
                set_mode(&state, &input, HostMode::Native).await.unwrap();
            }
            let attempt = claimed.attempt + i64::from(taken_over);
            let run = Held::new(
                &job.id,
                Lease {
                    owner: "here".into(),
                    attempt,
                },
            );
            let out = run_under(
                run,
                failed(
                    &state,
                    &job,
                    &input,
                    AppError::Internal("the PDS went away".into()),
                ),
            )
            .await;

            assert!(matches!(out, NativeOutcome::Failed(_)), "{case}");
            assert_eq!(repo(&state, &input).await.host_mode, mode, "{case}");
        }
    }
}
