use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceAccess {
    Read,
    ReadSelf,
    Write,
}

impl SpaceAccess {
    pub fn as_str(&self) -> &'static str {
        match self {
            SpaceAccess::Read => "read",
            SpaceAccess::ReadSelf => "read_self",
            SpaceAccess::Write => "write",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "read" => Some(SpaceAccess::Read),
            "read_self" => Some(SpaceAccess::ReadSelf),
            "write" => Some(SpaceAccess::Write),
            _ => None,
        }
    }

    pub fn can_write(&self) -> bool {
        matches!(self, SpaceAccess::Write)
    }

    pub fn can_read(&self) -> bool {
        true
    }
}

impl fmt::Display for SpaceAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MintPolicy {
    #[serde(rename = "member-list")]
    MemberList,
    #[serde(rename = "public")]
    Public,
    #[serde(rename = "managing-app")]
    ManagingApp,
}

impl MintPolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            MintPolicy::MemberList => "member-list",
            MintPolicy::Public => "public",
            MintPolicy::ManagingApp => "managing-app",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "member-list" => Some(MintPolicy::MemberList),
            "public" => Some(MintPolicy::Public),
            "managing-app" => Some(MintPolicy::ManagingApp),
            _ => None,
        }
    }
}

impl fmt::Display for MintPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AppAccess {
    #[default]
    Open,
    AllowList {
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
    pub sig: Option<Vec<u8>>,
    pub mac: Option<Vec<u8>>,
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
    pub mint_policy: MintPolicy,
    pub app_access: AppAccess,
    pub managing_app_did: Option<String>,
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
    pub access: SpaceAccess,
    pub is_delegation: bool,
    pub granted_by: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedMember {
    pub did: String,
    pub access: SpaceAccess,
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
    pub access: SpaceAccess,
    pub max_uses: Option<i64>,
    pub uses: i64,
    pub expires_at: Option<String>,
    pub revoked: bool,
    pub created_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn space_access_roundtrip() {
        assert_eq!(SpaceAccess::parse("read"), Some(SpaceAccess::Read));
        assert_eq!(SpaceAccess::parse("read_self"), Some(SpaceAccess::ReadSelf));
        assert_eq!(SpaceAccess::parse("write"), Some(SpaceAccess::Write));
        assert_eq!(SpaceAccess::parse("admin"), None);

        assert_eq!(SpaceAccess::Read.as_str(), "read");
        assert_eq!(SpaceAccess::ReadSelf.as_str(), "read_self");
        assert_eq!(SpaceAccess::Write.as_str(), "write");
    }

    #[test]
    fn space_access_permissions() {
        assert!(SpaceAccess::Read.can_read());
        assert!(!SpaceAccess::Read.can_write());
        assert!(SpaceAccess::ReadSelf.can_read());
        assert!(!SpaceAccess::ReadSelf.can_write());
        assert!(SpaceAccess::Write.can_read());
        assert!(SpaceAccess::Write.can_write());
    }

    #[test]
    fn mint_policy_roundtrip() {
        assert_eq!(
            MintPolicy::parse("member-list"),
            Some(MintPolicy::MemberList)
        );
        assert_eq!(MintPolicy::parse("public"), Some(MintPolicy::Public));
        assert_eq!(
            MintPolicy::parse("managing-app"),
            Some(MintPolicy::ManagingApp)
        );
        assert_eq!(MintPolicy::parse("invalid"), None);

        assert_eq!(MintPolicy::MemberList.as_str(), "member-list");
        assert_eq!(MintPolicy::Public.as_str(), "public");
        assert_eq!(MintPolicy::ManagingApp.as_str(), "managing-app");
    }

    #[test]
    fn mint_policy_serialization() {
        let json = serde_json::to_string(&MintPolicy::MemberList).unwrap();
        assert_eq!(json, "\"member-list\"");
        let parsed: MintPolicy = serde_json::from_str("\"public\"").unwrap();
        assert_eq!(parsed, MintPolicy::Public);
    }

    #[test]
    fn app_access_open_serialization() {
        let access = AppAccess::Open;
        let json = serde_json::to_string(&access).unwrap();
        assert_eq!(json, r#"{"type":"open"}"#);
        let parsed: AppAccess = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, AppAccess::Open));
    }

    #[test]
    fn app_access_allowlist_serialization() {
        let access = AppAccess::AllowList {
            allowed: vec!["https://app.example.com/client-metadata.json".into()],
        };
        let json = serde_json::to_string(&access).unwrap();
        let parsed: AppAccess = serde_json::from_str(&json).unwrap();
        match parsed {
            AppAccess::AllowList { allowed } => {
                assert_eq!(
                    allowed,
                    vec!["https://app.example.com/client-metadata.json"]
                );
            }
            _ => panic!("expected AllowList"),
        }
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
    fn space_access_serialization() {
        let json = serde_json::to_string(&SpaceAccess::Read).unwrap();
        assert_eq!(json, "\"read\"");

        let json = serde_json::to_string(&SpaceAccess::ReadSelf).unwrap();
        assert_eq!(json, "\"read_self\"");

        let json = serde_json::to_string(&SpaceAccess::Write).unwrap();
        assert_eq!(json, "\"write\"");

        let parsed: SpaceAccess = serde_json::from_str("\"read\"").unwrap();
        assert_eq!(parsed, SpaceAccess::Read);

        let parsed: SpaceAccess = serde_json::from_str("\"read_self\"").unwrap();
        assert_eq!(parsed, SpaceAccess::ReadSelf);

        let parsed: SpaceAccess = serde_json::from_str("\"write\"").unwrap();
        assert_eq!(parsed, SpaceAccess::Write);
    }
}
