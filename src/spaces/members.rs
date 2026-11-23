//! A space's effective member list.
//!
//! A memberspace's members are its direct members, plus the members of every
//! space it nests (`happyview_space_nested`), resolved when access is checked
//! rather than copied into the member table (HAP-388 §11). A nested space's
//! member gets at most the access the reference grants. So does a member of a
//! space named as a member the simplespace way (an `is_delegation` row): at
//! most the read and write the row gives, at every level it is reached
//! through.
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
use crate::spaces::types::{MemberAccess, ResolvedMember, Space};

const MAX_DELEGATION_DEPTH: usize = 10;

/// The most member rows and references one resolution reads.
pub const MAX_RESOLVED: usize = 10_000;

/// A member of the effective list, and where its access came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effective {
    pub access: MemberAccess,
    /// Whether the space lists the DID itself.
    pub direct: bool,
    /// The first nested or listed space the DID was found through, if any.
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

/// Whether `e` is the refusal of a resolution past the bound.
pub fn is_too_large(e: &AppError) -> bool {
    matches!(
        e,
        AppError::XrpcError {
            code: "MemberListTooLarge",
            ..
        }
    )
}

fn sql_error(e: sqlx::Error) -> AppError {
    AppError::Internal(format!("failed to resolve members: {e}"))
}

type Row = (String, i32, i32, i32, i32, i32, Option<String>);

/// The read and write a path of references allows; `None` before any.
type Cap = Option<(bool, bool)>;

/// A space to visit: its id, the cap of the path to it, the first nested or
/// listed space on that path, and its depth.
type Visit = (String, Cap, Option<String>, usize);

/// Resolve a space's effective member list on `conn`, which may be inside a
/// write transaction.
pub async fn resolve_on(
    conn: &mut sqlx::AnyConnection,
    backend: DatabaseBackend,
    space_id: &str,
) -> Result<Resolution, AppError> {
    resolve(conn, backend, space_id, false).await
}

/// [`resolve_on`], in the caller's write transaction, holding every space it
/// reads members or references from until that transaction ends.
///
/// Member changes lock their space's row first (`db::lock_space_for_members`),
/// and so do nesting changes. On Postgres each space visited here is
/// share-locked before it is read, so a change to any of them either commits
/// before this read or waits for the caller's transaction. On SQLite the
/// caller's `BEGIN IMMEDIATE` already orders it with every other writer.
pub async fn resolve_locked(
    conn: &mut sqlx::AnyConnection,
    backend: DatabaseBackend,
    space_id: &str,
) -> Result<Resolution, AppError> {
    resolve(conn, backend, space_id, true).await
}

/// The effective access a DID has to a space, read as [`resolve_locked`]
/// reads it.
pub async fn is_member_locked(
    conn: &mut sqlx::AnyConnection,
    backend: DatabaseBackend,
    space_id: &str,
    did: &str,
) -> Result<Option<MemberAccess>, AppError> {
    Ok(resolve_locked(conn, backend, space_id)
        .await?
        .remove(did)
        .map(|e| e.access))
}

/// [`resolve_members`], read on the caller's connection, so inside its
/// transaction.
pub async fn resolve_members_on(
    conn: &mut sqlx::AnyConnection,
    backend: DatabaseBackend,
    space_id: &str,
) -> Result<Vec<ResolvedMember>, AppError> {
    Ok(resolve_on(conn, backend, space_id)
        .await?
        .into_iter()
        .map(|(did, e)| ResolvedMember {
            did,
            access: e.access,
        })
        .collect())
}

/// The spaces that take `space_id`'s members through delegation entries or
/// nested references, directly or through other spaces, up to the depth the
/// resolver follows. Each is listed once, in id order; `space_id` itself is
/// not listed. Past [`MAX_RESOLVED`] entries read, the lookup fails with
/// `MemberListTooLarge`, as resolution does.
///
/// This is the reverse of [`resolve_members`]: a change to `space_id`'s
/// members can change who reads each of these.
pub async fn delegating_spaces(
    conn: &mut sqlx::AnyConnection,
    backend: DatabaseBackend,
    space_id: &str,
) -> Result<Vec<String>, AppError> {
    delegating_spaces_within(conn, backend, space_id, MAX_RESOLVED).await
}

/// [`delegating_spaces`], reading at most `budget` entries.
async fn delegating_spaces_within(
    conn: &mut sqlx::AnyConnection,
    backend: DatabaseBackend,
    space_id: &str,
    budget: usize,
) -> Result<Vec<String>, AppError> {
    let mut budget = budget;
    let mut visited: HashSet<String> = HashSet::from([space_id.to_string()]);
    let mut found: Vec<String> = Vec::new();
    let mut frontier = vec![space_id.to_string()];
    for _ in 1..MAX_DELEGATION_DEPTH {
        let mut next = Vec::new();
        for child_id in &frontier {
            // Entries that bring in the child's members keep the id of the
            // space they were consented for, as nested references keep the
            // id of the space they name. Whether each still resolves there
            // isn't checked: listing one that no longer does only re-checks
            // a space whose readers can't have changed.
            let delegating: Vec<(String,)> = crate::db::query_as(&adapt_sql(
                "SELECT space_id FROM happyview_space_members \
                 WHERE delegation_target_space_id = ? AND delegation_consented = 1 \
                 AND is_delegation = 1",
                backend,
            ))
            .bind(child_id)
            .fetch_all(&mut *conn)
            .await
            .map_err(sql_error)?;
            budget = budget.checked_sub(delegating.len()).ok_or_else(too_large)?;
            let mut parents: Vec<String> = delegating.into_iter().map(|(id,)| id).collect();
            let nesting: Vec<(String,)> = crate::db::query_as(&adapt_sql(
                "SELECT space_id FROM happyview_space_nested WHERE target_space_id = ?",
                backend,
            ))
            .bind(child_id)
            .fetch_all(&mut *conn)
            .await
            .map_err(sql_error)?;
            budget = budget.checked_sub(nesting.len()).ok_or_else(too_large)?;
            parents.extend(nesting.into_iter().map(|(id,)| id));
            for parent in parents {
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
    found.sort();
    Ok(found)
}

async fn resolve(
    conn: &mut sqlx::AnyConnection,
    backend: DatabaseBackend,
    space_id: &str,
    lock: bool,
) -> Result<Resolution, AppError> {
    let mut resolved = Resolution::new();
    // A space is visited once per access cap it is reached with, so a
    // restrictive path seen first doesn't hide a more permissive one. There
    // are four caps and the root's none, so each space is read at most five
    // times, and once when its first path is unrestricted.
    let mut visited: std::collections::HashMap<String, Vec<Cap>> = std::collections::HashMap::new();
    let mut budget = MAX_RESOLVED;
    // (space id, access cap from the references and entries so far, the
    // first nested or listed space on the path, depth). The root has no cap and no source.
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
        if lock && backend == DatabaseBackend::Postgres {
            crate::db::query("SELECT id FROM happyview_spaces WHERE id = $1 FOR SHARE")
                .bind(&id)
                .execute(&mut *conn)
                .await
                .map_err(sql_error)?;
        }
        let rows: Vec<Row> = crate::db::query_as(&adapt_sql(
            "SELECT member_did, can_read, can_write, read_self, is_delegation, delegation_consented, delegation_target_space_id FROM happyview_space_members WHERE space_id = ?",
            backend,
        ))
        .bind(&id)
        .fetch_all(&mut *conn)
        .await
        .map_err(sql_error)?;
        let refs: Vec<(String, i32, i32, Option<String>)> = crate::db::query_as(&adapt_sql(
            "SELECT target_space_uri, can_read, can_write, target_space_id FROM happyview_space_nested WHERE space_id = ?",
            backend,
        ))
        .bind(&id)
        .fetch_all(&mut *conn)
        .await
        .map_err(sql_error)?;
        budget = budget
            .checked_sub(rows.len() + refs.len())
            .ok_or_else(too_large)?;

        for (did, read, write, read_self, is_delegation, consented, consented_to) in rows {
            if is_delegation != 0 {
                // Simplespace's space-as-member: its members get at most the
                // entry's read and write, as a nested space's do. Only an
                // entry added by someone who administers the named space
                // brings its members in, and only while it names that same
                // space.
                let Some(consented_to) = consented_to.filter(|_| consented != 0) else {
                    continue;
                };
                let target = target_id(conn, backend, &did).await?;
                if let Some(target) = target.filter(|t| *t == consented_to) {
                    let cap = narrow(cap, (read != 0, write != 0));
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
        for (uri, read, write, nested_id) in refs {
            let cap = narrow(cap, (read != 0, write != 0));
            // Only while the address still names the space that was nested.
            let target = target_id(conn, backend, &uri).await?;
            if let Some(target) = target.filter(|t| nested_id.as_deref() == Some(t.as_str())) {
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

/// The cap of a path that goes on through a reference or entry granting
/// `step`.
fn narrow(cap: Cap, step: (bool, bool)) -> Cap {
    Some(match cap {
        Some((r, w)) => (r && step.0, w && step.1),
        None => step,
    })
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

/// The local space a space-as-member entry names: an `at://` (or legacy
/// `ats://`) address, or a space id.
pub async fn delegation_target(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    reference: &str,
) -> Result<Option<Space>, AppError> {
    if reference.starts_with("at://") || reference.starts_with("ats://") {
        let uri = SpaceUri::parse(reference)?;
        db::get_space_by_address(pool, backend, &uri.did, &uri.type_nsid, &uri.skey).await
    } else {
        db::get_space(pool, backend, reference).await
    }
}

#[cfg(test)]
mod tests {
    use super::{covers, narrow};

    #[test]
    fn a_step_narrows_the_cap_to_what_both_grant() {
        assert_eq!(narrow(None, (true, false)), Some((true, false)));
        assert_eq!(
            narrow(Some((true, true)), (false, true)),
            Some((false, true))
        );
        assert_eq!(
            narrow(Some((true, false)), (true, true)),
            Some((true, false))
        );
        assert_eq!(
            narrow(Some((false, true)), (true, false)),
            Some((false, false))
        );
    }

    /// The reverse lookup stops at its budget, by name, rather than answer
    /// with part of the list.
    #[tokio::test]
    async fn the_reverse_lookup_is_bounded() {
        use crate::spaces::db;
        use crate::spaces::types::{AppAccess, Policy, Space, SpaceConfig};
        let pool = crate::test_support::migrated_memory_pool().await;
        let backend = crate::db::DatabaseBackend::Sqlite;
        let space = |n: usize| Space {
            id: format!("space{n}"),
            did: format!("did:plc:space{n}"),
            authority_did: format!("did:plc:space{n}"),
            creator_did: "did:plc:owner".into(),
            type_nsid: "com.example.forum".into(),
            skey: format!("s{n}"),
            display_name: None,
            description: None,
            read_policy: Policy::MemberList,
            write_policy: Policy::MemberList,
            app_access: AppAccess::default(),
            config: SpaceConfig::default(),
            revision: None,
            created_at: crate::db::now_rfc3339(),
            updated_at: crate::db::now_rfc3339(),
        };
        let nested = space(0);
        db::create_space(&pool, backend, &nested).await.unwrap();
        for n in 1..=4 {
            db::create_space(&pool, backend, &space(n)).await.unwrap();
            db::upsert_nested(
                &pool,
                backend,
                &format!("space{n}"),
                &nested.id,
                &db::NestedRef {
                    target_space_uri: "at://did:plc:space0/space/com.example.forum/s0".into(),
                    read: true,
                    write: false,
                    added_by: None,
                    created_at: crate::db::now_rfc3339(),
                },
            )
            .await
            .unwrap();
        }
        let mut conn = pool.acquire().await.unwrap();
        let found = super::delegating_spaces_within(&mut conn, backend, &nested.id, 4)
            .await
            .unwrap();
        assert_eq!(found.len(), 4);
        let err = super::delegating_spaces_within(&mut conn, backend, &nested.id, 3)
            .await
            .expect_err("past the budget");
        assert!(super::is_too_large(&err), "{err:?}");
    }

    /// The spaces that take a space's members through entries naming it
    /// are the ones whose entries were consented for that space, whichever
    /// name the entry uses; an entry stored without consent, or consented for
    /// another space, takes none.
    #[tokio::test]
    async fn the_reverse_lookup_follows_consented_entries_by_id() {
        use crate::spaces::db;
        use crate::spaces::types::{
            AppAccess, MemberAccess, Policy, Space, SpaceConfig, SpaceMember,
        };
        let pool = crate::test_support::migrated_memory_pool().await;
        let backend = crate::db::DatabaseBackend::Sqlite;
        let space = |n: usize| Space {
            id: format!("space{n}"),
            did: format!("did:plc:space{n}"),
            authority_did: format!("did:plc:space{n}"),
            creator_did: "did:plc:owner".into(),
            type_nsid: "com.example.forum".into(),
            skey: format!("s{n}"),
            display_name: None,
            description: None,
            read_policy: Policy::MemberList,
            write_policy: Policy::MemberList,
            app_access: AppAccess::default(),
            config: SpaceConfig::default(),
            revision: None,
            created_at: crate::db::now_rfc3339(),
            updated_at: crate::db::now_rfc3339(),
        };
        for n in 0..=5 {
            db::create_space(&pool, backend, &space(n)).await.unwrap();
        }
        let entry = |space_id: &str, reference: &str| SpaceMember {
            id: uuid::Uuid::new_v4().to_string(),
            space_id: space_id.to_string(),
            did: reference.to_string(),
            access: MemberAccess::READ,
            is_delegation: true,
            granted_by: None,
            created_at: crate::db::now_rfc3339(),
        };
        let consented = [
            ("space1", "at://did:plc:space0/space/com.example.forum/s0"),
            ("space2", "ats://did:plc:space0/com.example.forum/s0"),
            ("space3", "space0"),
        ];
        for (parent, reference) in consented {
            assert!(
                db::insert_delegation_if_allowed(
                    &pool,
                    backend,
                    &entry(parent, reference),
                    db::DelegationConsent::Granted("space0"),
                )
                .await
                .unwrap()
            );
        }
        // Names space0, stored without consent.
        db::add_member(&pool, backend, &entry("space4", "space0"))
            .await
            .unwrap();
        // Names space0, consented for another space.
        assert!(
            db::insert_delegation_if_allowed(
                &pool,
                backend,
                &entry("space5", "space0"),
                db::DelegationConsent::Granted("space4"),
            )
            .await
            .unwrap()
        );
        let mut conn = pool.acquire().await.unwrap();
        let found = super::delegating_spaces(&mut conn, backend, "space0")
            .await
            .unwrap();
        assert_eq!(found, vec!["space1", "space2", "space3"]);
    }

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
