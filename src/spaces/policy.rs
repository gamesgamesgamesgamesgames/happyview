//! A space's `policy`, as Proposal 0016 shapes it.
//!
//! The protocol configures a space with one `policy` object, named by its
//! `$type`. HappyView implements `memberspace` and `managedspace`, and stores
//! either as the read policy, write policy and app access it has always
//! enforced, so one stored space has exactly one policy:
//!
//! | Policy | Read | Write | Apps |
//! |---|---|---|---|
//! | `memberspace` | `publicRead` ? public : member list | `publicWrite` ? public : member list | `allowedApps` ? allow list : open |
//! | `managedspace` | the managing app | the managing app | open |
//!
//! `personalspace` gives access to the authority's own account only. A space
//! here lives on a DID of its own that no one signs in as, so it would be
//! readable by no one, and is refused as unsupported.

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::spaces::types::{AppAccess, Policy, Space, SpaceConfig};

pub const MEMBERSPACE: &str = "com.atproto.memberspace.policy";
pub const MANAGEDSPACE: &str = "com.atproto.managedspace.policy";
pub const PERSONALSPACE: &str = "com.atproto.personalspace.policy";

/// Where the `publish*` flags live in a stored space's config.
const PUBLISH_POLICY: &str = "publishPolicy";
const PUBLISH_MEMBERS: &str = "publishMembers";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "$type")]
pub enum SpacePolicy {
    #[serde(rename = "com.atproto.memberspace.policy")]
    Member(Memberspace),
    #[serde(rename = "com.atproto.managedspace.policy")]
    Managed(Managedspace),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Memberspace {
    #[serde(default)]
    pub public_read: bool,
    #[serde(default)]
    pub public_write: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_apps: Option<Vec<String>>,
    #[serde(default)]
    pub publish_policy: bool,
    #[serde(default)]
    pub publish_members: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Managedspace {
    pub managing_app: String,
    #[serde(default)]
    pub publish_policy: bool,
}

/// What a policy is stored as.
pub struct Stored {
    pub read_policy: Policy,
    pub write_policy: Policy,
    pub app_access: AppAccess,
    pub publish_policy: bool,
    pub publish_members: bool,
}

fn unsupported(message: String) -> AppError {
    AppError::XrpcError {
        status: StatusCode::BAD_REQUEST,
        code: "UnsupportedPolicy",
        message,
    }
}

impl SpacePolicy {
    /// Read a `policy` from a request. A `$type` this host does not implement
    /// is `UnsupportedPolicy`, as `createSpace` and `updateSpace` require; a
    /// known one that is malformed is `InvalidRequest`.
    pub fn parse(value: serde_json::Value) -> Result<Self, AppError> {
        let kind = value
            .get("$type")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string();
        match kind.as_str() {
            MEMBERSPACE | MANAGEDSPACE => {}
            PERSONALSPACE => {
                return Err(unsupported(
                    "a space here has a DID of its own that no one signs in as, so a \
                     personalspace policy would let no one read it; use memberspace"
                        .into(),
                ));
            }
            other => {
                return Err(unsupported(format!(
                    "this host implements com.atproto.memberspace.policy and \
                     com.atproto.managedspace.policy, not {other:?}"
                )));
            }
        }
        let policy: Self = serde_json::from_value(value).map_err(|e| AppError::XrpcError {
            status: StatusCode::BAD_REQUEST,
            code: "InvalidRequest",
            message: format!("invalid {kind}: {e}"),
        })?;
        if let Self::Managed(managed) = &policy
            && !managed.managing_app.starts_with("did:")
        {
            return Err(AppError::XrpcError {
                status: StatusCode::BAD_REQUEST,
                code: "InvalidRequest",
                message: "managingApp must be a service identifier: a DID with an optional \
                          fragment"
                    .into(),
            });
        }
        Ok(policy)
    }

    /// The policy's `$type`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Member(_) => MEMBERSPACE,
            Self::Managed(_) => MANAGEDSPACE,
        }
    }

    pub fn to_stored(&self) -> Stored {
        match self {
            Self::Member(m) => Stored {
                read_policy: if m.public_read {
                    Policy::Public
                } else {
                    Policy::MemberList
                },
                write_policy: if m.public_write {
                    Policy::Public
                } else {
                    Policy::MemberList
                },
                app_access: match &m.allowed_apps {
                    Some(allowed) => AppAccess::AllowList {
                        allowed: allowed.clone(),
                    },
                    None => AppAccess::Open,
                },
                publish_policy: m.publish_policy,
                publish_members: m.publish_members,
            },
            Self::Managed(m) => Stored {
                read_policy: Policy::ManagingApp {
                    managing_app: m.managing_app.clone(),
                },
                write_policy: Policy::ManagingApp {
                    managing_app: m.managing_app.clone(),
                },
                app_access: AppAccess::Open,
                publish_policy: m.publish_policy,
                publish_members: false,
            },
        }
    }

    /// The policy a stored space has. A space created before policies existed
    /// may combine a managing app with another read or write policy, or a
    /// managing app with an app allow list; neither is a policy, and the error
    /// says so, so its owner can replace it with `updateSpace`.
    pub fn of(space: &Space) -> Result<Self, AppError> {
        let flag = |name: &str| {
            space
                .config
                .extra
                .get(name)
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        };
        let allowed_apps = match &space.app_access {
            AppAccess::Open => None,
            AppAccess::AllowList { allowed } => Some(allowed.clone()),
        };
        match (&space.read_policy, &space.write_policy) {
            (
                Policy::ManagingApp { managing_app },
                Policy::ManagingApp {
                    managing_app: write_app,
                },
            ) if managing_app == write_app && allowed_apps.is_none() => {
                Ok(Self::Managed(Managedspace {
                    managing_app: managing_app.clone(),
                    publish_policy: flag(PUBLISH_POLICY),
                }))
            }
            (Policy::ManagingApp { .. }, _) | (_, Policy::ManagingApp { .. }) => Err(unsupported(
                "this space's access rules predate space policies and match none; \
                     replace them with updateSpace"
                    .into(),
            )),
            (read, write) => Ok(Self::Member(Memberspace {
                public_read: *read == Policy::Public,
                public_write: *write == Policy::Public,
                allowed_apps,
                publish_policy: flag(PUBLISH_POLICY),
                publish_members: flag(PUBLISH_MEMBERS),
            })),
        }
    }

    /// Store this policy on `space`, replacing its access rules and its
    /// `publish*` flags.
    pub fn apply(&self, space: &mut Space) {
        let stored = self.to_stored();
        space.read_policy = stored.read_policy;
        space.write_policy = stored.write_policy;
        space.app_access = stored.app_access;
        set_flag(&mut space.config, PUBLISH_POLICY, stored.publish_policy);
        set_flag(&mut space.config, PUBLISH_MEMBERS, stored.publish_members);
    }
}

fn set_flag(config: &mut SpaceConfig, name: &str, on: bool) {
    if on {
        config
            .extra
            .insert(name.into(), serde_json::Value::Bool(true));
    } else {
        config.extra.remove(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn space() -> Space {
        Space {
            id: "s".into(),
            did: "did:plc:space".into(),
            authority_did: "did:plc:space".into(),
            creator_did: "did:plc:creator".into(),
            type_nsid: "com.example.forum".into(),
            skey: "main".into(),
            display_name: None,
            description: None,
            read_policy: Policy::MemberList,
            write_policy: Policy::MemberList,
            app_access: AppAccess::Open,
            config: SpaceConfig::default(),
            revision: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn each_policy_round_trips_through_storage() {
        for value in [
            json!({ "$type": MEMBERSPACE }),
            json!({ "$type": MEMBERSPACE, "publicRead": true }),
            json!({ "$type": MEMBERSPACE, "publicWrite": true, "allowedApps": ["https://app.example/client-metadata.json"] }),
            json!({ "$type": MEMBERSPACE, "publishPolicy": true, "publishMembers": true }),
            json!({ "$type": MANAGEDSPACE, "managingApp": "did:web:app.example#forum" }),
            json!({ "$type": MANAGEDSPACE, "managingApp": "did:plc:app", "publishPolicy": true }),
        ] {
            let policy = SpacePolicy::parse(value.clone()).unwrap();
            let mut space = space();
            policy.apply(&mut space);
            assert_eq!(SpacePolicy::of(&space).unwrap(), policy, "{value}");
        }
    }

    #[test]
    fn a_memberspace_serializes_with_its_type_and_defaults() {
        let policy = SpacePolicy::parse(json!({ "$type": MEMBERSPACE })).unwrap();
        assert_eq!(
            serde_json::to_value(&policy).unwrap(),
            json!({
                "$type": MEMBERSPACE,
                "publicRead": false,
                "publicWrite": false,
                "publishPolicy": false,
                "publishMembers": false,
            })
        );
    }

    #[test]
    fn other_policies_are_unsupported() {
        for value in [
            json!({ "$type": PERSONALSPACE }),
            json!({ "$type": "com.atproto.simplespace.defs#memberListPolicy" }),
            json!({}),
        ] {
            let err = SpacePolicy::parse(value.clone()).unwrap_err();
            assert!(
                matches!(
                    err,
                    AppError::XrpcError {
                        code: "UnsupportedPolicy",
                        ..
                    }
                ),
                "{value}: {err:?}"
            );
        }
    }

    #[test]
    fn a_malformed_known_policy_is_an_invalid_request() {
        for value in [
            json!({ "$type": MANAGEDSPACE }),
            json!({ "$type": MANAGEDSPACE, "managingApp": "https://app.example" }),
            json!({ "$type": MEMBERSPACE, "publicRead": "yes" }),
        ] {
            let err = SpacePolicy::parse(value.clone()).unwrap_err();
            assert!(
                matches!(
                    err,
                    AppError::XrpcError {
                        code: "InvalidRequest",
                        ..
                    }
                ),
                "{value}: {err:?}"
            );
        }
    }

    #[test]
    fn mixed_legacy_rules_are_no_policy() {
        let mut mixed = space();
        mixed.read_policy = Policy::ManagingApp {
            managing_app: "did:plc:app".into(),
        };
        assert!(SpacePolicy::of(&mixed).is_err());

        let mut allow_listed = space();
        allow_listed.read_policy = mixed.read_policy.clone();
        allow_listed.write_policy = mixed.read_policy.clone();
        allow_listed.app_access = AppAccess::AllowList {
            allowed: vec!["x".into()],
        };
        assert!(SpacePolicy::of(&allow_listed).is_err());
    }

    #[test]
    fn replacing_a_policy_clears_flags_it_does_not_set() {
        let mut space = space();
        SpacePolicy::parse(json!({ "$type": MEMBERSPACE, "publishMembers": true }))
            .unwrap()
            .apply(&mut space);
        SpacePolicy::parse(json!({ "$type": MANAGEDSPACE, "managingApp": "did:plc:app" }))
            .unwrap()
            .apply(&mut space);
        assert!(!space.config.extra.contains_key("publishMembers"));
    }
}
