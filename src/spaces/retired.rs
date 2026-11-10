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

/// What an operator converts a space in a retired shape to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConvertTo {
    /// The member list governs reads and writes; the managing app no longer
    /// does. Members and invites stay.
    Memberspace,
    /// The managing app governs reads and writes. Members and invites go.
    Managedspace,
}

fn flag(space: &Space, name: &str) -> bool {
    space
        .config
        .extra
        .get(name)
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// The policy a retired space converts to, keeping what each direction can.
///
/// To a memberspace, a managing-app rule becomes the member list, a public
/// rule stays public, and an app allow list and the publish flags carry over.
/// To a managedspace, the managing app is the one the rules name, or
/// `managing_app` when the read and write rules name different ones.
pub fn conversion(
    space: &Space,
    to: ConvertTo,
    managing_app: Option<&str>,
) -> Result<SpacePolicy, AppError> {
    use crate::spaces::policy::Memberspace;
    match to {
        ConvertTo::Memberspace => Ok(SpacePolicy::Member(Memberspace {
            public_read: space.read_policy == Policy::Public,
            public_write: space.write_policy == Policy::Public,
            allowed_apps: match &space.app_access {
                AppAccess::Open => None,
                AppAccess::AllowList { allowed } => Some(allowed.clone()),
            },
            publish_policy: flag(space, "publishPolicy"),
            publish_members: flag(space, "publishMembers"),
        })),
        ConvertTo::Managedspace => {
            let named: Vec<&str> = [&space.read_policy, &space.write_policy]
                .into_iter()
                .filter_map(|rule| match rule {
                    Policy::ManagingApp { managing_app } => Some(managing_app.as_str()),
                    _ => None,
                })
                .collect();
            let app = match managing_app {
                Some(app) => app.to_string(),
                None => match named.as_slice() {
                    [one] => one.to_string(),
                    [a, b] if a == b => a.to_string(),
                    _ => {
                        return Err(AppError::BadRequest(
                            "this space's rules name more than one managing app; say which one \
                             the managedspace keeps"
                                .into(),
                        ));
                    }
                },
            };
            SpacePolicy::parse(serde_json::json!({
                "$type": crate::spaces::policy::MANAGEDSPACE,
                "managingApp": app,
                "publishPolicy": flag(space, "publishPolicy"),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spaces::types::SpaceConfig;

    fn space(read: Policy, write: Policy, apps: AppAccess) -> Space {
        Space {
            id: "s".into(),
            did: "did:plc:space".into(),
            authority_did: "did:plc:space".into(),
            creator_did: "did:plc:creator".into(),
            type_nsid: "com.example.forum".into(),
            skey: "main".into(),
            display_name: None,
            description: None,
            read_policy: read,
            write_policy: write,
            app_access: apps,
            config: SpaceConfig::default(),
            revision: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn app(did: &str) -> Policy {
        Policy::ManagingApp {
            managing_app: did.into(),
        }
    }

    #[test]
    fn to_a_memberspace_keeps_a_public_rule_and_the_allow_list() {
        let allow = AppAccess::AllowList {
            allowed: vec!["https://app.example/client-metadata.json".into()],
        };
        let mixed = space(app("did:plc:app"), Policy::Public, allow.clone());
        let SpacePolicy::Member(member) = conversion(&mixed, ConvertTo::Memberspace, None).unwrap()
        else {
            panic!("not a memberspace");
        };
        assert!(!member.public_read);
        assert!(member.public_write);
        assert_eq!(
            member.allowed_apps,
            Some(vec!["https://app.example/client-metadata.json".into()])
        );
    }

    #[test]
    fn to_a_managedspace_takes_the_app_the_rules_name() {
        let mixed = space(Policy::MemberList, app("did:plc:app"), AppAccess::Open);
        assert_eq!(
            conversion(&mixed, ConvertTo::Managedspace, None).unwrap(),
            SpacePolicy::parse(serde_json::json!({
                "$type": crate::spaces::policy::MANAGEDSPACE,
                "managingApp": "did:plc:app",
            }))
            .unwrap()
        );
    }

    #[test]
    fn two_different_apps_need_one_named() {
        let mixed = space(app("did:plc:a"), app("did:plc:b"), AppAccess::Open);
        assert!(conversion(&mixed, ConvertTo::Managedspace, None).is_err());
        let SpacePolicy::Managed(managed) =
            conversion(&mixed, ConvertTo::Managedspace, Some("did:plc:b")).unwrap()
        else {
            panic!("not a managedspace");
        };
        assert_eq!(managed.managing_app, "did:plc:b");
    }
}
