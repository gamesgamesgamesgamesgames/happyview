//! A space's effective member list.
//!
//! A memberspace's members are its direct members, plus the members of every
//! space it nests (`happyview_space_nested`), resolved when access is checked
//! rather than copied into the member table (HAP-388 §11). A nested space's
//! member gets at most the access the reference grants. Spaces named as
//! members the simplespace way (`is_delegation` rows) still resolve as they
//! always did, granting their members' access unchanged.
//!
//! Resolution is bounded: at most [`MAX_DELEGATION_DEPTH`] levels, each space
//! visited once, and at most [`MAX_RESOLVED`] rows read in all. Past that the
//! resolution fails with `MemberListTooLarge` rather than answer with part of
//! the list.

use std::collections::{BTreeMap, HashSet};

use crate::db::{DatabaseBackend, adapt_sql};
use crate::error::AppError;
use crate::spaces::SpaceUri;
use crate::spaces::db;
use crate::spaces::types::{MemberAccess, ResolvedMember};

const MAX_DELEGATION_DEPTH: usize = 10;

/// The most member rows and references one resolution reads.
pub const MAX_RESOLVED: usize = 10_000;

/// A member of the effective list, and where its access came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effective {
    pub access: MemberAccess,
    /// Whether the space lists the DID itself.
    pub direct: bool,
    /// The first nested space the DID was found through, if any.
    pub via: Option<String>,
}

/// The effective member list, by DID.
pub type Resolution = BTreeMap<String, Effective>;

fn too_large() -> AppError {
    AppError::XrpcError {
        status: axum::http::StatusCode::BAD_REQUEST,
        code: "MemberListTooLarge",
        message: format!(
            "this space's members, with the spaces it nests, come to more than {MAX_RESOLVED} \
             entries"
        ),
    }
}

fn sql_error(e: sqlx::Error) -> AppError {
    AppError::Internal(format!("failed to resolve members: {e}"))
}

type Row = (String, i32, i32, i32, i32);

/// The read and write a path of references allows; `None` before any.
type Cap = Option<(bool, bool)>;

/// A space to visit: its id, the cap of the path to it, the first nested
/// space on that path, and its depth.
type Visit = (String, Cap, Option<String>, usize);

/// Resolve a space's effective member list on `conn`, which may be inside a
/// write transaction.
pub async fn resolve_on(
    conn: &mut sqlx::AnyConnection,
    backend: DatabaseBackend,
    space_id: &str,
) -> Result<Resolution, AppError> {
    let mut resolved = Resolution::new();
    // A space is visited once per access cap it is reached with, so a
    // restrictive path seen first doesn't hide a more permissive one. There
    // are four caps, so each space is read at most four times (and once when
    // nothing restricts it).
    let mut visited: std::collections::HashMap<String, Vec<Cap>> = std::collections::HashMap::new();
    let mut budget = MAX_RESOLVED;
    // (space id, access cap from the references so far, the first nested
    // space on the path, depth). The root has no cap and no source.
    let mut queue: Vec<Visit> = vec![(space_id.to_string(), None, None, 0)];
    while let Some((id, cap, via, depth)) = queue.pop() {
        if depth >= MAX_DELEGATION_DEPTH {
            continue;
        }
        let seen = visited.entry(id.clone()).or_default();
        if seen.iter().any(|earlier| covers(*earlier, cap)) {
            continue;
        }
        seen.push(cap);
        let rows: Vec<Row> = crate::db::query_as(&adapt_sql(
            "SELECT member_did, can_read, can_write, read_self, is_delegation FROM happyview_space_members WHERE space_id = ?",
            backend,
        ))
        .bind(&id)
        .fetch_all(&mut *conn)
        .await
        .map_err(sql_error)?;
        let refs: Vec<(String, i32, i32)> = crate::db::query_as(&adapt_sql(
            "SELECT target_space_uri, can_read, can_write FROM happyview_space_nested WHERE space_id = ?",
            backend,
        ))
        .bind(&id)
        .fetch_all(&mut *conn)
        .await
        .map_err(sql_error)?;
        budget = budget
            .checked_sub(rows.len() + refs.len())
            .ok_or_else(too_large)?;

        for (did, read, write, read_self, is_delegation) in rows {
            if is_delegation != 0 {
                // Simplespace's space-as-member: its members keep their own
                // access, as they always did.
                if let Some(target) = target_id(conn, backend, &did).await? {
                    queue.push((target, cap, via.clone().or(Some(did)), depth + 1));
                }
                continue;
            }
            let own = MemberAccess {
                read: read != 0,
                write: write != 0,
                read_self: read_self != 0,
            };
            let access = match cap {
                Some((r, w)) => MemberAccess {
                    read: own.read && r,
                    write: own.write && w,
                    read_self: own.read_self,
                },
                None => own,
            };
            let entry = resolved.entry(did).or_insert(Effective {
                access,
                direct: false,
                via: via.clone(),
            });
            if depth == 0 {
                entry.direct = true;
            }
            entry.access = entry.access.union(access);
        }
        for (uri, read, write) in refs {
            let step = (read != 0, write != 0);
            let cap = Some(match cap {
                Some((r, w)) => (r && step.0, w && step.1),
                None => step,
            });
            if let Some(target) = target_id(conn, backend, &uri).await? {
                queue.push((target, cap, via.clone().or(Some(uri)), depth + 1));
            }
        }
    }
    // The first sighting of a DID seeds its entry; a later, more permissive
    // one is merged in above. A direct member's own access needs no source.
    for entry in resolved.values_mut() {
        if entry.direct && entry.via.is_some() {
            entry.via = None;
        }
    }
    Ok(resolved)
}

/// Whether an earlier visit's cap grants everything `cap` does.
fn covers(earlier: Cap, cap: Cap) -> bool {
    match (earlier, cap) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some((r1, w1)), Some((r2, w2))) => (r1 || !r2) && (w1 || !w2),
    }
}

/// The local space a reference names: an `at://` address, or (for rows from
/// before addresses) a space id.
async fn target_id(
    conn: &mut sqlx::AnyConnection,
    backend: DatabaseBackend,
    reference: &str,
) -> Result<Option<String>, AppError> {
    if reference.starts_with("at://") || reference.starts_with("ats://") {
        let Ok(uri) = SpaceUri::parse(reference) else {
            return Ok(None);
        };
        let row: Option<(String,)> = crate::db::query_as(&adapt_sql(
            "SELECT id FROM happyview_spaces WHERE did = ? AND type_nsid = ? AND skey = ?",
            backend,
        ))
        .bind(&uri.did)
        .bind(&uri.type_nsid)
        .bind(&uri.skey)
        .fetch_optional(&mut *conn)
        .await
        .map_err(sql_error)?;
        Ok(row.map(|r| r.0))
    } else {
        let row: Option<(String,)> = crate::db::query_as(&adapt_sql(
            "SELECT id FROM happyview_spaces WHERE id = ?",
            backend,
        ))
        .bind(reference)
        .fetch_optional(&mut *conn)
        .await
        .map_err(sql_error)?;
        Ok(row.map(|r| r.0))
    }
}

/// The spaces that reach `space_id` through nested references or
/// simplespace space-members, each once, up to [`MAX_DELEGATION_DEPTH`]
/// levels up, not counting `space_id` itself.
pub async fn reaching_on(
    conn: &mut sqlx::AnyConnection,
    backend: DatabaseBackend,
    space_id: &str,
) -> Result<Vec<String>, AppError> {
    let mut found: Vec<String> = Vec::new();
    let mut visited: HashSet<String> = HashSet::from([space_id.to_string()]);
    let mut frontier = vec![space_id.to_string()];
    let mut budget = MAX_RESOLVED;
    for _ in 0..MAX_DELEGATION_DEPTH {
        let mut next = Vec::new();
        for id in frontier {
            let Some(space) = db::get_space(&mut *conn, backend, &id).await? else {
                continue;
            };
            let address = format!(
                "at://{}/space/{}/{}",
                space.did, space.type_nsid, space.skey
            );
            let mut parents: Vec<(String,)> = crate::db::query_as(&adapt_sql(
                "SELECT space_id FROM happyview_space_nested WHERE target_space_uri = ?",
                backend,
            ))
            .bind(&address)
            .fetch_all(&mut *conn)
            .await
            .map_err(sql_error)?;
            let legacy: Vec<(String,)> = crate::db::query_as(&adapt_sql(
                "SELECT space_id FROM happyview_space_members WHERE is_delegation = 1 AND (member_did = ? OR member_did = ?)",
                backend,
            ))
            .bind(&address)
            .bind(&id)
            .fetch_all(&mut *conn)
            .await
            .map_err(sql_error)?;
            parents.extend(legacy);
            budget = budget.checked_sub(parents.len()).ok_or_else(too_large)?;
            for (parent,) in parents {
                if visited.insert(parent.clone()) {
                    found.push(parent.clone());
                    next.push(parent);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    Ok(found)
}

async fn with_conn(
    pool: &sqlx::AnyPool,
) -> Result<sqlx::pool::PoolConnection<sqlx::Any>, AppError> {
    pool.acquire()
        .await
        .map_err(|e| AppError::Internal(format!("failed to acquire connection: {e}")))
}

/// Resolve the full member list for a space, nested spaces included. If a
/// DID is reached more than once, the most permissive access wins.
pub async fn resolve_members(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    space_id: &str,
) -> Result<Vec<ResolvedMember>, AppError> {
    let mut conn = with_conn(pool).await?;
    Ok(resolve_on(&mut conn, backend, space_id)
        .await?
        .into_iter()
        .map(|(did, e)| ResolvedMember {
            did,
            access: e.access,
        })
        .collect())
}

/// The effective access a DID has to a space, nested spaces included.
pub async fn is_member(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    space_id: &str,
    did: &str,
) -> Result<Option<MemberAccess>, AppError> {
    let mut conn = with_conn(pool).await?;
    Ok(resolve_on(&mut conn, backend, space_id)
        .await?
        .remove(did)
        .map(|e| e.access))
}

#[cfg(test)]
mod tests {
    use super::covers;

    #[test]
    fn no_cap_covers_every_cap() {
        for cap in [
            None,
            Some((true, true)),
            Some((true, false)),
            Some((false, false)),
        ] {
            assert!(covers(None, cap), "{cap:?}");
        }
    }

    #[test]
    fn a_cap_covers_only_caps_it_grants_at_least() {
        assert!(!covers(Some((true, true)), None));
        assert!(covers(Some((true, true)), Some((true, false))));
        assert!(covers(Some((true, false)), Some((true, false))));
        // A read-only path seen first doesn't hide a read-write one.
        assert!(!covers(Some((true, false)), Some((true, true))));
        assert!(!covers(Some((false, true)), Some((true, false))));
    }
}
