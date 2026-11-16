use std::collections::{HashMap, HashSet};

use crate::db::DatabaseBackend;
use crate::error::AppError;
use crate::spaces::SpaceUri;
use crate::spaces::db;
use crate::spaces::types::{MemberAccess, ResolvedMember, SpaceMember};

const MAX_DELEGATION_DEPTH: usize = 10;

/// Resolve the full member list for a space, traversing delegation references.
///
/// When a space delegates to another space (is_delegation=true), the delegated
/// space's members are included in the result. If both a direct membership and
/// a delegated membership exist for the same DID, the higher access level wins
/// (write > read).
pub async fn resolve_members(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    space_id: &str,
) -> Result<Vec<ResolvedMember>, AppError> {
    let mut conn = pool
        .acquire()
        .await
        .map_err(|e| AppError::Internal(format!("failed to resolve members: {e}")))?;
    resolve_on(&mut conn, backend, space_id, false).await
}

/// Check if a DID is a member of a space (resolving delegations).
pub async fn is_member(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    space_id: &str,
    did: &str,
) -> Result<Option<MemberAccess>, AppError> {
    let members = resolve_members(pool, backend, space_id).await?;
    Ok(members.into_iter().find(|m| m.did == did).map(|m| m.access))
}

/// [`is_member`], read in the caller's write transaction, holding every space
/// it reads members from until that transaction ends.
///
/// Member changes lock their space's row first (`db::lock_space_for_members`).
/// On Postgres each space visited here is share-locked before its members are
/// read, so a change to any of them either commits before this read or waits
/// for the caller's transaction. On SQLite the caller's `BEGIN IMMEDIATE`
/// already orders it with every other writer.
pub async fn is_member_locked(
    conn: &mut sqlx::AnyConnection,
    backend: DatabaseBackend,
    space_id: &str,
    did: &str,
) -> Result<Option<MemberAccess>, AppError> {
    let members = resolve_on(conn, backend, space_id, true).await?;
    Ok(members.into_iter().find(|m| m.did == did).map(|m| m.access))
}

/// [`resolve_members`], read on the caller's connection, so inside its
/// transaction.
pub async fn resolve_members_on(
    conn: &mut sqlx::AnyConnection,
    backend: DatabaseBackend,
    space_id: &str,
) -> Result<Vec<ResolvedMember>, AppError> {
    resolve_on(conn, backend, space_id, false).await
}

/// The spaces that take `space_id`'s members through delegation entries,
/// directly or through other spaces, up to the depth the resolver follows.
/// Each is listed once, in id order; `space_id` itself is not listed.
///
/// This is the reverse of [`resolve_members`]: a change to `space_id`'s
/// members can change who reads each of these.
pub async fn delegating_spaces(
    conn: &mut sqlx::AnyConnection,
    backend: DatabaseBackend,
    space_id: &str,
) -> Result<Vec<String>, AppError> {
    let mut visited: HashSet<String> = HashSet::from([space_id.to_string()]);
    let mut found: Vec<String> = Vec::new();
    let mut frontier = vec![space_id.to_string()];
    for _ in 1..MAX_DELEGATION_DEPTH {
        let mut next = Vec::new();
        for child_id in &frontier {
            let Some(child) = db::get_space(&mut *conn, backend, child_id).await? else {
                continue;
            };
            for entry in db::delegation_entries_naming(&mut *conn, backend, &child).await? {
                if visited.contains(&entry.space_id) {
                    continue;
                }
                let target = resolve_delegation_target(conn, backend, &entry).await;
                if target.ok().flatten().as_deref() == Some(child_id.as_str()) {
                    visited.insert(entry.space_id.clone());
                    found.push(entry.space_id.clone());
                    next.push(entry.space_id);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    found.sort();
    Ok(found)
}

async fn resolve_on(
    conn: &mut sqlx::AnyConnection,
    backend: DatabaseBackend,
    space_id: &str,
    lock: bool,
) -> Result<Vec<ResolvedMember>, AppError> {
    let mut resolved: HashMap<String, MemberAccess> = HashMap::new();
    let mut visited: HashSet<String> = HashSet::new();

    resolve_members_recursive(
        conn,
        backend,
        space_id,
        lock,
        &mut resolved,
        &mut visited,
        0,
    )
    .await?;

    let mut members: Vec<ResolvedMember> = resolved
        .into_iter()
        .map(|(did, access)| ResolvedMember { did, access })
        .collect();
    members.sort_by(|a, b| a.did.cmp(&b.did));
    Ok(members)
}

fn resolve_members_recursive<'a>(
    conn: &'a mut sqlx::AnyConnection,
    backend: DatabaseBackend,
    space_id: &'a str,
    lock: bool,
    resolved: &'a mut HashMap<String, MemberAccess>,
    visited: &'a mut HashSet<String>,
    depth: usize,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), AppError>> + Send + 'a>> {
    Box::pin(async move {
        if depth >= MAX_DELEGATION_DEPTH {
            return Ok(());
        }

        if !visited.insert(space_id.to_string()) {
            return Ok(());
        }

        if lock && backend == DatabaseBackend::Postgres {
            crate::db::query("SELECT id FROM happyview_spaces WHERE id = $1 FOR SHARE")
                .bind(space_id)
                .execute(&mut *conn)
                .await
                .map_err(|e| AppError::Internal(format!("failed to lock space: {e}")))?;
        }

        let direct_members = db::list_direct_members(&mut *conn, backend, space_id).await?;

        for member in direct_members {
            if member.is_delegation {
                let delegated_space_id = resolve_delegation_target(conn, backend, &member).await?;
                if let Some(target_id) = delegated_space_id {
                    resolve_members_recursive(
                        conn,
                        backend,
                        &target_id,
                        lock,
                        resolved,
                        visited,
                        depth + 1,
                    )
                    .await?;
                }
            } else {
                merge_access(resolved, &member.did, member.access);
            }
        }

        Ok(())
    })
}

/// Resolve a delegation member entry to the target space ID.
///
/// Delegation entries store either an at:// URI or a space ID directly.
async fn resolve_delegation_target(
    conn: &mut sqlx::AnyConnection,
    backend: DatabaseBackend,
    member: &SpaceMember,
) -> Result<Option<String>, AppError> {
    if member.did.starts_with("at://") || member.did.starts_with("ats://") {
        let uri = SpaceUri::parse(&member.did)?;
        let space =
            db::get_space_by_address(&mut *conn, backend, &uri.did, &uri.type_nsid, &uri.skey)
                .await?;
        Ok(space.map(|s| s.id))
    } else {
        let space = db::get_space(&mut *conn, backend, &member.did).await?;
        Ok(space.map(|s| s.id))
    }
}

fn merge_access(resolved: &mut HashMap<String, MemberAccess>, did: &str, access: MemberAccess) {
    // Seed with the member's own access rather than a plain read, so a
    // `read_self` member is not promoted to whole-space reads. Later paths merge
    // in with `MemberAccess::union`.
    resolved
        .entry(did.to_string())
        .and_modify(|entry| *entry = entry.union(access))
        .or_insert(access);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_access_write_wins() {
        let mut map = HashMap::new();
        merge_access(&mut map, "did:plc:user1", MemberAccess::READ);
        assert_eq!(map["did:plc:user1"], MemberAccess::READ);

        merge_access(&mut map, "did:plc:user1", MemberAccess::WRITE);
        assert_eq!(map["did:plc:user1"], MemberAccess::WRITE);

        // Write should not be downgraded to Read
        merge_access(&mut map, "did:plc:user1", MemberAccess::READ);
        assert_eq!(map["did:plc:user1"], MemberAccess::WRITE);
    }

    #[test]
    fn merge_access_preserves_read_self() {
        // A read_self member must NOT be silently promoted to full read.
        let mut map = HashMap::new();
        merge_access(&mut map, "did:plc:user", MemberAccess::READ_SELF);
        assert_eq!(map["did:plc:user"], MemberAccess::READ_SELF);
    }

    #[test]
    fn merge_access_upgrades_but_never_downgrades() {
        // read_self upgraded by a higher grant on another path.
        let mut map = HashMap::new();
        merge_access(&mut map, "u", MemberAccess::READ_SELF);
        merge_access(&mut map, "u", MemberAccess::READ);
        assert_eq!(map["u"], MemberAccess::READ);
        merge_access(&mut map, "u", MemberAccess::WRITE);
        assert_eq!(map["u"], MemberAccess::WRITE);

        // A lower grant on another path never downgrades.
        let mut map2 = HashMap::new();
        merge_access(&mut map2, "v", MemberAccess::READ);
        merge_access(&mut map2, "v", MemberAccess::READ_SELF);
        assert_eq!(map2["v"], MemberAccess::READ);

        merge_access(&mut map2, "v", MemberAccess::WRITE);
        merge_access(&mut map2, "v", MemberAccess::READ_SELF);
        assert_eq!(map2["v"], MemberAccess::WRITE);
    }

    #[test]
    fn merge_access_multiple_users() {
        let mut map = HashMap::new();
        merge_access(&mut map, "did:plc:alice", MemberAccess::WRITE);
        merge_access(&mut map, "did:plc:bob", MemberAccess::READ);
        assert_eq!(map.len(), 2);
        assert_eq!(map["did:plc:alice"], MemberAccess::WRITE);
        assert_eq!(map["did:plc:bob"], MemberAccess::READ);
    }
}
