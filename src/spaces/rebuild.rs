//! Rebuild derived repo state from records.
//!
//! A commit is derived data: `lthash_state` is a fold over `happyview_space_records`,
//! `hash` is `sha256(lthash_state)`, and `ikm` is fresh per commit. A change to
//! how a commit is computed therefore needs no dual-support path: the state is
//! recomputed from the records, which this module does not modify.
//!
//! The two callers differ only in what happens to `rev`:
//!
//! - [`cid_backfill`](super::cid_backfill) repairs record CIDs. The record set
//!   changed, so the repo advances to a new revision.
//! - [`run_commit_format_rebuild`] re-mints commits after a change to the commit
//!   format. The records are identical, so the revision is preserved; advancing
//!   it would tell every syncer the repo changed when it did not.

use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::lua::tid::generate_tid;
use crate::spaces::lthash::{LtHashState, record_element};
use crate::spaces::{commit, db};

/// Marker so the one-time commit-format rebuild does not repeat on every boot.
const COMMIT_FORMAT_MARKER_KEY: &str = "space_commit_format_rebuild_completed_at";

/// What happens to a repo's revision when its state is rebuilt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevPolicy {
    /// Keep the stored revision. For rebuilds where the record set is unchanged
    /// and only the commit computation differs.
    Preserve,
    /// Mint a new TID. For rebuilds where the records themselves changed.
    Advance,
}

/// Recompute one repo's set hash from its records and re-mint its commit.
///
/// Returns `false` when there was nothing to rebuild: a repo with no commit
/// under [`RevPolicy::Preserve`] has no revision to keep, and its state is
/// already the empty fold.
pub async fn rebuild_repo_state(
    tx: &mut sqlx::AnyConnection,
    backend: DatabaseBackend,
    space_id: &str,
    author_did: &str,
    signing_key: &p256::ecdsa::SigningKey,
    rev_policy: RevPolicy,
) -> Result<bool, AppError> {
    let Some(space) = db::get_space(&mut *tx, backend, space_id).await? else {
        tracing::warn!(space_id, "repo state references a missing space; skipping");
        return Ok(false);
    };

    let sql = adapt_sql(
        "SELECT collection, rkey, cid FROM happyview_space_records WHERE space_id = ? AND author_did = ?",
        backend,
    );
    let records: Vec<(String, String, String)> = crate::db::query_as(&sql)
        .bind(space_id)
        .bind(author_did)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(format!("failed to load records for rebuild: {e}")))?;

    let mut set_hash = LtHashState::new();
    for (collection, rkey, cid) in &records {
        set_hash.add(&record_element(collection, rkey, cid));
    }

    let mut repo_state =
        db::get_or_create_repo_state(&mut *tx, backend, space_id, author_did).await?;

    let rev = match rev_policy {
        RevPolicy::Advance => generate_tid(),
        RevPolicy::Preserve => match repo_state.rev.clone() {
            Some(rev) => rev,
            // Never committed, so there is no commit to re-mint.
            None => return Ok(false),
        },
    };

    let space_uri = format!(
        "at://{}/space/{}/{}",
        space.did, space.type_nsid, space.skey
    );
    let signed = commit::sign_commit(&set_hash.hash(), &space_uri, author_did, &rev, signing_key)?;

    repo_state.lthash_state = set_hash.as_bytes().to_vec();
    repo_state.rev = Some(signed.rev);
    repo_state.hash = Some(signed.hash.to_vec());
    repo_state.ikm = Some(signed.ikm.to_vec());
    repo_state.sig = Some(signed.sig);
    repo_state.mac = Some(signed.mac.to_vec());
    db::update_repo_state(&mut *tx, backend, &repo_state).await?;

    if rev_policy == RevPolicy::Advance {
        db::update_space_revision(&mut *tx, backend, space_id, &rev).await?;
    }

    Ok(true)
}

/// Re-mint every existing commit once, after a change to the commit format.
///
/// Idempotent across restarts via an instance-settings marker. Returns the
/// number of repos rebuilt, or `None` if the rebuild had already completed.
pub async fn run_commit_format_rebuild(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    signing_key: &p256::ecdsa::SigningKey,
) -> Result<Option<usize>, AppError> {
    let read_sql = adapt_sql(
        "SELECT value FROM happyview_instance_settings WHERE key = ?",
        backend,
    );
    let existing: Option<(String,)> = crate::db::query_as(&read_sql)
        .bind(COMMIT_FORMAT_MARKER_KEY)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to read rebuild marker: {e}")))?;
    if existing.is_some() {
        return Ok(None);
    }

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| AppError::Internal(format!("failed to begin transaction: {e}")))?;

    let list_sql = adapt_sql(
        "SELECT space_id, author_did FROM happyview_space_repo_state WHERE hash IS NOT NULL",
        backend,
    );
    let repos: Vec<(String, String)> = crate::db::query_as(&list_sql)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(format!("failed to load repo states: {e}")))?;

    let mut rebuilt = 0usize;
    for (space_id, author_did) in repos {
        if rebuild_repo_state(
            &mut tx,
            backend,
            &space_id,
            &author_did,
            signing_key,
            RevPolicy::Preserve,
        )
        .await?
        {
            rebuilt += 1;
        }
    }

    let now = now_rfc3339();
    let marker_sql = adapt_sql(
        "INSERT INTO happyview_instance_settings (key, value) VALUES (?, ?)",
        backend,
    );
    crate::db::query(&marker_sql)
        .bind(COMMIT_FORMAT_MARKER_KEY)
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(format!("failed to write rebuild marker: {e}")))?;

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("failed to commit rebuild: {e}")))?;

    Ok(Some(rebuilt))
}
