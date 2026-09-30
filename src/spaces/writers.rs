//! The space host's record of which repos have written to a space, and the
//! space-wide revision that orders their updates.
//!
//! Each accepted update is stamped with a space revision, a TID strictly greater
//! than the one before it. A syncer that sees a gap between the previous and
//! current revision of two notifications knows it missed one and can ask
//! `listRepos` for everything since the last revision it saw.

use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::lua::tid;

/// How far into the future a repo revision may point before it is refused.
pub const MAX_REV_CLOCK_SKEW_MICROS: i64 = 5 * 60 * 1_000_000;

/// The outcome of reporting a repo's new state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    /// The repo moved forward and the space revision advanced with it.
    Advanced {
        space_rev: String,
        prev_space_rev: Option<String>,
    },
    /// The revision was not newer than the one already held, so nothing changed.
    Stale,
}

/// Record a repo's new `rev` and `hash`, advancing the space revision.
///
/// Runs on the caller's connection so a local write records its writer in the
/// same transaction as the commit.
pub async fn record(
    conn: &mut sqlx::AnyConnection,
    backend: DatabaseBackend,
    space_id: &str,
    repo: &str,
    rev: &str,
    hash: &[u8],
) -> Result<Recorded, AppError> {
    let rev_micros = tid::tid_to_unix_microseconds(rev)
        .ok_or_else(|| AppError::BadRequest("rev is not a TID".into()))?;
    if rev_micros > chrono::Utc::now().timestamp_micros() + MAX_REV_CLOCK_SKEW_MICROS {
        return Err(AppError::XrpcError {
            status: axum::http::StatusCode::BAD_REQUEST,
            code: "FutureRev",
            message: "the repo revision exceeds the permitted clock skew".into(),
        });
    }

    // Postgres runs concurrent writers in parallel, so hold the space row to
    // hand out space revisions one at a time. SQLite already serializes writers.
    if backend == DatabaseBackend::Postgres {
        crate::db::query("SELECT id FROM happyview_spaces WHERE id = $1 FOR UPDATE")
            .bind(space_id)
            .execute(&mut *conn)
            .await
            .map_err(|e| AppError::Internal(format!("failed to lock space: {e}")))?;
    }

    let held: Option<(String,)> = crate::db::query_as(&adapt_sql(
        "SELECT rev FROM happyview_space_writers WHERE space_id = ? AND repo_did = ?",
        backend,
    ))
    .bind(space_id)
    .bind(repo)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| AppError::Internal(format!("failed to read writer state: {e}")))?;
    // TIDs sort as strings in time order.
    if held.is_some_and(|(held,)| rev <= held.as_str()) {
        return Ok(Recorded::Stale);
    }

    let (prev_space_rev,): (Option<String>,) = crate::db::query_as(&adapt_sql(
        "SELECT MAX(space_rev) FROM happyview_space_writers WHERE space_id = ?",
        backend,
    ))
    .bind(space_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| AppError::Internal(format!("failed to read space revision: {e}")))?;
    let space_rev = next_space_rev(prev_space_rev.as_deref());

    crate::db::query(&adapt_sql(
        "INSERT INTO happyview_space_writers (space_id, repo_did, rev, hash, space_rev, updated_at) VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT (space_id, repo_did) DO UPDATE SET rev = excluded.rev, hash = excluded.hash, space_rev = excluded.space_rev, updated_at = excluded.updated_at",
        backend,
    ))
    .bind(space_id)
    .bind(repo)
    .bind(rev)
    .bind(hash.to_vec())
    .bind(&space_rev)
    .bind(now_rfc3339())
    .execute(&mut *conn)
    .await
    .map_err(|e| AppError::Internal(format!("failed to record writer: {e}")))?;

    Ok(Recorded::Advanced {
        space_rev,
        prev_space_rev,
    })
}

/// A repo in a space's writer set.
#[derive(Debug, Clone)]
pub struct Writer {
    pub repo_did: String,
    pub rev: String,
    pub hash: Vec<u8>,
    pub space_rev: String,
}

/// One page of a space's writers.
///
/// Without `since` the whole set is listed by DID, and `cursor` is the last DID
/// seen. With `since` only repos updated after that space revision are listed,
/// in the order they were updated, and `cursor` is the last space revision seen.
pub async fn list(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    space_id: &str,
    since: Option<&str>,
    cursor: Option<&str>,
    limit: i64,
) -> Result<Vec<Writer>, AppError> {
    let (sql, after) = match since {
        Some(since) => (
            "SELECT repo_did, rev, hash, space_rev FROM happyview_space_writers WHERE space_id = ? AND space_rev > ? ORDER BY space_rev ASC LIMIT ?",
            Some(cursor.unwrap_or(since)),
        ),
        None if cursor.is_some() => (
            "SELECT repo_did, rev, hash, space_rev FROM happyview_space_writers WHERE space_id = ? AND repo_did > ? ORDER BY repo_did ASC LIMIT ?",
            cursor,
        ),
        None => (
            "SELECT repo_did, rev, hash, space_rev FROM happyview_space_writers WHERE space_id = ? ORDER BY repo_did ASC LIMIT ?",
            None,
        ),
    };
    let sql = adapt_sql(sql, backend);
    let mut query = crate::db::query_as::<(String, String, Vec<u8>, String)>(&sql).bind(space_id);
    if let Some(after) = after {
        query = query.bind(after);
    }
    let rows = query
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list space writers: {e}")))?;

    Ok(rows
        .into_iter()
        .map(|(repo_did, rev, hash, space_rev)| Writer {
            repo_did,
            rev,
            hash,
            space_rev,
        })
        .collect())
}

/// The latest space revision, or `None` if nothing has been written.
pub async fn current_space_rev(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    space_id: &str,
) -> Result<Option<String>, AppError> {
    let (rev,): (Option<String>,) = crate::db::query_as(&adapt_sql(
        "SELECT MAX(space_rev) FROM happyview_space_writers WHERE space_id = ?",
        backend,
    ))
    .bind(space_id)
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::Internal(format!("failed to read space revision: {e}")))?;
    Ok(rev)
}

/// Replace a writer's hash without moving the space revision, for a commit
/// re-minted at the same `rev`. Nothing new was written, so nothing is sequenced.
pub async fn replace_hash(
    conn: &mut sqlx::AnyConnection,
    backend: DatabaseBackend,
    space_id: &str,
    repo: &str,
    rev: &str,
    hash: &[u8],
) -> Result<(), AppError> {
    crate::db::query(&adapt_sql(
        "UPDATE happyview_space_writers SET hash = ?, updated_at = ? WHERE space_id = ? AND repo_did = ? AND rev = ?",
        backend,
    ))
    .bind(hash.to_vec())
    .bind(now_rfc3339())
    .bind(space_id)
    .bind(repo)
    .bind(rev)
    .execute(&mut *conn)
    .await
    .map_err(|e| AppError::Internal(format!("failed to update writer hash: {e}")))?;
    Ok(())
}

/// The current time as a TID, or one past `prev` if the clock has not moved on.
fn next_space_rev(prev: Option<&str>) -> String {
    let now = tid::generate_tid();
    match prev.and_then(tid::tid_to_number) {
        Some(prev) if tid::tid_to_number(&now).is_some_and(|now| now <= prev) => {
            tid::tid_from_number(prev + 1)
        }
        _ => now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spaces::types::{AppAccess, Policy, Space, SpaceConfig};

    async fn space_db() -> (sqlx::AnyPool, String) {
        let pool = crate::test_support::migrated_memory_pool().await;
        let space = Space {
            id: uuid::Uuid::new_v4().to_string(),
            did: "did:plc:authority".into(),
            authority_did: "did:plc:authority".into(),
            creator_did: "did:plc:authority".into(),
            type_nsid: "com.example.forum".into(),
            skey: "main".into(),
            display_name: None,
            description: None,
            read_policy: Policy::MemberList,
            write_policy: Policy::MemberList,
            app_access: AppAccess::Open,
            config: SpaceConfig::default(),
            revision: None,
            created_at: now_rfc3339(),
            updated_at: now_rfc3339(),
        };
        crate::spaces::db::create_space(&pool, DatabaseBackend::Sqlite, &space)
            .await
            .unwrap();
        (pool, space.id)
    }

    async fn record_on(
        pool: &sqlx::AnyPool,
        space_id: &str,
        repo: &str,
        rev: &str,
    ) -> Result<Recorded, AppError> {
        let mut conn = pool.acquire().await.unwrap();
        record(
            &mut conn,
            DatabaseBackend::Sqlite,
            space_id,
            repo,
            rev,
            &[1, 2, 3],
        )
        .await
    }

    fn advanced(recorded: Recorded) -> (String, Option<String>) {
        match recorded {
            Recorded::Advanced {
                space_rev,
                prev_space_rev,
            } => (space_rev, prev_space_rev),
            Recorded::Stale => panic!("expected the space revision to advance"),
        }
    }

    #[tokio::test]
    async fn the_first_update_has_no_previous_space_revision() {
        let (pool, space_id) = space_db().await;
        let (_, prev) = advanced(
            record_on(&pool, &space_id, "did:plc:alice", &tid::generate_tid())
                .await
                .unwrap(),
        );
        assert_eq!(prev, None);
    }

    #[tokio::test]
    async fn each_update_names_the_revision_before_it() {
        let (pool, space_id) = space_db().await;
        let (first, _) = advanced(
            record_on(&pool, &space_id, "did:plc:alice", &tid::generate_tid())
                .await
                .unwrap(),
        );
        let (second, prev) = advanced(
            record_on(&pool, &space_id, "did:plc:bob", &tid::generate_tid())
                .await
                .unwrap(),
        );
        assert_eq!(prev.as_deref(), Some(first.as_str()));
        assert!(second > first, "{second} should sort after {first}");
    }

    #[tokio::test]
    async fn space_revisions_increase_even_when_the_clock_does_not() {
        let (pool, space_id) = space_db().await;
        let mut last = String::new();
        for i in 0..20 {
            let (space_rev, _) = advanced(
                record_on(
                    &pool,
                    &space_id,
                    &format!("did:plc:w{i}"),
                    &tid::generate_tid(),
                )
                .await
                .unwrap(),
            );
            assert!(space_rev > last, "{space_rev} should sort after {last}");
            last = space_rev;
        }
    }

    #[tokio::test]
    async fn an_old_or_repeated_rev_is_ignored() {
        let (pool, space_id) = space_db().await;
        let rev = tid::generate_tid();
        record_on(&pool, &space_id, "did:plc:alice", &rev)
            .await
            .unwrap();
        let older =
            tid::tid_from_unix_microseconds(tid::tid_to_unix_microseconds(&rev).unwrap() - 1);
        assert_eq!(
            record_on(&pool, &space_id, "did:plc:alice", &rev)
                .await
                .unwrap(),
            Recorded::Stale
        );
        assert_eq!(
            record_on(&pool, &space_id, "did:plc:alice", &older)
                .await
                .unwrap(),
            Recorded::Stale
        );
    }

    #[tokio::test]
    async fn a_re_minted_commit_replaces_the_hash_without_sequencing() {
        let (pool, space_id) = space_db().await;
        let rev = tid::generate_tid();
        let (space_rev, _) = advanced(
            record_on(&pool, &space_id, "did:plc:alice", &rev)
                .await
                .unwrap(),
        );
        let mut conn = pool.acquire().await.unwrap();
        replace_hash(
            &mut conn,
            DatabaseBackend::Sqlite,
            &space_id,
            "did:plc:alice",
            &rev,
            &[9, 9],
        )
        .await
        .unwrap();

        let (hash, held_space_rev): (Vec<u8>, String) = crate::db::query_as(
            "SELECT hash, space_rev FROM happyview_space_writers WHERE space_id = ? AND repo_did = ?",
        )
        .bind(&space_id)
        .bind("did:plc:alice")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(hash, vec![9, 9]);
        assert_eq!(held_space_rev, space_rev);
    }

    #[tokio::test]
    async fn a_rev_from_the_future_is_refused() {
        let (pool, space_id) = space_db().await;
        let now = tid::tid_to_unix_microseconds(&tid::generate_tid()).unwrap();
        let future = tid::tid_from_unix_microseconds(now + 2 * MAX_REV_CLOCK_SKEW_MICROS);
        let err = record_on(&pool, &space_id, "did:plc:alice", &future)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            AppError::XrpcError {
                code: "FutureRev",
                ..
            }
        ));
    }
}
