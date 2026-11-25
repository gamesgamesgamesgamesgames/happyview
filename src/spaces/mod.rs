pub mod auth;
pub mod authority;
pub mod car;
pub mod cid_backfill;
pub mod client_attestation;
pub mod commit;
pub mod credential;
pub mod db;
pub mod delegation_key;
pub mod describe;
pub mod host_mode;
pub mod http_signature;
pub mod lifecycle;
pub mod lthash;
pub mod members;
pub mod memberspace;
pub mod native_client;
pub mod native_sync;
pub mod nested;
pub mod notifications;
pub mod oplog;
pub mod pds_support;
pub mod policy;
pub mod publish;
pub mod rebuild;
pub mod retired;
pub mod routes;
pub mod scope;
pub mod service;
pub mod simplespace;
pub mod space_mac;
pub mod star_repo;
pub mod types;
pub mod writers;

#[cfg(test)]
mod integration_tests;

use crate::error::AppError;
use std::fmt;

/// A parsed URI addressing permissioned data: a space, or a record in one.
///
/// Three written forms are read:
/// - `at:space/<space-did>/<space-type-nsid>/<skey>[/<user-did>/<collection-nsid>/<rkey>]`,
///   Proposal 0016's form since Nov 24, 2026;
/// - `at://<space-did>/space/<space-type-nsid>/<skey>[/…]`, its form before;
/// - `ats://<space-did>/<space-type-nsid>/<skey>[/…]`, HappyView's earliest.
///
/// HappyView stores and compares the `at://…/space/…` form ([`fmt::Display`]),
/// and writes a URI out in the form a request used ([`UriForm`]).
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
        let stripped = if let Some(rest) = uri.strip_prefix(AT_SPACE_PREFIX) {
            // at:space/did/type/skey[/…] reads as at://did/space/type/skey[/…].
            let (did, path) = rest.split_once('/').ok_or_else(|| {
                AppError::BadRequest("at:space/ URI requires at least did/type/skey".into())
            })?;
            if !did.starts_with("did:") {
                return Err(AppError::BadRequest(
                    "an at:space/ URI names its space by DID".into(),
                ));
            }
            normalized = format!("at://{did}/space/{path}");
            normalized
                .strip_prefix("at://")
                .expect("just constructed with at:// prefix")
        } else if let Some(ats_rest) = uri.strip_prefix("ats://") {
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

    /// This URI written in `form`.
    pub fn to_form(&self, form: UriForm) -> String {
        match form {
            UriForm::Legacy => self.to_string(),
            UriForm::AtSpace => {
                let mut out = format!(
                    "{AT_SPACE_PREFIX}{}/{}/{}",
                    self.did, self.type_nsid, self.skey
                );
                if let (Some(user), Some(col), Some(rkey)) =
                    (&self.user_did, &self.collection, &self.rkey)
                {
                    out.push_str(&format!("/{user}/{col}/{rkey}"));
                }
                out
            }
        }
    }

    pub fn is_record_uri(&self) -> bool {
        self.user_did.is_some()
    }

    pub fn is_space_uri(&self) -> bool {
        self.user_did.is_none()
    }
}

/// The prefix of Proposal 0016's space URIs since Nov 24, 2026.
pub const AT_SPACE_PREFIX: &str = "at:space/";

/// The written form of a space or space-record URI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UriForm {
    /// `at:space/<did>/<type>/<skey>[/…]`: Proposal 0016's form since Nov 24,
    /// 2026, and what HappyView writes when nothing says otherwise.
    AtSpace,
    /// `at://<did>/space/<type>/<skey>[/…]`: the form before, which older
    /// clients send and read (`ats://` requests are answered in it too).
    Legacy,
}

impl UriForm {
    /// The form `uri` is written in.
    pub fn of(uri: &str) -> Self {
        if uri.starts_with(AT_SPACE_PREFIX) {
            Self::AtSpace
        } else {
            Self::Legacy
        }
    }

    /// Write `uri`, a space or space-record URI in any form, in this form. A
    /// string that isn't one is returned unchanged.
    pub fn write(self, uri: &str) -> String {
        match SpaceUri::parse(uri) {
            Ok(parsed) => parsed.to_form(self),
            Err(_) => uri.to_string(),
        }
    }
}

/// The form a new space is named in when the request names no space to copy
/// the form from (`space` on createSpace and listSpaces). `uri`, the Oct 29
/// name, stays in the `at://` form either way.
pub const NEW_SPACE_FORM: UriForm = UriForm::AtSpace;

/// Whether a string names a space (or a record in one) rather than an
/// account, in any of the forms [`SpaceUri::parse`] reads.
pub fn is_space_reference(s: &str) -> bool {
    s.starts_with(AT_SPACE_PREFIX) || s.starts_with("at://") || s.starts_with("ats://")
}

/// `uri` in the form HappyView stores and compares (`at://…/space/…`). A
/// string that isn't a space URI is returned unchanged.
pub fn canonical_uri(uri: &str) -> String {
    UriForm::Legacy.write(uri)
}

/// Whether two space or space-record URIs name the same thing, whatever form
/// each is written in.
pub fn same_space_uri(a: &str, b: &str) -> bool {
    match (SpaceUri::parse(a), SpaceUri::parse(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
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
    fn the_at_space_form_reads_as_the_same_uri() {
        let new = SpaceUri::parse("at:space/did:plc:abc123/com.example.forum/main").unwrap();
        let old = SpaceUri::parse("at://did:plc:abc123/space/com.example.forum/main").unwrap();
        assert_eq!(new, old);
        let record = SpaceUri::parse(
            "at:space/did:plc:abc123/com.example.forum/main/did:plc:user1/com.example.post/3k",
        )
        .unwrap();
        assert_eq!(record.user_did.as_deref(), Some("did:plc:user1"));
        assert_eq!(record.collection.as_deref(), Some("com.example.post"));
        assert_eq!(record.rkey.as_deref(), Some("3k"));
        assert_eq!(
            record.to_string(),
            "at://did:plc:abc123/space/com.example.forum/main/did:plc:user1/com.example.post/3k"
        );
    }

    #[test]
    fn the_at_space_form_is_refused_when_malformed() {
        for bad in [
            "at:space/",
            "at:space/did:plc:abc123",
            "at:space/did:plc:abc123/com.example.forum",
            "at:space/alice.example/com.example.forum/main",
            "at:space/did:plc:abc123/com.example.forum/main/did:plc:user1",
            "at:space//com.example.forum/main",
        ] {
            assert!(SpaceUri::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_uri_is_written_in_the_form_asked() {
        let space = "at://did:plc:abc123/space/com.example.forum/main";
        let new = "at:space/did:plc:abc123/com.example.forum/main";
        assert_eq!(UriForm::AtSpace.write(space), new);
        assert_eq!(UriForm::Legacy.write(new), space);
        assert_eq!(
            UriForm::Legacy.write("ats://did:plc:abc123/com.example.forum/main"),
            space
        );
        let record =
            "at://did:plc:abc123/space/com.example.forum/main/did:plc:u/com.example.post/k";
        assert_eq!(
            UriForm::AtSpace.write(record),
            "at:space/did:plc:abc123/com.example.forum/main/did:plc:u/com.example.post/k"
        );
        assert_eq!(UriForm::AtSpace.write("did:plc:someone"), "did:plc:someone");
        assert_eq!(UriForm::of(new), UriForm::AtSpace);
        assert_eq!(UriForm::of(space), UriForm::Legacy);
        assert_eq!(canonical_uri(new), space);
        assert!(same_space_uri(new, space));
        assert!(!same_space_uri(
            new,
            "at://did:plc:abc123/space/com.example.forum/other"
        ));
        assert!(is_space_reference(new) && is_space_reference(space));
        assert!(!is_space_reference("did:plc:abc123"));
    }

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
