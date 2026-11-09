//! Spaces whose stored rules predate policies and match none.
//!
//! Before policies, a space's read rule, write rule and app access were set
//! independently, so a space could pair a managing app with a member list or
//! an app allow list. No policy describes that ([`SpacePolicy::of`] refuses
//! it), and it is retired: such a space reads only through its managing app
//! ([`crate::spaces::service::require_membership`]), and its owner converts
//! it to a memberspace or a managedspace.
//!
//! How many exist on an instance can't be known ahead of time, so they are
//! found at boot and logged, and listed to operators.

use serde::Serialize;

use crate::AppState;
use crate::db::DatabaseBackend;
use crate::error::AppError;
use crate::spaces::db;
use crate::spaces::policy::SpacePolicy;
use crate::spaces::types::{AppAccess, Policy, Space};

/// A space in a retired shape, as operators see it: where it is and what its
/// rules are, never what it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetiredSpace {
    pub id: String,
    pub uri: String,
    pub space_type: String,
    pub skey: String,
    pub creator_did: String,
    pub read_policy: Policy,
    pub write_policy: Policy,
    pub app_access: AppAccess,
}

impl RetiredSpace {
    fn of(space: Space) -> Self {
        Self {
            uri: format!(
                "at://{}/space/{}/{}",
                space.did, space.type_nsid, space.skey
            ),
            id: space.id,
            space_type: space.type_nsid,
            skey: space.skey,
            creator_did: space.creator_did,
            read_policy: space.read_policy,
            write_policy: space.write_policy,
            app_access: space.app_access,
        }
    }
}

/// Every space whose rules match no policy.
pub async fn find(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
) -> Result<Vec<RetiredSpace>, AppError> {
    // Only a managing-app rule can make a shape no policy describes; the
    // query narrows to those, and `SpacePolicy::of` decides.
    Ok(db::list_spaces_with_managing_app_rules(pool, backend)
        .await?
        .into_iter()
        .filter(|space| SpacePolicy::of(space).is_err())
        .map(RetiredSpace::of)
        .collect())
}

/// Log each space in a retired shape, once, after boot.
pub fn spawn_boot_check(state: AppState) {
    tokio::spawn(async move {
        match find(&state.db, state.db_backend).await {
            Ok(spaces) => {
                for space in &spaces {
                    tracing::warn!(
                        space = %space.uri,
                        read_policy = %serde_json::to_string(&space.read_policy).unwrap_or_default(),
                        write_policy = %serde_json::to_string(&space.write_policy).unwrap_or_default(),
                        app_access = %serde_json::to_string(&space.app_access).unwrap_or_default(),
                        "space uses a retired policy shape; convert it to a memberspace or a managedspace"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not check spaces for retired policy shapes")
            }
        }
    });
}
