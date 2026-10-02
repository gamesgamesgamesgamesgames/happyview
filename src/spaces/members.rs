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
    let mut resolved: HashMap<String, MemberAccess> = HashMap::new();
    let mut visited: HashSet<String> = HashSet::new();

    resolve_members_recursive(pool, backend, space_id, &mut resolved, &mut visited, 0).await?;

    let mut members: Vec<ResolvedMember> = resolved
        .into_iter()
        .map(|(did, access)| ResolvedMember { did, access })
        .collect();
    members.sort_by(|a, b| a.did.cmp(&b.did));
    Ok(members)
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

fn resolve_members_recursive<'a>(
    pool: &'a sqlx::AnyPool,
    backend: DatabaseBackend,
    space_id: &'a str,
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

        let direct_members = db::list_direct_members(pool, backend, space_id).await?;

        for member in direct_members {
            if member.is_delegation {
                let delegated_space_id = resolve_delegation_target(pool, backend, &member).await?;
                if let Some(target_id) = delegated_space_id {
                    resolve_members_recursive(
                        pool,
                        backend,
                        &target_id,
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
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    member: &SpaceMember,
) -> Result<Option<String>, AppError> {
    if member.did.starts_with("at://") || member.did.starts_with("ats://") {
        let uri = SpaceUri::parse(&member.did)?;
        let space =
            db::get_space_by_address(pool, backend, &uri.did, &uri.type_nsid, &uri.skey).await?;
        Ok(space.map(|s| s.id))
    } else {
        let space = db::get_space(pool, backend, &member.did).await?;
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
