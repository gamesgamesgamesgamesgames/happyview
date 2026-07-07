use std::collections::{HashMap, HashSet};

use crate::db::DatabaseBackend;
use crate::error::AppError;
use crate::spaces::SpaceUri;
use crate::spaces::db;
use crate::spaces::types::{ResolvedMember, SpaceAccess, SpaceMember};

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
    let mut resolved: HashMap<String, SpaceAccess> = HashMap::new();
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
) -> Result<Option<SpaceAccess>, AppError> {
    let members = resolve_members(pool, backend, space_id).await?;
    Ok(members.into_iter().find(|m| m.did == did).map(|m| m.access))
}

fn resolve_members_recursive<'a>(
    pool: &'a sqlx::AnyPool,
    backend: DatabaseBackend,
    space_id: &'a str,
    resolved: &'a mut HashMap<String, SpaceAccess>,
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
    if member.did.starts_with("at://") {
        let uri = SpaceUri::parse(&member.did)?;
        let space =
            db::get_space_by_address(pool, backend, &uri.did, &uri.type_nsid, &uri.skey).await?;
        Ok(space.map(|s| s.id))
    } else {
        let space = db::get_space(pool, backend, &member.did).await?;
        Ok(space.map(|s| s.id))
    }
}

fn merge_access(resolved: &mut HashMap<String, SpaceAccess>, did: &str, access: SpaceAccess) {
    let entry = resolved.entry(did.to_string()).or_insert(SpaceAccess::Read);
    if access.can_write() {
        *entry = SpaceAccess::Write;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_access_write_wins() {
        let mut map = HashMap::new();
        merge_access(&mut map, "did:plc:user1", SpaceAccess::Read);
        assert_eq!(map["did:plc:user1"], SpaceAccess::Read);

        merge_access(&mut map, "did:plc:user1", SpaceAccess::Write);
        assert_eq!(map["did:plc:user1"], SpaceAccess::Write);

        // Write should not be downgraded to Read
        merge_access(&mut map, "did:plc:user1", SpaceAccess::Read);
        assert_eq!(map["did:plc:user1"], SpaceAccess::Write);
    }

    #[test]
    fn merge_access_multiple_users() {
        let mut map = HashMap::new();
        merge_access(&mut map, "did:plc:alice", SpaceAccess::Write);
        merge_access(&mut map, "did:plc:bob", SpaceAccess::Read);
        assert_eq!(map.len(), 2);
        assert_eq!(map["did:plc:alice"], SpaceAccess::Write);
        assert_eq!(map["did:plc:bob"], SpaceAccess::Read);
    }
}
