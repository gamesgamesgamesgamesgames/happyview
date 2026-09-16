use serde::{Deserialize, Serialize};
use std::fmt;

/// A member's access within a space.
///
/// `read` and `write` are the spec's independent member-list booleans and the
/// only two that appear on the wire. `read_self` is HappyView-local: the spec
/// expresses own-records-only as an OAuth *action*, not a membership level, so
/// this flag keeps members that held own-records-only access before membership
/// became two booleans. Folding it into `read` would promote them from
/// own-records-only to whole-space reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberAccess {
    pub read: bool,
    pub write: bool,
    #[serde(skip)]
    pub read_self: bool,
}

impl MemberAccess {
    pub const READ: Self = Self {
        read: true,
        write: false,
        read_self: false,
    };
    pub const WRITE: Self = Self {
        read: true,
        write: true,
        read_self: false,
    };
    pub const READ_SELF: Self = Self {
        read: true,
        write: false,
        read_self: true,
    };

    pub fn can_read(&self) -> bool {
        self.read
    }

    pub fn can_write(&self) -> bool {
        self.write
    }

    /// Whether reads are confined to the member's own repo.
    pub fn restricted_to_own_records(&self) -> bool {
        self.read_self
    }

    /// Compact text form, for columns that store access as a single string.
    ///
    /// Used by the invite table, which is a HappyView extension rather than a
    /// spec surface and so keeps a single column. The member list itself stores
    /// the three flags separately, matching the lexicon.
    pub fn as_wire_str(&self) -> &'static str {
        match (self.read, self.write, self.read_self) {
            (_, true, _) => "write",
            (true, false, true) => "read_self",
            (true, false, false) => "read",
            (false, false, _) => "none",
        }
    }

    pub fn parse_wire(s: &str) -> Option<Self> {
        match s {
            "write" => Some(Self::WRITE),
            "read" => Some(Self::READ),
            "read_self" => Some(Self::READ_SELF),
            "none" => Some(Self {
                read: false,
                write: false,
                read_self: false,
            }),
            _ => None,
        }
    }

    /// Merge memberships reached via multiple paths (direct + delegation).
    ///
    /// The most permissive value wins on each axis independently, and a single
    /// unrestricted grant lifts `read_self`. Otherwise a delegated whole-space
    /// read would stay clamped to the member's own repo.
    pub fn union(self, other: Self) -> Self {
        Self {
            read: self.read || other.read,
            write: self.write || other.write,
            read_self: self.read_self && other.read_self,
        }
    }
}

impl fmt::Display for MemberAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_wire_str())
    }
}

/// How a space authority decides whether to authorize a user.
///
/// An open union at the schema layer, but a host MUST reject variants it does
/// not implement at createSpace/updateSpace time rather than store a policy it
/// cannot enforce. A closed Rust enum fails to deserialize any unknown `$type`
/// tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "$type")]
pub enum Policy {
    #[serde(rename = "com.atproto.simplespace.defs#publicPolicy")]
    Public,
    #[serde(rename = "com.atproto.simplespace.defs#memberListPolicy")]
    MemberList,
    #[serde(rename = "com.atproto.simplespace.defs#managingAppPolicy")]
    #[serde(rename_all = "camelCase")]
    ManagingApp {
        /// Service identifier: a DID with an optional fragment.
        managing_app: String,
    },
}

impl Default for Policy {
    /// Member-list, never public: a policy we could not read must not open a
    /// space up.
    fn default() -> Self {
        Policy::MemberList
    }
}

impl Policy {
    /// The managing app this policy defers to, if any.
    pub fn managing_app(&self) -> Option<&str> {
        match self {
            Policy::ManagingApp { managing_app } => Some(managing_app),
            _ => None,
        }
    }
}

/// How a space authority decides whether to authorize a requesting app.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "$type")]
pub enum AppAccess {
    #[default]
    #[serde(rename = "com.atproto.simplespace.defs#open")]
    Open,
    #[serde(rename = "com.atproto.simplespace.defs#allowList")]
    AllowList {
        /// OAuth client IDs permitted to access the space, evaluated against
        /// the *attested* client_id.
        allowed: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OplogAction {
    Create,
    Update,
    Delete,
}

impl OplogAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            OplogAction::Create => "create",
            OplogAction::Update => "update",
            OplogAction::Delete => "delete",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "create" => Some(OplogAction::Create),
            "update" => Some(OplogAction::Update),
            "delete" => Some(OplogAction::Delete),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OplogEntry {
    pub id: String,
    pub space_id: String,
    pub author_did: String,
    pub rev: String,
    pub idx: i32,
    pub action: OplogAction,
    pub collection: String,
    pub rkey: String,
    pub cid: Option<String>,
    pub prev: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct RepoState {
    pub id: String,
    pub space_id: String,
    pub author_did: String,
    pub lthash_state: Vec<u8>,
    pub rev: Option<String>,
    pub hash: Option<Vec<u8>>,
    pub ikm: Option<Vec<u8>>,
    /// `sign(context)` over (space, author, rev, ikm). NULL for rows written
    /// without a signature; the rebuild re-mints those.
    pub sig: Option<Vec<u8>>,
    pub mac: Option<Vec<u8>>,
    /// Which host is authoritative for this repo.
    pub host_mode: crate::spaces::host_mode::HostMode,
    /// Last rev consumed from the PDS in native mode, for incremental
    /// `listRepoOps`.
    pub sync_cursor: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Space {
    pub id: String,
    pub did: String,
    pub authority_did: String,
    pub creator_did: String,
    #[serde(rename = "type")]
    pub type_nsid: String,
    pub skey: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    /// Gates whether a space credential is minted for a user.
    pub read_policy: Policy,
    /// Gates whether the authority tracks a writer in listRepos and forwards
    /// their notifyWrite. Independent of `read_policy`.
    pub write_policy: Policy,
    pub app_access: AppAccess,
    pub config: SpaceConfig,
    pub revision: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpaceConfig {
    #[serde(default)]
    pub membership_public: bool,
    #[serde(default)]
    pub records_public: bool,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceMember {
    pub id: String,
    pub space_id: String,
    pub did: String,
    pub access: MemberAccess,
    pub is_delegation: bool,
    pub granted_by: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedMember {
    pub did: String,
    /// Flattened, because the lexicon's member is `{did, read, write}`: the
    /// booleans sit alongside the DID rather than nested under a level.
    #[serde(flatten)]
    pub access: MemberAccess,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceRecord {
    pub uri: String,
    pub space_id: String,
    pub author_did: String,
    pub collection: String,
    pub rkey: String,
    pub record: serde_json::Value,
    pub cid: String,
    pub indexed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotifyRegistration {
    pub id: String,
    pub space_id: String,
    pub author_did: Option<String>,
    pub endpoint: String,
    pub registered_by: String,
    pub expires_at: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceInvite {
    pub id: String,
    pub space_id: String,
    pub token_hash: String,
    pub created_by: String,
    pub access: MemberAccess,
    pub max_uses: Option<i64>,
    pub uses: i64,
    pub expires_at: Option<String>,
    pub revoked: bool,
    pub created_at: String,
}

#[cfg(test)]
mod policy_tests {
    use super::*;

    // The wire shape is a lexicon open union: a `$type` tag naming the variant,
    // with any payload inline. These tests pin it, because a tag mismatch
    // surfaces only as an unparseable policy, with no other error.

    #[test]
    fn policy_serializes_with_the_lexicon_type_tag() {
        assert_eq!(
            serde_json::to_value(Policy::MemberList).unwrap(),
            serde_json::json!({ "$type": "com.atproto.simplespace.defs#memberListPolicy" })
        );
        assert_eq!(
            serde_json::to_value(Policy::Public).unwrap(),
            serde_json::json!({ "$type": "com.atproto.simplespace.defs#publicPolicy" })
        );
    }

    #[test]
    fn managing_app_policy_nests_the_app_inside_the_variant() {
        // The app is part of the policy value, not a sibling field. Bulletin
        // sends this shape to createSpace.
        let p = Policy::ManagingApp {
            managing_app: "did:web:example.com#forum".into(),
        };
        assert_eq!(
            serde_json::to_value(&p).unwrap(),
            serde_json::json!({
                "$type": "com.atproto.simplespace.defs#managingAppPolicy",
                "managingApp": "did:web:example.com#forum"
            })
        );
    }

    #[test]
    fn policy_round_trips_every_variant() {
        for p in [
            Policy::Public,
            Policy::MemberList,
            Policy::ManagingApp {
                managing_app: "did:web:x#f".into(),
            },
        ] {
            let json = serde_json::to_string(&p).unwrap();
            let back: Policy = serde_json::from_str(&json).unwrap();
            assert_eq!(p, back);
        }
    }

    #[test]
    fn unknown_policy_variant_is_rejected() {
        let raw = serde_json::json!({ "$type": "com.example.defs#bespokePolicy" });
        assert!(serde_json::from_value::<Policy>(raw).is_err());
    }

    #[test]
    fn managing_app_policy_requires_the_app() {
        let raw = serde_json::json!({ "$type": "com.atproto.simplespace.defs#managingAppPolicy" });
        assert!(serde_json::from_value::<Policy>(raw).is_err());
    }

    #[test]
    fn managing_app_is_only_exposed_by_the_variant_that_has_one() {
        assert_eq!(Policy::Public.managing_app(), None);
        assert_eq!(Policy::MemberList.managing_app(), None);
        assert_eq!(
            Policy::ManagingApp {
                managing_app: "did:web:x#f".into()
            }
            .managing_app(),
            Some("did:web:x#f")
        );
    }

    #[test]
    fn the_default_policy_is_member_list() {
        assert_eq!(Policy::default(), Policy::MemberList);
    }

    #[test]
    fn app_access_uses_the_lexicon_type_tag() {
        assert_eq!(
            serde_json::to_value(AppAccess::Open).unwrap(),
            serde_json::json!({ "$type": "com.atproto.simplespace.defs#open" })
        );
        assert_eq!(
            serde_json::to_value(AppAccess::AllowList {
                allowed: vec!["https://app".into()]
            })
            .unwrap(),
            serde_json::json!({
                "$type": "com.atproto.simplespace.defs#allowList",
                "allowed": ["https://app"]
            })
        );
    }

    #[test]
    fn app_access_round_trips_and_rejects_unknown_variants() {
        let raw = serde_json::json!({ "$type": "com.example.defs#bespokeAccess" });
        assert!(serde_json::from_value::<AppAccess>(raw).is_err());

        let al = AppAccess::AllowList {
            allowed: vec!["a".into()],
        };
        let back: AppAccess = serde_json::from_str(&serde_json::to_string(&al).unwrap()).unwrap();
        assert_eq!(al, back);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_access_wire_form_roundtrip() {
        for s in ["read", "read_self", "write", "none"] {
            let parsed = MemberAccess::parse_wire(s).expect("parses");
            assert_eq!(parsed.as_wire_str(), s, "wire form must round-trip");
        }
        assert_eq!(MemberAccess::parse_wire("admin"), None);
    }

    #[test]
    fn member_access_serializes_only_the_spec_booleans() {
        let json = serde_json::to_value(MemberAccess::READ_SELF).unwrap();
        assert_eq!(json, serde_json::json!({ "read": true, "write": false }));
    }

    #[test]
    fn space_access_permissions() {
        assert!(MemberAccess::READ.can_read());
        assert!(!MemberAccess::READ.can_write());
        assert!(MemberAccess::READ_SELF.can_read());
        assert!(!MemberAccess::READ_SELF.can_write());
        assert!(MemberAccess::WRITE.can_read());
        assert!(MemberAccess::WRITE.can_write());
    }

    #[test]
    fn read_and_write_are_independent_axes() {
        let write_only = MemberAccess {
            read: false,
            write: true,
            read_self: false,
        };
        assert!(!write_only.can_read());
        assert!(write_only.can_write());
    }

    #[test]
    fn oplog_action_roundtrip() {
        assert_eq!(OplogAction::parse("create"), Some(OplogAction::Create));
        assert_eq!(OplogAction::parse("update"), Some(OplogAction::Update));
        assert_eq!(OplogAction::parse("delete"), Some(OplogAction::Delete));
        assert_eq!(OplogAction::parse("invalid"), None);
    }

    #[test]
    fn space_config_defaults() {
        let config: SpaceConfig = serde_json::from_str("{}").unwrap();
        assert!(!config.membership_public);
        assert!(!config.records_public);
    }

    #[test]
    fn space_config_with_extra_fields() {
        let config: SpaceConfig =
            serde_json::from_str(r#"{"membership_public": true, "custom_field": 42}"#).unwrap();
        assert!(config.membership_public);
        assert!(!config.records_public);
        assert_eq!(config.extra.get("custom_field").unwrap(), &42);
    }

    #[test]
    fn member_access_serializes_as_the_spec_member_shape() {
        assert_eq!(
            serde_json::to_value(MemberAccess::READ).unwrap(),
            serde_json::json!({ "read": true, "write": false })
        );
        assert_eq!(
            serde_json::to_value(MemberAccess::WRITE).unwrap(),
            serde_json::json!({ "read": true, "write": true })
        );

        let parsed: MemberAccess =
            serde_json::from_value(serde_json::json!({ "read": true, "write": true })).unwrap();
        assert_eq!(parsed, MemberAccess::WRITE);
    }
}
