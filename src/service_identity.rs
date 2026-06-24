use crate::db::{DatabaseBackend, adapt_sql};
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use sqlx::AnyPool;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum IdentityMode {
    DidWeb,
    DidPlc,
    AttachAccount,
    NotExposed,
}

impl IdentityMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DidWeb => "did_web",
            Self::DidPlc => "did_plc",
            Self::AttachAccount => "attach_account",
            Self::NotExposed => "not_exposed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "did_web" => Some(Self::DidWeb),
            "did_plc" => Some(Self::DidPlc),
            "attach_account" => Some(Self::AttachAccount),
            "not_exposed" => Some(Self::NotExposed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceIdentity {
    pub mode: IdentityMode,
    pub did: Option<String>,
    pub signing_key_enc: Option<String>,
    pub setup_complete: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetupStatus {
    pub identity_mode: Option<IdentityMode>,
    pub identity_configured: bool,
    pub plc_verified: bool,
    pub setup_complete: bool,
}

type ServiceIdentityRow = (String, Option<String>, Option<String>, i32, String, String);

fn parse_row(r: ServiceIdentityRow) -> Result<ServiceIdentity, AppError> {
    let mode = IdentityMode::parse(&r.0)
        .ok_or_else(|| AppError::Internal(format!("invalid identity mode: {}", r.0)))?;
    Ok(ServiceIdentity {
        mode,
        did: r.1,
        signing_key_enc: r.2,
        setup_complete: r.3 != 0,
        created_at: r.4,
        updated_at: r.5,
    })
}

/// Fetch the service identity row (id = 1), if it exists.
pub async fn get_identity(
    db: &AnyPool,
    backend: DatabaseBackend,
) -> Result<Option<ServiceIdentity>, AppError> {
    let sql = adapt_sql(
        "SELECT mode, did, signing_key_enc, CAST(setup_complete AS INTEGER), created_at, updated_at FROM service_identity WHERE id = 1",
        backend,
    );

    let row: Option<ServiceIdentityRow> = sqlx::query_as(&sql)
        .fetch_optional(db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to get service identity: {e}")))?;

    row.map(parse_row).transpose()
}

/// Derive setup status from the current identity row.
pub async fn get_setup_status(
    db: &AnyPool,
    backend: DatabaseBackend,
) -> Result<SetupStatus, AppError> {
    let identity = get_identity(db, backend).await?;

    match identity {
        None => Ok(SetupStatus {
            identity_mode: None,
            identity_configured: false,
            plc_verified: false,
            setup_complete: false,
        }),
        Some(id) => {
            let plc_verified = matches!(id.mode, IdentityMode::DidPlc) && id.setup_complete;
            let identity_configured = match id.mode {
                IdentityMode::DidWeb => id.signing_key_enc.is_some(),
                _ => id.did.is_some(),
            };
            let setup_complete = id.setup_complete;
            let identity_mode = Some(id.mode);
            Ok(SetupStatus {
                identity_mode,
                identity_configured,
                plc_verified,
                setup_complete,
            })
        }
    }
}

/// Insert or update the service identity row (always resets setup_complete to FALSE).
#[allow(clippy::too_many_arguments)]
pub async fn upsert_identity(
    db: &AnyPool,
    backend: DatabaseBackend,
    mode: &IdentityMode,
    did: Option<&str>,
    signing_key_enc: Option<&str>,
    rotation_key_enc: Option<&str>,
    attached_account_did: Option<&str>,
) -> Result<(), AppError> {
    let now = chrono::Utc::now().to_rfc3339();
    let sql = adapt_sql(
        "INSERT INTO service_identity (id, mode, did, signing_key_enc, rotation_key_enc, attached_account_did, setup_complete, created_at, updated_at)
         VALUES (1, ?, ?, ?, ?, ?, FALSE, ?, ?)
         ON CONFLICT (id) DO UPDATE SET
             mode = excluded.mode,
             did = excluded.did,
             signing_key_enc = excluded.signing_key_enc,
             rotation_key_enc = excluded.rotation_key_enc,
             attached_account_did = excluded.attached_account_did,
             setup_complete = excluded.setup_complete,
             updated_at = excluded.updated_at",
        backend,
    );

    sqlx::query(&sql)
        .bind(mode.as_str())
        .bind(did)
        .bind(signing_key_enc)
        .bind(rotation_key_enc)
        .bind(attached_account_did)
        .bind(&now)
        .bind(&now)
        .execute(db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to upsert service identity: {e}")))?;

    Ok(())
}

/// Mark setup as complete for the service identity row.
pub async fn mark_setup_complete(db: &AnyPool, backend: DatabaseBackend) -> Result<(), AppError> {
    let now = chrono::Utc::now().to_rfc3339();
    let sql = adapt_sql(
        "UPDATE service_identity SET setup_complete = TRUE, updated_at = ? WHERE id = 1",
        backend,
    );

    sqlx::query(&sql)
        .bind(&now)
        .execute(db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to mark setup complete: {e}")))?;

    Ok(())
}

/// Generate a DID document for did:web identity mode.
/// The DID is derived dynamically from the request host rather than stored,
/// so the same signing key works across any domain pointing at this server.
/// Returns None if the identity mode is not DidWeb.
pub fn generate_did_document(
    identity: &ServiceIdentity,
    host: &str,
    signing_key_multibase: &str,
    service_entries: &[(String, String)],
    service_endpoint: &str,
) -> Option<serde_json::Value> {
    if identity.mode != IdentityMode::DidWeb {
        return None;
    }

    let did = format!("did:web:{}", host.replace(':', "%3A"));

    let verification_method = serde_json::json!([{
        "id": format!("{did}#atproto"),
        "type": "Multikey",
        "controller": &did,
        "publicKeyMultibase": signing_key_multibase
    }]);

    let services: Vec<serde_json::Value> = service_entries
        .iter()
        .map(|(fragment, svc_type)| {
            serde_json::json!({
                "id": fragment,
                "type": svc_type,
                "serviceEndpoint": service_endpoint
            })
        })
        .collect();

    Some(serde_json::json!({
        "@context": [
            "https://www.w3.org/ns/did/v1",
            "https://w3id.org/security/multikey/v1"
        ],
        "id": &did,
        "verificationMethod": verification_method,
        "service": services
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_identity(mode: IdentityMode, did: Option<&str>) -> ServiceIdentity {
        ServiceIdentity {
            mode,
            did: did.map(String::from),
            signing_key_enc: None,
            setup_complete: true,
            created_at: "2024-01-01".into(),
            updated_at: "2024-01-01".into(),
        }
    }

    #[test]
    fn identity_mode_roundtrip() {
        for mode in [
            IdentityMode::DidWeb,
            IdentityMode::DidPlc,
            IdentityMode::AttachAccount,
            IdentityMode::NotExposed,
        ] {
            let s = mode.as_str();
            let parsed = IdentityMode::parse(s).unwrap();
            assert_eq!(parsed, mode);
        }
    }

    #[test]
    fn identity_mode_from_str_invalid() {
        assert!(IdentityMode::parse("invalid").is_none());
        assert!(IdentityMode::parse("").is_none());
    }

    #[test]
    fn generate_did_document_returns_none_for_non_web() {
        let identity = make_identity(IdentityMode::DidPlc, Some("did:plc:abc123"));
        assert!(
            generate_did_document(&identity, "example.com", "zKey", &[], "https://example.com")
                .is_none()
        );
    }

    #[test]
    fn generate_did_document_derives_did_from_host() {
        let identity = make_identity(IdentityMode::DidWeb, None);
        let doc = generate_did_document(
            &identity,
            "example.com",
            "zKey123",
            &[],
            "https://example.com",
        )
        .unwrap();
        assert_eq!(doc["id"], "did:web:example.com");
    }

    #[test]
    fn generate_did_document_with_no_entries() {
        let identity = make_identity(IdentityMode::DidWeb, None);
        let doc = generate_did_document(
            &identity,
            "example.com",
            "zKey123",
            &[],
            "https://example.com",
        )
        .unwrap();
        assert_eq!(doc["id"], "did:web:example.com");
        assert_eq!(
            doc["verificationMethod"][0]["publicKeyMultibase"],
            "zKey123"
        );
        assert_eq!(doc["service"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn generate_did_document_with_entries() {
        let identity = make_identity(IdentityMode::DidWeb, None);
        let entries = vec![
            ("#chess".to_string(), "ChessService".to_string()),
            ("#checkers".to_string(), "CheckersService".to_string()),
        ];
        let doc = generate_did_document(
            &identity,
            "example.com",
            "zKey123",
            &entries,
            "https://example.com",
        )
        .unwrap();
        let services = doc["service"].as_array().unwrap();
        assert_eq!(services.len(), 2);
        assert_eq!(services[0]["id"], "#chess");
        assert_eq!(services[0]["type"], "ChessService");
        assert_eq!(services[0]["serviceEndpoint"], "https://example.com");
        assert_eq!(services[1]["id"], "#checkers");
    }

    #[test]
    fn generate_did_document_context_and_structure() {
        let identity = make_identity(IdentityMode::DidWeb, None);
        let doc =
            generate_did_document(&identity, "example.com", "zKey", &[], "https://example.com")
                .unwrap();
        let context = doc["@context"].as_array().unwrap();
        assert_eq!(context.len(), 2);
        assert_eq!(context[0], "https://www.w3.org/ns/did/v1");
        assert_eq!(context[1], "https://w3id.org/security/multikey/v1");

        let vm = &doc["verificationMethod"][0];
        assert_eq!(vm["id"], "did:web:example.com#atproto");
        assert_eq!(vm["type"], "Multikey");
        assert_eq!(vm["controller"], "did:web:example.com");
    }
}
