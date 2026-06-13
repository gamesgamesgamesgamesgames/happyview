use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use p256::ecdsa::{Signature as P256Signature, VerifyingKey as P256Key, signature::Verifier};
use serde::Deserialize;

use crate::AppState;
use crate::error::AppError;

/// Authenticated ATProto user identity extracted from a service auth JWT.
///
/// Used for XRPC endpoints that receive proxied requests from PDSes.
/// The JWT is signed by the caller's signing key and validated by
/// resolving their DID document.
#[derive(Debug, Clone)]
pub struct ServiceAuth {
    /// The authenticated user's DID (from `iss`).
    pub did: String,
}

// JWT types
#[derive(Deserialize)]
struct JwtHeader {
    alg: String,
    #[serde(default)]
    typ: Option<String>,
}

#[derive(Deserialize)]
struct JwtPayload {
    iss: String,
    aud: String,
    exp: u64,
    #[serde(default)]
    lxm: Option<String>,
}

// DID document types
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DidDocument {
    #[serde(default)]
    verification_method: Vec<VerificationMethod>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VerificationMethod {
    id: String,
    #[serde(rename = "type")]
    method_type: String,
    #[serde(default)]
    public_key_multibase: Option<String>,
}

impl ServiceAuth {
    /// Validate a Bearer token as a service auth JWT.
    pub async fn from_bearer(token: &str, state: &AppState) -> Result<Self, AppError> {
        let payload = verify_service_jwt(token, state, false).await?;
        Ok(ServiceAuth { did: payload.iss })
    }
}

// Axum extractor
impl FromRequestParts<AppState> for ServiceAuth {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or(AppError::Auth("missing Authorization header".into()))?;

        let token = header
            .strip_prefix("Bearer ")
            .ok_or(AppError::Auth("invalid Authorization scheme".into()))?;

        Self::from_bearer(token, state).await
    }
}

// JWT verification (boxed future to allow recursion for retry)
fn verify_service_jwt<'a>(
    token: &'a str,
    state: &'a AppState,
    is_retry: bool,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<JwtPayload, AppError>> + Send + 'a>>
{
    Box::pin(async move {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            return Err(AppError::Auth("invalid JWT structure".into()));
        }

        let header_bytes = URL_SAFE_NO_PAD
            .decode(parts[0])
            .map_err(|_| AppError::Auth("invalid JWT header".into()))?;
        let payload_bytes = URL_SAFE_NO_PAD
            .decode(parts[1])
            .map_err(|_| AppError::Auth("invalid JWT payload".into()))?;
        let sig_bytes = URL_SAFE_NO_PAD
            .decode(parts[2])
            .map_err(|_| AppError::Auth("invalid JWT signature".into()))?;

        let header: JwtHeader = serde_json::from_slice(&header_bytes)
            .map_err(|_| AppError::Auth("invalid JWT header".into()))?;
        let payload: JwtPayload = serde_json::from_slice(&payload_bytes)
            .map_err(|_| AppError::Auth("invalid JWT payload".into()))?;

        // Reject forbidden typ values.
        if let Some(ref typ) = header.typ {
            let t = typ.to_lowercase();
            if t == "at+jwt" || t == "refresh+jwt" || t == "dpop+jwt" {
                return Err(AppError::Auth("forbidden JWT typ".into()));
            }
        }

        // Only support ES256 and ES256K.
        if header.alg != "ES256" && header.alg != "ES256K" {
            tracing::warn!(alg = %header.alg, "unsupported JWT algorithm");
            return Err(AppError::Auth("unsupported JWT algorithm".into()));
        }

        // Check expiration.
        let now = chrono::Utc::now().timestamp() as u64;
        if now > payload.exp {
            tracing::warn!(exp = payload.exp, now = now, "service auth JWT expired");
            return Err(AppError::Auth("JWT expired".into()));
        }

        // Check audience if SERVICE_DID is configured (optional for HappyView).
        // For now, accept any audience.
        let _ = &payload.aud;

        // Check lxm if present (optional validation).
        if let Some(ref _lxm) = payload.lxm {
            // Allow any lxm for now — HappyView serves many different XRPC methods.
        }

        // Resolve the issuer's DID document to get their signing key.
        let signing_key = resolve_signing_key(&payload.iss, state).await?;

        // Verify signature: message is "header.payload" as UTF-8 bytes.
        let msg = format!("{}.{}", parts[0], parts[1]);

        let valid = match header.alg.as_str() {
            "ES256" => verify_es256(msg.as_bytes(), &sig_bytes, &signing_key),
            "ES256K" => verify_es256k(msg.as_bytes(), &sig_bytes, &signing_key),
            _ => false,
        };

        if !valid && !is_retry {
            tracing::debug!(iss = %payload.iss, "signature failed, retrying with fresh DID doc");
            return verify_service_jwt(token, state, true).await;
        }
        if !valid {
            tracing::warn!(iss = %payload.iss, "service auth JWT signature verification failed");
            return Err(AppError::Auth("JWT signature verification failed".into()));
        }

        Ok(payload)
    })
}

// DID resolution
async fn resolve_signing_key(did: &str, state: &AppState) -> Result<Vec<u8>, AppError> {
    let url = if did.starts_with("did:plc:") {
        format!("{}/{did}", state.config.plc_url.trim_end_matches('/'))
    } else if did.starts_with("did:web:") {
        let identifier = did.strip_prefix("did:web:").unwrap();
        let mut segments = identifier.split(':');
        let host = segments.next().unwrap();
        let host = urlencoding::decode(host).unwrap_or_else(|_| host.into());
        let path_segments: Vec<&str> = segments.collect();
        if path_segments.is_empty() {
            format!("https://{host}/.well-known/did.json")
        } else {
            format!("https://{host}/{}/did.json", path_segments.join("/"))
        }
    } else {
        return Err(AppError::BadRequest(format!(
            "unsupported DID method: {did}"
        )));
    };

    let resp = state
        .http
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("DID resolution failed for {did}: {e}")))?;

    if !resp.status().is_success() {
        return Err(AppError::Internal(format!(
            "DID resolution returned {} for {did}",
            resp.status()
        )));
    }

    let doc: DidDocument = resp
        .json()
        .await
        .map_err(|e| AppError::Internal(format!("invalid DID document for {did}: {e}")))?;

    let vm = doc
        .verification_method
        .iter()
        .find(|vm| vm.id == format!("{did}#atproto") || vm.id == "#atproto")
        .ok_or_else(|| {
            AppError::Internal(format!(
                "no #atproto verification method in DID doc for {did}"
            ))
        })?;

    let multibase = vm.public_key_multibase.as_deref().ok_or_else(|| {
        AppError::Internal(format!("no publicKeyMultibase on #atproto key for {did}"))
    })?;

    decode_multibase_key(multibase, &vm.method_type)
}

/// Decode a multibase-encoded public key from a DID document.
fn decode_multibase_key(multibase_str: &str, method_type: &str) -> Result<Vec<u8>, AppError> {
    let (_, decoded) = multibase::decode(multibase_str)
        .map_err(|e| AppError::Internal(format!("multibase decode failed: {e}")))?;

    match method_type {
        "Multikey" => {
            if decoded.len() < 2 {
                return Err(AppError::Internal("multikey too short".into()));
            }
            Ok(decoded[2..].to_vec())
        }
        "EcdsaSecp256r1VerificationKey2019" | "EcdsaSecp256k1VerificationKey2019" => Ok(decoded),
        other => Err(AppError::Internal(format!(
            "unsupported verification method type: {other}"
        ))),
    }
}

// Signature verification
fn verify_es256(msg: &[u8], sig_bytes: &[u8], key_bytes: &[u8]) -> bool {
    let Ok(verifying_key) = P256Key::from_sec1_bytes(key_bytes) else {
        tracing::warn!("failed to parse P-256 public key");
        return false;
    };

    if let Ok(sig) = P256Signature::from_bytes(sig_bytes.into())
        && verifying_key.verify(msg, &sig).is_ok()
    {
        return true;
    }

    if let Ok(sig) = P256Signature::from_bytes(sig_bytes.into())
        && let Some(normalized) = sig.normalize_s()
        && verifying_key.verify(msg, &normalized).is_ok()
    {
        return true;
    }

    false
}

/// Minimal JWT payload for extracting fields after signature verification.
#[derive(Debug, serde::Deserialize)]
pub struct PublicJwtPayload {
    pub iss: String,
    pub aud: Option<String>,
    pub exp: u64,
}

/// Decode the JWT payload without verification.
///
/// Call this *after* `ServiceAuth::from_bearer` has already validated the
/// signature. This is used to extract the `aud` field for service auth
/// fragment matching.
pub fn decode_jwt_payload(token: &str) -> Result<PublicJwtPayload, AppError> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(AppError::Auth("invalid JWT format".into()));
    }
    let payload_bytes = URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|_| AppError::Auth("invalid JWT payload encoding".into()))?;
    serde_json::from_slice(&payload_bytes).map_err(|_| AppError::Auth("invalid JWT payload".into()))
}

fn verify_es256k(msg: &[u8], sig_bytes: &[u8], key_bytes: &[u8]) -> bool {
    use k256::ecdsa::{Signature as K256Signature, VerifyingKey as K256Key, signature::Verifier};

    let Ok(verifying_key) = K256Key::from_sec1_bytes(key_bytes) else {
        tracing::warn!("failed to parse secp256k1 public key");
        return false;
    };

    if let Ok(sig) = K256Signature::from_bytes(sig_bytes.into())
        && verifying_key.verify(msg, &sig).is_ok()
    {
        return true;
    }

    if let Ok(sig) = K256Signature::from_bytes(sig_bytes.into())
        && let Some(normalized) = sig.normalize_s()
        && verifying_key.verify(msg, &normalized).is_ok()
    {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_jwt(payload_json: &str) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"alg":"ES256","typ":"JWT"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload_json);
        let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("fake_sig");
        format!("{}.{}.{}", header, payload, signature)
    }

    #[test]
    fn decode_valid_payload() {
        let jwt = make_test_jwt(
            r#"{"iss":"did:plc:abc","aud":"did:web:example.com#svc","exp":9999999999}"#,
        );
        let payload = decode_jwt_payload(&jwt).unwrap();
        assert_eq!(payload.iss, "did:plc:abc");
        assert_eq!(payload.aud.unwrap(), "did:web:example.com#svc");
        assert_eq!(payload.exp, 9999999999);
    }

    #[test]
    fn decode_payload_without_aud() {
        let jwt = make_test_jwt(r#"{"iss":"did:plc:abc","exp":9999999999}"#);
        let payload = decode_jwt_payload(&jwt).unwrap();
        assert!(payload.aud.is_none());
    }

    #[test]
    fn decode_rejects_invalid_format() {
        assert!(decode_jwt_payload("not.a.valid.jwt.with.too.many.parts").is_err());
        assert!(decode_jwt_payload("onlyonepart").is_err());
        assert!(decode_jwt_payload("two.parts").is_err());
    }
}
