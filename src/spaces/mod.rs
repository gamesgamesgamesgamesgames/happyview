pub mod auth;
pub mod client_attestation;
pub mod commit;
pub mod credential;
pub mod db;
pub mod lthash;
pub mod members;
pub mod notifications;
pub mod oplog;
pub mod routes;
pub mod scope;
pub mod simplespace;
pub mod types;

#[cfg(test)]
mod integration_tests;

use crate::error::AppError;
use std::fmt;

/// A parsed `ats://` URI for addressing permissioned data.
///
/// Full form: `ats://<space-did>/<space-type-nsid>/<skey>/<user-did>/<collection-nsid>/<rkey>`
/// Space-only form: `ats://<space-did>/<space-type-nsid>/<skey>`
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SpaceUri {
    pub did: String,
    pub type_nsid: String,
    pub skey: String,
    pub user_did: Option<String>,
    pub collection: Option<String>,
    pub rkey: Option<String>,
}

impl SpaceUri {
    pub fn parse(uri: &str) -> Result<Self, AppError> {
        let stripped = uri
            .strip_prefix("ats://")
            .ok_or_else(|| AppError::BadRequest("SpaceUri must start with ats://".into()))?;

        let parts: Vec<&str> = stripped.split('/').collect();

        if parts.len() < 3 {
            return Err(AppError::BadRequest(
                "SpaceUri requires at least did/type_nsid/skey".into(),
            ));
        }

        if parts[0].is_empty() || parts[1].is_empty() || parts[2].is_empty() {
            return Err(AppError::BadRequest(
                "SpaceUri components must not be empty".into(),
            ));
        }

        let did = parts[0].to_string();
        let type_nsid = parts[1].to_string();
        let skey = parts[2].to_string();

        let (user_did, collection, rkey) = if parts.len() >= 6 {
            (
                Some(parts[3].to_string()),
                Some(parts[4].to_string()),
                Some(parts[5].to_string()),
            )
        } else if parts.len() == 3 {
            (None, None, None)
        } else {
            return Err(AppError::BadRequest(
                "SpaceUri must have 3 components (space) or 6 components (record)".into(),
            ));
        };

        Ok(SpaceUri {
            did,
            type_nsid,
            skey,
            user_did,
            collection,
            rkey,
        })
    }

    pub fn space_uri(&self) -> String {
        format!("ats://{}/{}/{}", self.did, self.type_nsid, self.skey)
    }

    pub fn is_record_uri(&self) -> bool {
        self.user_did.is_some()
    }

    pub fn is_space_uri(&self) -> bool {
        self.user_did.is_none()
    }
}

impl fmt::Display for SpaceUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ats://{}/{}/{}", self.did, self.type_nsid, self.skey)?;
        if let (Some(user), Some(col), Some(rkey)) = (&self.user_did, &self.collection, &self.rkey)
        {
            write!(f, "/{}/{}/{}", user, col, rkey)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_space_uri() {
        let uri = SpaceUri::parse("ats://did:plc:abc123/com.example.forum/main").unwrap();
        assert_eq!(uri.did, "did:plc:abc123");
        assert_eq!(uri.type_nsid, "com.example.forum");
        assert_eq!(uri.skey, "main");
        assert!(uri.is_space_uri());
        assert!(!uri.is_record_uri());
        assert_eq!(uri.user_did, None);
    }

    #[test]
    fn parse_record_uri() {
        let uri = SpaceUri::parse(
            "ats://did:plc:abc123/com.example.forum/main/did:plc:user1/com.example.forum.post/3k2abc",
        )
        .unwrap();
        assert_eq!(uri.did, "did:plc:abc123");
        assert_eq!(uri.type_nsid, "com.example.forum");
        assert_eq!(uri.skey, "main");
        assert_eq!(uri.user_did.as_deref(), Some("did:plc:user1"));
        assert_eq!(uri.collection.as_deref(), Some("com.example.forum.post"));
        assert_eq!(uri.rkey.as_deref(), Some("3k2abc"));
        assert!(uri.is_record_uri());
        assert!(!uri.is_space_uri());
    }

    #[test]
    fn display_space_uri() {
        let uri = SpaceUri {
            did: "did:plc:abc123".into(),
            type_nsid: "com.example.forum".into(),
            skey: "main".into(),
            user_did: None,
            collection: None,
            rkey: None,
        };
        assert_eq!(
            uri.to_string(),
            "ats://did:plc:abc123/com.example.forum/main"
        );
    }

    #[test]
    fn display_record_uri() {
        let uri = SpaceUri {
            did: "did:plc:abc123".into(),
            type_nsid: "com.example.forum".into(),
            skey: "main".into(),
            user_did: Some("did:plc:user1".into()),
            collection: Some("com.example.forum.post".into()),
            rkey: Some("3k2abc".into()),
        };
        assert_eq!(
            uri.to_string(),
            "ats://did:plc:abc123/com.example.forum/main/did:plc:user1/com.example.forum.post/3k2abc"
        );
    }

    #[test]
    fn space_uri_extracts_space_part() {
        let uri = SpaceUri::parse(
            "ats://did:plc:abc123/com.example.forum/main/did:plc:user1/com.example.forum.post/3k2abc",
        )
        .unwrap();
        assert_eq!(
            uri.space_uri(),
            "ats://did:plc:abc123/com.example.forum/main"
        );
    }

    #[test]
    fn reject_at_scheme() {
        let result = SpaceUri::parse("at://did:plc:abc123/com.example.forum/main");
        assert!(result.is_err());
    }

    #[test]
    fn reject_too_few_components() {
        let result = SpaceUri::parse("ats://did:plc:abc123/com.example.forum");
        assert!(result.is_err());
    }

    #[test]
    fn reject_wrong_component_count() {
        let result = SpaceUri::parse("ats://did:plc:abc123/com.example.forum/main/did:plc:user1");
        assert!(result.is_err());
    }

    #[test]
    fn reject_empty_components() {
        let result = SpaceUri::parse("ats:///com.example.forum/main");
        assert!(result.is_err());
    }

    #[test]
    fn roundtrip_parse_display() {
        let original = "ats://did:plc:abc123/com.example.forum/main";
        let uri = SpaceUri::parse(original).unwrap();
        assert_eq!(uri.to_string(), original);

        let original_record = "ats://did:plc:abc123/com.example.forum/main/did:plc:user1/com.example.forum.post/3k2abc";
        let uri = SpaceUri::parse(original_record).unwrap();
        assert_eq!(uri.to_string(), original_record);
    }
}
