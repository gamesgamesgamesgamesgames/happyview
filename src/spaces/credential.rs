use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use k256::ecdsa::{
    Signature as K256Signature, SigningKey as K256SigningKey, VerifyingKey as K256VerifyingKey,
    signature::Signer as K256Signer, signature::Verifier as K256Verifier,
};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::AppError;
use crate::profile;

pub const DEFAULT_CREDENTIAL_TTL_SECS: u64 = 2 * 60 * 60; // 2 hours
pub const DELEGATION_TOKEN_TTL_SECS: u64 = 60; // 60 seconds

pub const DELEGATION_TOKEN_TYP: &str = "atproto-space-delegation+jwt";
pub const SPACE_CREDENTIAL_TYP: &str = "atproto-space-credential+jwt";

/// Peek at a JWT's header to check its `typ` field without verifying the signature.
pub fn peek_jwt_typ(token: &str) -> Option<String> {
    let header_b64 = token.split('.').next()?;
    let header_bytes = URL_SAFE_NO_PAD.decode(header_b64).ok()?;
    let header: serde_json::Value = serde_json::from_slice(&header_bytes).ok()?;
    header["typ"].as_str().map(|s| s.to_string())
}

/// Peek at a space credential JWT's payload to extract the `sub` (space URI) without verifying.
pub fn peek_credential_sub(token: &str) -> Option<String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let payload_bytes = URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
    let claims: SpaceCredentialClaims = serde_json::from_slice(&payload_bytes).ok()?;
    Some(claims.sub)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationTokenClaims {
    pub iss: String, // User DID
    pub sub: String, // Space URI (ats://...)
    pub aud: String, // Space host (did#atproto_space_host)
    pub iat: u64,
    pub exp: u64,
    pub jti: String, // Random nonce
}

pub fn sign_delegation_token(
    claims: &DelegationTokenClaims,
    signing_key: &K256SigningKey,
) -> Result<String, AppError> {
    let header = serde_json::json!({
        "alg": "ES256K",
        "typ": DELEGATION_TOKEN_TYP,
        "kid": "#atproto",
    });

    let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
    let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());

    let message = format!("{}.{}", header_b64, payload_b64);
    let signature: K256Signature = signing_key.sign(message.as_bytes());
    let sig_b64 = URL_SAFE_NO_PAD.encode(signature.to_bytes());

    Ok(format!("{}.{}.{}", header_b64, payload_b64, sig_b64))
}

pub fn verify_delegation_token(
    token: &str,
    verifying_key: &K256VerifyingKey,
) -> Result<DelegationTokenClaims, AppError> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(AppError::Auth("invalid delegation token format".into()));
    }

    let header_bytes = URL_SAFE_NO_PAD
        .decode(parts[0])
        .map_err(|_| AppError::Auth("invalid delegation token header encoding".into()))?;
    let header: serde_json::Value = serde_json::from_slice(&header_bytes)
        .map_err(|_| AppError::Auth("invalid delegation token header".into()))?;

    if header["alg"].as_str() != Some("ES256K") {
        return Err(AppError::Auth("delegation token alg must be ES256K".into()));
    }

    if header["typ"].as_str() != Some(DELEGATION_TOKEN_TYP) {
        return Err(AppError::Auth(format!(
            "delegation token typ must be {DELEGATION_TOKEN_TYP}"
        )));
    }

    let message = format!("{}.{}", parts[0], parts[1]);
    let sig_bytes = URL_SAFE_NO_PAD
        .decode(parts[2])
        .map_err(|_| AppError::Auth("invalid delegation token signature encoding".into()))?;

    // Try direct verify, then with low-S normalization
    let verified = if let Ok(sig) = K256Signature::from_bytes(sig_bytes.as_slice().into()) {
        if verifying_key.verify(message.as_bytes(), &sig).is_ok() {
            true
        } else if let Some(normalized) = sig.normalize_s() {
            verifying_key
                .verify(message.as_bytes(), &normalized)
                .is_ok()
        } else {
            false
        }
    } else {
        false
    };

    if !verified {
        return Err(AppError::Auth(
            "delegation token signature verification failed".into(),
        ));
    }

    let payload_bytes = URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|_| AppError::Auth("invalid delegation token payload encoding".into()))?;
    let claims: DelegationTokenClaims = serde_json::from_slice(&payload_bytes)
        .map_err(|_| AppError::Auth("invalid delegation token payload".into()))?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    if now >= claims.exp {
        return Err(AppError::Auth("delegation token has expired".into()));
    }

    Ok(claims)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceCredentialClaims {
    pub iss: String, // Space authority DID
    pub sub: String, // Space URI (ats://...)
    pub iat: u64,
    pub exp: u64,
    pub jti: String, // Random nonce
}

pub fn sign_credential(
    claims: &SpaceCredentialClaims,
    private_jwk: &serde_json::Value,
) -> Result<String, AppError> {
    let d_b64 = private_jwk["d"]
        .as_str()
        .ok_or_else(|| AppError::Internal("signing key missing d parameter".into()))?;

    let d_bytes = URL_SAFE_NO_PAD
        .decode(d_b64)
        .map_err(|_| AppError::Internal("invalid signing key d parameter".into()))?;

    let signing_key = SigningKey::from_bytes((&d_bytes[..]).into())
        .map_err(|e| AppError::Internal(format!("invalid signing key: {e}")))?;

    let header = serde_json::json!({
        "alg": "ES256",
        "typ": SPACE_CREDENTIAL_TYP,
        "kid": "#atproto_space",
    });

    let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
    let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());

    let message = format!("{}.{}", header_b64, payload_b64);
    let signature: Signature = signing_key.sign(message.as_bytes());
    let sig_b64 = URL_SAFE_NO_PAD.encode(signature.to_bytes());

    Ok(format!("{}.{}.{}", header_b64, payload_b64, sig_b64))
}

pub fn verify_credential(
    token: &str,
    public_jwk: &serde_json::Value,
) -> Result<SpaceCredentialClaims, AppError> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(AppError::Auth("invalid credential format".into()));
    }

    let header_bytes = URL_SAFE_NO_PAD
        .decode(parts[0])
        .map_err(|_| AppError::Auth("invalid credential header encoding".into()))?;
    let header: serde_json::Value = serde_json::from_slice(&header_bytes)
        .map_err(|_| AppError::Auth("invalid credential header".into()))?;

    if header["alg"].as_str() != Some("ES256") {
        return Err(AppError::Auth("credential alg must be ES256".into()));
    }

    if header["typ"].as_str() != Some(SPACE_CREDENTIAL_TYP) {
        return Err(AppError::Auth(format!(
            "credential typ must be {SPACE_CREDENTIAL_TYP}"
        )));
    }

    let verifying_key = p256_jwk_to_verifying_key(public_jwk)?;

    let message = format!("{}.{}", parts[0], parts[1]);
    let sig_bytes = URL_SAFE_NO_PAD
        .decode(parts[2])
        .map_err(|_| AppError::Auth("invalid credential signature encoding".into()))?;
    let signature = Signature::from_bytes(sig_bytes.as_slice().into())
        .map_err(|_| AppError::Auth("invalid credential signature format".into()))?;

    verifying_key
        .verify(message.as_bytes(), &signature)
        .map_err(|_| AppError::Auth("credential signature verification failed".into()))?;

    let payload_bytes = URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|_| AppError::Auth("invalid credential payload encoding".into()))?;
    let claims: SpaceCredentialClaims = serde_json::from_slice(&payload_bytes)
        .map_err(|_| AppError::Auth("invalid credential payload".into()))?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    if now >= claims.exp {
        return Err(AppError::Auth("credential has expired".into()));
    }

    Ok(claims)
}

/// Extract a P-256 verifying key from a JWK.
pub fn p256_jwk_to_verifying_key(jwk: &serde_json::Value) -> Result<VerifyingKey, AppError> {
    let x_b64 = jwk["x"]
        .as_str()
        .ok_or_else(|| AppError::Auth("JWK missing x".into()))?;
    let y_b64 = jwk["y"]
        .as_str()
        .ok_or_else(|| AppError::Auth("JWK missing y".into()))?;

    let x_bytes = URL_SAFE_NO_PAD
        .decode(x_b64)
        .map_err(|_| AppError::Auth("invalid JWK x".into()))?;
    let y_bytes = URL_SAFE_NO_PAD
        .decode(y_b64)
        .map_err(|_| AppError::Auth("invalid JWK y".into()))?;

    let mut sec1 = Vec::with_capacity(65);
    sec1.push(0x04);
    sec1.extend_from_slice(&x_bytes);
    sec1.extend_from_slice(&y_bytes);

    VerifyingKey::from_sec1_bytes(&sec1)
        .map_err(|_| AppError::Auth("invalid P-256 public key".into()))
}

/// Convert a multibase-encoded P-256 public key (from a DID doc `publicKeyMultibase`)
/// into a JWK suitable for `verify_credential`.
pub fn multikey_to_p256_jwk(public_key_multibase: &str) -> Result<serde_json::Value, AppError> {
    let (_base, key_bytes) = multibase::decode(public_key_multibase)
        .map_err(|e| AppError::Auth(format!("invalid multibase encoding: {e}")))?;

    // P-256 multicodec prefix: varint 0x1200 → bytes [0x80, 0x24]
    if key_bytes.len() < 2 || key_bytes[0] != 0x80 || key_bytes[1] != 0x24 {
        return Err(AppError::Auth(
            "public key is not a P-256 multicodec key".into(),
        ));
    }

    let compressed = &key_bytes[2..];
    let verifying_key = VerifyingKey::from_sec1_bytes(compressed)
        .map_err(|_| AppError::Auth("invalid P-256 public key bytes".into()))?;

    let point = verifying_key.to_encoded_point(false);
    let x = point
        .x()
        .ok_or_else(|| AppError::Auth("failed to extract x coordinate".into()))?;
    let y = point
        .y()
        .ok_or_else(|| AppError::Auth("failed to extract y coordinate".into()))?;

    Ok(serde_json::json!({
        "kty": "EC",
        "crv": "P-256",
        "x": URL_SAFE_NO_PAD.encode(x),
        "y": URL_SAFE_NO_PAD.encode(y),
    }))
}

/// Verify a space credential JWT issued by an external space host.
///
/// Resolves the issuer's DID document, extracts the `#atproto_space` signing key,
/// and verifies the JWT signature and expiry.
pub async fn verify_external_credential(
    token: &str,
    http: &reqwest::Client,
    plc_url: &str,
) -> Result<SpaceCredentialClaims, AppError> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(AppError::Auth("invalid credential format".into()));
    }

    let payload_bytes = URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|_| AppError::Auth("invalid credential payload encoding".into()))?;
    let peek: SpaceCredentialClaims = serde_json::from_slice(&payload_bytes)
        .map_err(|_| AppError::Auth("invalid credential payload".into()))?;

    let did_doc = profile::resolve_did_document(http, plc_url, &peek.iss).await?;

    let vm = did_doc
        .verification_method
        .iter()
        .find(|v| v.id.ends_with("#atproto_space"))
        .ok_or_else(|| {
            AppError::Auth("issuer DID has no #atproto_space verification method".into())
        })?;

    let multibase = vm
        .public_key_multibase
        .as_deref()
        .ok_or_else(|| AppError::Auth("verification method missing publicKeyMultibase".into()))?;

    let jwk = multikey_to_p256_jwk(multibase)?;
    verify_credential(token, &jwk)
}

pub fn make_jti() -> String {
    Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::keys::generate_dpop_keypair;

    fn make_claims() -> SpaceCredentialClaims {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        SpaceCredentialClaims {
            iss: "did:plc:spaceowner".into(),
            sub: "ats://did:plc:spaceowner/com.example.forum/main".into(),
            iat: now,
            exp: now + DEFAULT_CREDENTIAL_TTL_SECS,
            jti: make_jti(),
        }
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let keypair = generate_dpop_keypair().unwrap();
        let claims = make_claims();

        let token = sign_credential(&claims, &keypair.private_jwk).unwrap();
        let verified = verify_credential(&token, &keypair.public_jwk).unwrap();

        assert_eq!(verified.iss, claims.iss);
        assert_eq!(verified.sub, claims.sub);
        assert_eq!(verified.iat, claims.iat);
        assert_eq!(verified.exp, claims.exp);
        assert_eq!(verified.jti, claims.jti);
    }

    #[test]
    fn verify_rejects_tampered_payload() {
        let keypair = generate_dpop_keypair().unwrap();
        let claims = make_claims();
        let token = sign_credential(&claims, &keypair.private_jwk).unwrap();

        let parts: Vec<&str> = token.split('.').collect();
        let mut payload_bytes = URL_SAFE_NO_PAD.decode(parts[1]).unwrap();
        payload_bytes[0] ^= 0xFF;
        let tampered_payload = URL_SAFE_NO_PAD.encode(&payload_bytes);
        let tampered = format!("{}.{}.{}", parts[0], tampered_payload, parts[2]);

        let result = verify_credential(&tampered, &keypair.public_jwk);
        assert!(result.is_err());
    }

    #[test]
    fn verify_rejects_wrong_key() {
        let keypair1 = generate_dpop_keypair().unwrap();
        let keypair2 = generate_dpop_keypair().unwrap();
        let claims = make_claims();
        let token = sign_credential(&claims, &keypair1.private_jwk).unwrap();

        let result = verify_credential(&token, &keypair2.public_jwk);
        assert!(result.is_err());
    }

    #[test]
    fn verify_rejects_expired() {
        let keypair = generate_dpop_keypair().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let claims = SpaceCredentialClaims {
            iss: "did:plc:owner".into(),
            sub: "ats://did:plc:owner/com.example.test/main".into(),
            iat: now - 7200,
            exp: now - 3600,
            jti: make_jti(),
        };

        let token = sign_credential(&claims, &keypair.private_jwk).unwrap();
        let result = verify_credential(&token, &keypair.public_jwk);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("expired"));
    }

    #[test]
    fn verify_rejects_invalid_format() {
        let keypair = generate_dpop_keypair().unwrap();
        let result = verify_credential("not-a-jwt", &keypair.public_jwk);
        assert!(result.is_err());
    }

    fn make_k256_signing_key() -> K256SigningKey {
        let key_bytes = [0x42u8; 32];
        K256SigningKey::from_bytes((&key_bytes[..]).into()).expect("valid key")
    }

    fn make_delegation_claims() -> DelegationTokenClaims {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        DelegationTokenClaims {
            iss: "did:plc:member".into(),
            sub: "ats://did:plc:space/com.example.forum/main".into(),
            aud: "did:plc:space#atproto_space_host".into(),
            iat: now,
            exp: now + DELEGATION_TOKEN_TTL_SECS,
            jti: make_jti(),
        }
    }

    #[test]
    fn delegation_sign_and_verify_roundtrip() {
        let signing_key = make_k256_signing_key();
        let verifying_key = K256VerifyingKey::from(&signing_key);
        let claims = make_delegation_claims();

        let token = sign_delegation_token(&claims, &signing_key).unwrap();
        let verified = verify_delegation_token(&token, &verifying_key).unwrap();

        assert_eq!(verified.iss, claims.iss);
        assert_eq!(verified.sub, claims.sub);
        assert_eq!(verified.aud, claims.aud);
        assert_eq!(verified.jti, claims.jti);
    }

    #[test]
    fn delegation_rejects_wrong_key() {
        let signing_key = make_k256_signing_key();
        let other_key = K256SigningKey::from_bytes((&[0x99u8; 32][..]).into()).unwrap();
        let verifying_key = K256VerifyingKey::from(&other_key);
        let claims = make_delegation_claims();

        let token = sign_delegation_token(&claims, &signing_key).unwrap();
        let result = verify_delegation_token(&token, &verifying_key);
        assert!(result.is_err());
    }

    #[test]
    fn delegation_rejects_expired() {
        let signing_key = make_k256_signing_key();
        let verifying_key = K256VerifyingKey::from(&signing_key);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let claims = DelegationTokenClaims {
            iss: "did:plc:member".into(),
            sub: "ats://did:plc:space/com.example.forum/main".into(),
            aud: "did:plc:space#atproto_space_host".into(),
            iat: now - 120,
            exp: now - 60,
            jti: make_jti(),
        };

        let token = sign_delegation_token(&claims, &signing_key).unwrap();
        let result = verify_delegation_token(&token, &verifying_key);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("expired"));
    }

    #[test]
    fn delegation_rejects_wrong_typ() {
        let signing_key = make_k256_signing_key();
        let verifying_key = K256VerifyingKey::from(&signing_key);
        let claims = make_delegation_claims();

        // Craft a token with wrong typ
        let header = serde_json::json!({ "alg": "ES256K", "typ": "wrong-typ", "kid": "#atproto" });
        let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
        let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let message = format!("{}.{}", header_b64, payload_b64);
        let sig: K256Signature = signing_key.sign(message.as_bytes());
        let token = format!(
            "{}.{}.{}",
            header_b64,
            payload_b64,
            URL_SAFE_NO_PAD.encode(sig.to_bytes())
        );

        let result = verify_delegation_token(&token, &verifying_key);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("typ"));
    }

    #[test]
    fn credential_has_space_credential_typ() {
        let keypair = generate_dpop_keypair().unwrap();
        let claims = make_claims();
        let token = sign_credential(&claims, &keypair.private_jwk).unwrap();
        assert_eq!(peek_jwt_typ(&token).as_deref(), Some(SPACE_CREDENTIAL_TYP));
    }

    #[test]
    fn delegation_has_delegation_typ() {
        let signing_key = make_k256_signing_key();
        let claims = make_delegation_claims();
        let token = sign_delegation_token(&claims, &signing_key).unwrap();
        assert_eq!(peek_jwt_typ(&token).as_deref(), Some(DELEGATION_TOKEN_TYP));
    }

    #[test]
    fn peek_jwt_typ_returns_none_for_garbage() {
        assert_eq!(peek_jwt_typ("not-a-jwt"), None);
        assert_eq!(peek_jwt_typ(""), None);
    }

    #[test]
    fn peek_credential_sub_extracts_space_uri() {
        let keypair = generate_dpop_keypair().unwrap();
        let claims = make_claims();
        let token = sign_credential(&claims, &keypair.private_jwk).unwrap();
        assert_eq!(
            peek_credential_sub(&token).as_deref(),
            Some("ats://did:plc:spaceowner/com.example.forum/main")
        );
    }

    #[test]
    fn peek_credential_sub_returns_none_for_garbage() {
        assert_eq!(peek_credential_sub("not-a-jwt"), None);
    }

    #[test]
    fn verify_rejects_wrong_typ() {
        let keypair = generate_dpop_keypair().unwrap();
        let claims = make_claims();

        let d_b64 = keypair.private_jwk["d"].as_str().unwrap();
        let d_bytes = URL_SAFE_NO_PAD.decode(d_b64).unwrap();
        let signing_key = p256::ecdsa::SigningKey::from_bytes((&d_bytes[..]).into()).unwrap();

        let header = serde_json::json!({ "alg": "ES256", "typ": "JWT" });
        let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
        let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let message = format!("{}.{}", header_b64, payload_b64);
        let sig: p256::ecdsa::Signature =
            p256::ecdsa::signature::Signer::sign(&signing_key, message.as_bytes());
        let token = format!(
            "{}.{}.{}",
            header_b64,
            payload_b64,
            URL_SAFE_NO_PAD.encode(sig.to_bytes())
        );

        let result = verify_credential(&token, &keypair.public_jwk);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("typ"));
    }

    #[test]
    fn multikey_to_p256_jwk_invalid_multibase() {
        let result = multikey_to_p256_jwk("xabc123");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("multibase"));
    }

    #[test]
    fn multikey_to_p256_jwk_wrong_codec() {
        let mut bytes = vec![0x99u8, 0x99];
        bytes.extend_from_slice(&[0u8; 33]);
        let encoded = multibase::encode(multibase::Base::Base58Btc, &bytes);
        let result = multikey_to_p256_jwk(&encoded);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("P-256"));
    }
}
