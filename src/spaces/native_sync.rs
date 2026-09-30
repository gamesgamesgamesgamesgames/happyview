//! Keeping HappyView's index in step with a repo hosted on a user's PDS.
//!
//! Once a repo is `native` the PDS is the source of truth and HappyView indexes
//! it, the same way it indexes public data. Sync is pull-based: a `notifyWrite`
//! tells us a repo advanced, and we fetch the ops we have not seen.
//!
//! The cursor records the last rev we have *applied*, so it advances only after
//! the ops are in the index. Advancing it first would drop a window of writes if
//! the sync failed partway.

use crate::AppState;
use crate::error::AppError;
use crate::spaces::host_mode::HostMode;
use crate::spaces::lthash::{self, LtHashState};
use crate::spaces::native_client::OpAction;
use crate::spaces::types::{Space, SpaceRecord};
use crate::spaces::{db, native_client};

/// What a sync attempt did, for logging and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncOutcome {
    /// The repo is not hosted elsewhere, so there is nothing upstream to pull.
    NotNative,
    /// Nothing new since the cursor.
    UpToDate,
    /// Ops were applied and the cursor advanced.
    Applied { ops: usize, rev: String },
    /// The upstream commit disagrees with our index after applying. The index is
    /// left as-is and a full recovery is needed.
    Diverged { expected: String, ours: String },
}

fn space_uri(space: &Space) -> String {
    format!(
        "at://{}/space/{}/{}",
        space.did, space.type_nsid, space.skey
    )
}

fn record_uri(space: &Space, author_did: &str, collection: &str, rkey: &str) -> String {
    format!("{}/{author_did}/{collection}/{rkey}", space_uri(space))
}

/// Pull and apply everything a native repo has done since our cursor.
///
/// Returns [`SyncOutcome::NotNative`] rather than erroring for a polyfill repo:
/// there is no upstream to pull, and treating a stray notification as a sync
/// would overwrite the source of truth with an empty remote.
pub async fn sync_repo(
    state: &AppState,
    space: &Space,
    author_did: &str,
) -> Result<SyncOutcome, AppError> {
    let mut conn = state
        .db
        .acquire()
        .await
        .map_err(|e| AppError::Internal(format!("failed to acquire connection: {e}")))?;
    let repo_state =
        db::get_or_create_repo_state(&mut conn, state.db_backend, &space.id, author_did).await?;
    drop(conn);

    if repo_state.host_mode != HostMode::Native {
        return Ok(SyncOutcome::NotNative);
    }

    let session = crate::repo::get_oauth_session(state, author_did).await?;
    let uri = space_uri(space);
    let ops = native_client::list_repo_ops(
        state,
        &session,
        &uri,
        author_did,
        repo_state.sync_cursor.as_deref(),
    )
    .await?;

    if ops.is_empty() {
        return Ok(SyncOutcome::UpToDate);
    }

    let applied = ops.len();
    let last_rev = ops
        .last()
        .map(|o| o.rev.clone())
        .ok_or_else(|| AppError::Internal("listRepoOps returned ops with no rev".into()))?;

    let mut conn = state
        .db
        .acquire()
        .await
        .map_err(|e| AppError::Internal(format!("failed to acquire connection: {e}")))?;

    for op in native_client::latest_op_per_record(&ops) {
        let uri = record_uri(space, author_did, &op.collection, &op.rkey);
        match op.action {
            OpAction::Delete => {
                db::delete_space_record(&mut *conn, state.db_backend, &uri).await?;
            }
            OpAction::Create | OpAction::Update => {
                let (Some(cid), Some(value)) = (op.cid.as_deref(), op.value.as_ref()) else {
                    // The latest op for a record is never superseded, and we asked
                    // for values, so this is something we cannot index.
                    return Err(AppError::Internal(format!(
                        "listRepoOps returned a {:?} op with no record value for {uri}",
                        op.action
                    )));
                };
                let record = SpaceRecord {
                    uri: uri.clone(),
                    space_id: space.id.clone(),
                    author_did: author_did.to_string(),
                    collection: op.collection.clone(),
                    rkey: op.rkey.clone(),
                    record: value.clone(),
                    cid: cid.to_string(),
                    indexed_at: crate::db::now_rfc3339(),
                };
                db::upsert_space_record(&mut *conn, state.db_backend, &record).await?;
            }
        }
    }
    drop(conn);

    // Check the index against the host's commit. Incremental sync can miss ops
    // if a notification is lost, and the commit hash detects that.
    let commit = native_client::get_latest_commit(state, &session, &uri, author_did).await?;
    let key =
        native_client::author_signing_key(&state.http, &state.config.plc_url, author_did).await?;
    crate::spaces::commit::verify_commit(&commit, &uri, author_did, &key)?;

    let records =
        db::list_all_space_records(&state.db, state.db_backend, &space.id, author_did).await?;
    let mut fold = LtHashState::new();
    for r in &records {
        fold.add(&lthash::record_element(&r.collection, &r.rkey, &r.cid));
    }

    if commit.hash != fold.hash() {
        // Do not advance the cursor. Leaving it where it is means the next sync
        // re-fetches the same window rather than skipping past a gap we have
        // already failed to close.
        return Ok(SyncOutcome::Diverged {
            expected: hex(&commit.hash),
            ours: hex(&fold.hash()),
        });
    }

    let mut conn = state
        .db
        .acquire()
        .await
        .map_err(|e| AppError::Internal(format!("failed to acquire connection: {e}")))?;
    let mut repo_state =
        db::get_or_create_repo_state(&mut conn, state.db_backend, &space.id, author_did).await?;
    repo_state.sync_cursor = Some(last_rev.clone());
    // The index now matches what the host signed, so adopt its commit too.
    repo_state.rev = Some(commit.rev.clone());
    repo_state.hash = Some(commit.hash.to_vec());
    repo_state.ikm = Some(commit.ikm.to_vec());
    repo_state.sig = Some(commit.sig.clone());
    repo_state.mac = Some(commit.mac.to_vec());
    repo_state.lthash_state = fold.as_bytes().to_vec();
    db::update_repo_state(&mut *conn, state.db_backend, &repo_state).await?;

    Ok(SyncOutcome::Applied {
        ops: applied,
        rev: last_rev,
    })
}

/// Record a commit pulled from a repo's host as the space's view of that
/// repo, and tell syncers if it moved the repo forward.
pub async fn adopt_synced_commit(
    state: &AppState,
    space: &Space,
    author_did: &str,
    rev: &str,
    hash: &[u8],
) -> Result<(), AppError> {
    let mut conn = state
        .db
        .acquire()
        .await
        .map_err(|e| AppError::Internal(format!("failed to acquire connection: {e}")))?;
    let recorded = crate::spaces::writers::record(
        &mut conn,
        state.db_backend,
        &space.id,
        author_did,
        rev,
        hash,
    )
    .await?;
    if let crate::spaces::writers::Recorded::Advanced {
        space_rev,
        prev_space_rev,
    } = recorded
    {
        crate::spaces::notifications::forward_repo_update(
            state,
            &space.id,
            crate::spaces::notifications::RepoUpdate {
                space_uri: space_uri(space),
                repo: author_did.to_string(),
                rev: rev.to_string(),
                hash: hash.to_vec(),
                space_rev,
                prev_space_rev,
            },
        );
    }
    Ok(())
}

/// How a sweep went.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SweepSummary {
    pub visited: usize,
    pub failed: usize,
}

/// How often every native repo is pulled regardless of notifications.
pub const SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Pull every native repo, catching writes whose notification never arrived.
///
/// Repo hosts are meant to retry notifications until the space host has them,
/// but hosts that predate that rule deliver once and move on. This is what
/// keeps a dropped notification from leaving a repo stale indefinitely.
pub async fn sweep(state: &AppState) -> Result<SweepSummary, AppError> {
    let sql = crate::db::adapt_sql(
        "SELECT space_id, author_did FROM happyview_space_repo_state WHERE host_mode = 'native'",
        state.db_backend,
    );
    let repos: Vec<(String, String)> = crate::db::query_as(&sql)
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list native repos: {e}")))?;

    let mut summary = SweepSummary::default();
    for (space_id, author_did) in repos {
        summary.visited += 1;
        if let Err(e) = sweep_repo(state, &space_id, &author_did).await {
            summary.failed += 1;
            tracing::warn!(space_id, author_did, error = %e, "sweep could not sync a native repo");
        }
    }
    Ok(summary)
}

async fn sweep_repo(state: &AppState, space_id: &str, author_did: &str) -> Result<(), AppError> {
    let Some(space) = db::get_space(&state.db, state.db_backend, space_id).await? else {
        return Ok(());
    };
    match sync_repo(state, &space, author_did).await? {
        SyncOutcome::Applied { .. } => {
            let mut conn =
                state.db.acquire().await.map_err(|e| {
                    AppError::Internal(format!("failed to acquire connection: {e}"))
                })?;
            let repo_state =
                db::get_or_create_repo_state(&mut conn, state.db_backend, space_id, author_did)
                    .await?;
            drop(conn);
            if let (Some(rev), Some(hash)) = (repo_state.rev, repo_state.hash) {
                adopt_synced_commit(state, &space, author_did, &rev, &hash).await?;
            }
            Ok(())
        }
        SyncOutcome::Diverged { expected, ours } => Err(AppError::Internal(format!(
            "native repo diverged: host has {expected}, index has {ours}"
        ))),
        SyncOutcome::NotNative | SyncOutcome::UpToDate => Ok(()),
    }
}

/// Run [`sweep`] every [`SWEEP_INTERVAL`] while spaces are enabled.
pub async fn run_sweeper(state: AppState) {
    loop {
        tokio::time::sleep(SWEEP_INTERVAL).await;
        if !crate::feature_flags::is_enabled(
            &state.db,
            crate::feature_flags::FeatureFlag::SPACES_ENABLED,
            state.db_backend,
        )
        .await
        {
            continue;
        }
        match sweep(&state).await {
            Ok(summary) if summary.failed > 0 => {
                tracing::warn!(?summary, "native repo sweep finished with failures");
            }
            Ok(summary) => tracing::debug!(?summary, "native repo sweep finished"),
            Err(e) => tracing::warn!(error = %e, "native repo sweep failed"),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_uri_is_the_space_uri_plus_the_record_path() {
        let space = Space {
            id: "s".into(),
            did: "did:plc:auth".into(),
            authority_did: "did:plc:auth".into(),
            creator_did: "did:plc:auth".into(),
            type_nsid: "com.example.forum".into(),
            skey: "main".into(),
            display_name: None,
            description: None,
            read_policy: crate::spaces::types::Policy::MemberList,
            write_policy: crate::spaces::types::Policy::MemberList,
            app_access: crate::spaces::types::AppAccess::Open,
            config: crate::spaces::types::SpaceConfig::default(),
            revision: None,
            created_at: String::new(),
            updated_at: String::new(),
        };

        assert_eq!(
            record_uri(&space, "did:plc:me", "com.example.note", "abc"),
            "at://did:plc:auth/space/com.example.forum/main/did:plc:me/com.example.note/abc"
        );
    }

    fn test_space(id: &str) -> Space {
        Space {
            id: id.into(),
            did: "did:plc:auth".into(),
            authority_did: "did:plc:auth".into(),
            creator_did: "did:plc:auth".into(),
            type_nsid: "com.example.forum".into(),
            skey: id.into(),
            display_name: None,
            description: None,
            read_policy: crate::spaces::types::Policy::MemberList,
            write_policy: crate::spaces::types::Policy::MemberList,
            app_access: crate::spaces::types::AppAccess::Open,
            config: crate::spaces::types::SpaceConfig::default(),
            revision: None,
            created_at: crate::db::now_rfc3339(),
            updated_at: crate::db::now_rfc3339(),
        }
    }

    #[tokio::test]
    async fn an_adopted_commit_joins_the_writer_set() {
        let state = crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        );
        let space = test_space("sp-adopt");
        db::create_space(&state.db, state.db_backend, &space)
            .await
            .unwrap();

        let rev = crate::tid::generate_tid();
        adopt_synced_commit(&state, &space, "did:plc:me", &rev, &[4; 32])
            .await
            .unwrap();

        let writers =
            crate::spaces::writers::list(&state.db, state.db_backend, &space.id, None, None, 10)
                .await
                .unwrap();
        assert_eq!(writers.len(), 1);
        assert_eq!(writers[0].repo_did, "did:plc:me");
        assert_eq!(writers[0].rev, rev);
    }

    #[tokio::test]
    async fn a_sweep_visits_every_native_repo_and_survives_failures() {
        let state = crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        );
        for (space_id, author) in [("sp-a", "did:plc:a"), ("sp-b", "did:plc:b")] {
            db::create_space(&state.db, state.db_backend, &test_space(space_id))
                .await
                .unwrap();
            let mut conn = state.db.acquire().await.unwrap();
            let mut repo_state =
                db::get_or_create_repo_state(&mut conn, state.db_backend, space_id, author)
                    .await
                    .unwrap();
            repo_state.host_mode = HostMode::Native;
            db::update_repo_state(&mut *conn, state.db_backend, &repo_state)
                .await
                .unwrap();
        }
        db::create_space(&state.db, state.db_backend, &test_space("sp-poly"))
            .await
            .unwrap();
        let mut conn = state.db.acquire().await.unwrap();
        db::get_or_create_repo_state(&mut conn, state.db_backend, "sp-poly", "did:plc:p")
            .await
            .unwrap();
        drop(conn);

        // Neither native repo has an OAuth session, so each sync fails, and the
        // failure of the first must not stop the second being tried.
        let summary = sweep(&state).await.unwrap();
        assert_eq!(summary.visited, 2);
        assert_eq!(summary.failed, 2);
    }

    #[tokio::test]
    async fn a_polyfill_repo_is_never_synced_from_upstream() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let state = crate::test_support::test_state_with_pool(pool);

        let space = Space {
            id: "sp-poly".into(),
            did: "did:plc:auth".into(),
            authority_did: "did:plc:auth".into(),
            creator_did: "did:plc:auth".into(),
            type_nsid: "com.example.forum".into(),
            skey: "main".into(),
            display_name: None,
            description: None,
            read_policy: crate::spaces::types::Policy::MemberList,
            write_policy: crate::spaces::types::Policy::MemberList,
            app_access: crate::spaces::types::AppAccess::Open,
            config: crate::spaces::types::SpaceConfig::default(),
            revision: None,
            created_at: crate::db::now_rfc3339(),
            updated_at: crate::db::now_rfc3339(),
        };
        db::create_space(&state.db, state.db_backend, &space)
            .await
            .expect("seed space");

        // No OAuth session exists, so this would error if it reached the
        // network.
        let outcome = sync_repo(&state, &space, "did:plc:me")
            .await
            .expect("no error");
        assert_eq!(outcome, SyncOutcome::NotNative);
    }
}
