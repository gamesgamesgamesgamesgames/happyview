pub mod auth;
pub mod car;
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

/// A parsed `at://` URI for addressing permissioned data.
///
/// Full form: `at://<space-did>/space/<space-type-nsid>/<skey>/<user-did>/<collection-nsid>/<rkey>`
/// Space-only form: `at://<space-did>/space/<space-type-nsid>/<skey>`
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
        // Rewrite legacy ats:// URIs (ats://did/type/skey) to at://did/space/type/skey
        let normalized;
        let stripped = if let Some(ats_rest) = uri.strip_prefix("ats://") {
            let ats_parts: Vec<&str> = ats_rest.split('/').collect();
            if ats_parts.len() >= 3 {
                normalized = format!("at://{}/space/{}", ats_parts[0], ats_parts[1..].join("/"));
                normalized
                    .strip_prefix("at://")
                    .expect("just constructed with at:// prefix")
            } else {
                return Err(AppError::BadRequest(
                    "ats:// URI requires at least did/type/skey".into(),
                ));
            }
        } else {
            uri.strip_prefix("at://")
                .ok_or_else(|| AppError::BadRequest("SpaceUri must start with at://".into()))?
        };

        let parts: Vec<&str> = stripped.split('/').collect();

        // Must have at least: did/space/type_nsid/skey (4 segments)
        if parts.len() < 4 {
            return Err(AppError::BadRequest(
                "SpaceUri requires at least did/space/type_nsid/skey".into(),
            ));
        }

        if parts[1] != "space" {
            return Err(AppError::BadRequest(
                "SpaceUri must have 'space' as the second path segment".into(),
            ));
        }

        if parts[0].is_empty() || parts[2].is_empty() || parts[3].is_empty() {
            return Err(AppError::BadRequest(
                "SpaceUri components must not be empty".into(),
            ));
        }

        let did = parts[0].to_string();
        let type_nsid = parts[2].to_string();
        let skey = parts[3].to_string();

        let (user_did, collection, rkey) = if parts.len() == 7 {
            if parts[4].is_empty() || parts[5].is_empty() || parts[6].is_empty() {
                return Err(AppError::BadRequest(
                    "SpaceUri record components must not be empty".into(),
                ));
            }
            (
                Some(parts[4].to_string()),
                Some(parts[5].to_string()),
                Some(parts[6].to_string()),
            )
        } else if parts.len() == 4 {
            (None, None, None)
        } else {
            return Err(AppError::BadRequest(
                "SpaceUri must have 4 components (space) or 7 components (record)".into(),
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
        format!("at://{}/space/{}/{}", self.did, self.type_nsid, self.skey)
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
        write!(
            f,
            "at://{}/space/{}/{}",
            self.did, self.type_nsid, self.skey
        )?;
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
        let uri = SpaceUri::parse("at://did:plc:abc123/space/com.example.forum/main").unwrap();
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
            "at://did:plc:abc123/space/com.example.forum/main/did:plc:user1/com.example.forum.post/3k2abc",
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
            "at://did:plc:abc123/space/com.example.forum/main"
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
            "at://did:plc:abc123/space/com.example.forum/main/did:plc:user1/com.example.forum.post/3k2abc"
        );
    }

    #[test]
    fn space_uri_extracts_space_part() {
        let uri = SpaceUri::parse(
            "at://did:plc:abc123/space/com.example.forum/main/did:plc:user1/com.example.forum.post/3k2abc",
        )
        .unwrap();
        assert_eq!(
            uri.space_uri(),
            "at://did:plc:abc123/space/com.example.forum/main"
        );
    }

    #[test]
    fn rewrite_ats_space_uri() {
        let uri = SpaceUri::parse("ats://did:plc:abc123/com.example.forum/main").unwrap();
        assert_eq!(uri.did, "did:plc:abc123");
        assert_eq!(uri.type_nsid, "com.example.forum");
        assert_eq!(uri.skey, "main");
        assert!(uri.is_space_uri());
        assert_eq!(
            uri.to_string(),
            "at://did:plc:abc123/space/com.example.forum/main"
        );
    }

    #[test]
    fn rewrite_ats_record_uri() {
        let uri = SpaceUri::parse(
            "ats://did:plc:abc123/com.example.forum/main/did:plc:user1/com.example.forum.post/3k2abc",
        )
        .unwrap();
        assert_eq!(uri.did, "did:plc:abc123");
        assert_eq!(uri.type_nsid, "com.example.forum");
        assert_eq!(uri.skey, "main");
        assert_eq!(uri.user_did.as_deref(), Some("did:plc:user1"));
        assert!(uri.is_record_uri());
    }

    #[test]
    fn reject_ats_too_few_segments() {
        let result = SpaceUri::parse("ats://did:plc:abc123/com.example.forum");
        assert!(result.is_err());
    }

    #[test]
    fn reject_missing_space_segment() {
        let result = SpaceUri::parse("at://did:plc:abc123/com.example.forum/main");
        assert!(result.is_err());
    }

    #[test]
    fn reject_too_few_components() {
        let result = SpaceUri::parse("at://did:plc:abc123/space/com.example.forum");
        assert!(result.is_err());
    }

    #[test]
    fn reject_wrong_component_count() {
        let result =
            SpaceUri::parse("at://did:plc:abc123/space/com.example.forum/main/did:plc:user1");
        assert!(result.is_err());
    }

    #[test]
    fn reject_empty_components() {
        let result = SpaceUri::parse("at:///space/com.example.forum/main");
        assert!(result.is_err());
    }

    #[test]
    fn roundtrip_parse_display() {
        let original = "at://did:plc:abc123/space/com.example.forum/main";
        let uri = SpaceUri::parse(original).unwrap();
        assert_eq!(uri.to_string(), original);

        let original_record = "at://did:plc:abc123/space/com.example.forum/main/did:plc:user1/com.example.forum.post/3k2abc";
        let uri = SpaceUri::parse(original_record).unwrap();
        assert_eq!(uri.to_string(), original_record);
    }
}
