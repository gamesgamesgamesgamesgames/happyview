//! Published copies of a space's configuration.
//!
//! A policy can ask for itself and for its member list to be published to the
//! space's readers (`publishPolicy`, `publishMembers`), as records in the
//! authority's own repo in the space:
//!
//! - `com.atproto.space.policy/self`, holding the policy, for a `memberspace`
//!   or `managedspace` that sets `publishPolicy`;
//! - `com.atproto.memberspace.member/<did>`, holding `{read, write}`, one per
//!   member of a `memberspace` that sets `publishMembers`.
//!
//! The policy and the member list stay the source of truth. HappyView keeps
//! these records in step with them, and refuses a direct write to either
//! collection in any repo ([`refuse_direct_write`]).

use std::collections::BTreeMap;

use axum::http::StatusCode;

use crate::AppState;
use crate::db::{DatabaseBackend, now_rfc3339};
use crate::error::AppError;
use crate::spaces::db;
use crate::spaces::policy::SpacePolicy;
use crate::spaces::service::{self, AppliedOp, CommittedWrite};
use crate::spaces::types::{OplogAction, Space, SpaceRecord};

pub const POLICY_COLLECTION: &str = "com.atproto.space.policy";
pub const POLICY_RKEY: &str = "self";
pub const MEMBER_COLLECTION: &str = "com.atproto.memberspace.member";

/// Refuse a direct write to a collection HappyView publishes from a space's
/// configuration, whichever repo it names.
pub(crate) fn refuse_direct_write(collection: &str) -> Result<(), AppError> {
    if collection == POLICY_COLLECTION || collection == MEMBER_COLLECTION {
        return Err(AppError::XrpcError {
            status: StatusCode::BAD_REQUEST,
            code: "InvalidRequest",
            message: format!(
                "{collection} records are published from a space's configuration and cannot \
                 be written directly"
            ),
        });
    }
    Ok(())
}

/// A DID as a member record's key. A record key cannot hold `%`, so the
/// percent-encoded port of a localhost `did:web` is written unencoded, as the
/// reference implementation does.
pub fn member_rkey(did: &str) -> String {
    did.replace("%3A", ":")
}

/// Which published records a change can have touched.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Scope<'a> {
    /// The policy record and every member record: after the policy changes.
    All,
    /// These DIDs' member records: after their entries change.
    Members(&'a [String]),
}

/// A commit to the authority's repo that brought its published records into
/// step, owed to syncers once the transaction commits.
pub(crate) struct Published {
    author_did: String,
    rev: String,
    ops: Vec<AppliedOp>,
    committed: CommittedWrite,
}

impl Published {
    /// Tell syncers about the commit. Call it after the transaction commits.
    pub(crate) async fn announce(self, state: &AppState, space: &Space) {
        service::notify_ops(
            state,
            space,
            &self.author_did,
            &self.ops,
            &self.rev,
            self.committed,
        )
        .await;
    }
}

/// Whether a write may touch `space`'s published records at all: the policy
/// publishes something, or records it published before may need withdrawing.
/// Read through the pool, so call it before the transaction opens.
pub(crate) async fn may_touch(
    state: &AppState,
    space: &Space,
    scope: Scope<'_>,
) -> Result<bool, AppError> {
    let (policy, members) = flags(space);
    let wanted = match scope {
        Scope::All => policy || members,
        Scope::Members(_) => members,
    };
    if wanted {
        return Ok(true);
    }
    // Scope::Members with publishing off has nothing to withdraw: turning
    // `publishMembers` off withdraws the whole list (Scope::All).
    if let Scope::All = scope {
        for collection in [POLICY_COLLECTION, MEMBER_COLLECTION] {
            let found = db::repo_collection_records(
                &state.db,
                state.db_backend,
                &space.id,
                &space.authority_did,
                collection,
                None,
            )
            .await?;
            if !found.is_empty() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// The `publishPolicy` and `publishMembers` a space's policy sets. A space
/// whose rules match no policy publishes nothing.
fn flags(space: &Space) -> (bool, bool) {
    match SpacePolicy::of(space) {
        Ok(SpacePolicy::Member(m)) => (m.publish_policy, m.publish_members),
        Ok(SpacePolicy::Managed(m)) => (m.publish_policy, false),
        Err(_) => (false, false),
    }
}

/// Bring `space`'s published records in `scope` into step with its policy
/// and member list, in the caller's transaction, as one commit to the
/// authority's repo. `space` must be as it stands in that transaction.
/// Returns `None` when nothing needed to change.
///
/// A space whose rules match no policy is left as it is: what it publishes
/// cannot be told, so nothing is withdrawn either.
pub(crate) async fn sync_in(
    conn: &mut sqlx::AnyConnection,
    backend: DatabaseBackend,
    space: &Space,
    scope: Scope<'_>,
    signing_key: &p256::ecdsa::SigningKey,
) -> Result<Option<Published>, AppError> {
    let Ok(policy) = SpacePolicy::of(space) else {
        return Ok(None);
    };
    let (publish_policy, publish_members) = flags(space);
    let author = space.authority_did.as_str();

    // What should be published, and what is, keyed by (collection, rkey).
    let mut wanted: BTreeMap<(&str, String), serde_json::Value> = BTreeMap::new();
    let mut held: BTreeMap<(&str, String), String> = BTreeMap::new();

    if let Scope::All = scope {
        if publish_policy {
            let policy = serde_json::to_value(&policy)
                .map_err(|e| AppError::Internal(format!("failed to encode policy: {e}")))?;
            wanted.insert(
                (POLICY_COLLECTION, POLICY_RKEY.to_string()),
                serde_json::json!({ "$type": POLICY_COLLECTION, "policy": policy }),
            );
        }
        for record in db::repo_collection_records(
            &mut *conn,
            backend,
            &space.id,
            author,
            POLICY_COLLECTION,
            None,
        )
        .await?
        {
            held.insert((POLICY_COLLECTION, record.rkey), record.cid);
        }
    }

    let (only_dids, only_rkeys) = match scope {
        Scope::All => (None, None),
        Scope::Members(dids) => (
            Some(dids),
            Some(dids.iter().map(|d| member_rkey(d)).collect::<Vec<_>>()),
        ),
    };
    if publish_members {
        // The published list is the effective one: nested spaces' members
        // included, with the access they actually have.
        let effective = crate::spaces::members::resolve_on(&mut *conn, backend, &space.id).await?;
        let listed = effective
            .into_iter()
            .filter(|(did, _)| only_dids.is_none_or(|only| only.contains(did)))
            .map(|(did, e)| (did, e.access));
        for (did, access) in listed {
            wanted.insert(
                (MEMBER_COLLECTION, member_rkey(&did)),
                serde_json::json!({
                    "$type": MEMBER_COLLECTION,
                    "read": access.read,
                    "write": access.write,
                }),
            );
        }
    }
    for record in db::repo_collection_records(
        &mut *conn,
        backend,
        &space.id,
        author,
        MEMBER_COLLECTION,
        only_rkeys.as_deref(),
    )
    .await?
    {
        held.insert((MEMBER_COLLECTION, record.rkey), record.cid);
    }

    let now = now_rfc3339();
    let mut ops = Vec::new();
    for ((collection, rkey), value) in wanted {
        let cid = service::content_cid(&value)?;
        let old_cid = held.remove(&(collection, rkey.clone()));
        if old_cid.as_deref() == Some(cid.as_str()) {
            continue;
        }
        let record = SpaceRecord {
            uri: record_uri(space, collection, &rkey),
            space_id: space.id.clone(),
            author_did: author.to_string(),
            collection: collection.to_string(),
            rkey: rkey.clone(),
            record: value,
            cid: cid.clone(),
            indexed_at: now.clone(),
        };
        db::upsert_space_record(&mut *conn, backend, &record).await?;
        ops.push(AppliedOp {
            action: if old_cid.is_some() {
                OplogAction::Update
            } else {
                OplogAction::Create
            },
            collection: collection.to_string(),
            rkey,
            new_cid: Some(cid),
            old_cid,
        });
    }
    for ((collection, rkey), old_cid) in held {
        db::delete_space_record(&mut *conn, backend, &record_uri(space, collection, &rkey)).await?;
        ops.push(AppliedOp {
            action: OplogAction::Delete,
            collection: collection.to_string(),
            rkey,
            new_cid: None,
            old_cid: Some(old_cid),
        });
    }
    if ops.is_empty() {
        return Ok(None);
    }

    let rev = crate::tid::generate_tid();
    let committed =
        service::commit_write(conn, backend, space, author, &rev, &ops, signing_key).await?;
    Ok(Some(Published {
        author_did: author.to_string(),
        rev,
        ops,
        committed,
    }))
}

/// [`sync_in`] in a transaction of its own, for a change already committed,
/// and announce the commit. The space is read again under its lock, so a
/// policy change that raced this one is the one published.
pub(crate) async fn sync(
    state: &AppState,
    space_id: &str,
    scope: Scope<'_>,
) -> Result<(), AppError> {
    let Some(space) = db::get_space(&state.db, state.db_backend, space_id).await? else {
        return Ok(());
    };
    if !may_touch(state, &space, scope).await? {
        return Ok(());
    }
    // Before the transaction; see `service_signing_key`.
    let signing_key = service::service_signing_key(state).await?;
    let mut tx = service::begin_space_write(state, space_id).await?;
    let Some(space) = db::lock_space(&mut *tx, state.db_backend, space_id)
        .await
        .map_err(|e| AppError::Internal(format!("failed to lock space: {e}")))?
    else {
        return Ok(());
    };
    let published = sync_in(&mut tx, state.db_backend, &space, scope, &signing_key).await?;
    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("failed to commit transaction: {e}")))?;
    if let Some(published) = published {
        published.announce(state, &space).await;
    }
    Ok(())
}

fn record_uri(space: &Space, collection: &str, rkey: &str) -> String {
    format!(
        "at://{}/space/{}/{}/{}/{}/{}",
        space.did, space.type_nsid, space.skey, space.authority_did, collection, rkey
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_localhost_did_web_port_is_written_unencoded() {
        assert_eq!(
            member_rkey("did:web:localhost%3A2583"),
            "did:web:localhost:2583"
        );
        assert_eq!(member_rkey("did:plc:abc"), "did:plc:abc");
    }

    #[test]
    fn only_the_published_collections_are_refused() {
        assert!(refuse_direct_write(POLICY_COLLECTION).is_err());
        assert!(refuse_direct_write(MEMBER_COLLECTION).is_err());
        assert!(refuse_direct_write("com.example.post").is_ok());
        assert!(refuse_direct_write("com.atproto.memberspace.members").is_ok());
    }
}
