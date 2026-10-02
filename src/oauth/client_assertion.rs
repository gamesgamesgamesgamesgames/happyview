//! `private_key_jwt` client assertions (RFC 7523).
//!
//! Mirrors `pds_write::generate_dpop_proof_inner`'s hand-rolled ES256 signing
//! rather than going through atrium, because the two callers that need one —
//! the assertion endpoint and `refresh_access_token` — both sit outside
//! atrium's client.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::ecdsa::{SigningKey, signature::Signer};

use crate::error::AppError;

pub const CLIENT_ASSERTION_TYPE: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";

/// Seconds a client assertion is valid for. The provider rejects anything
/// older than one minute (`CLIENT_ASSERTION_MAX_AGE`).
pub const ASSERTION_TTL_SECS: u64 = 60;

/// Build a signed client assertion for `issuer`.
pub fn build(
    private_jwk: &serde_json::Value,
    kid: &str,
    client_id: &str,
    issuer: &str,
) -> Result<String, AppError> {
    let d_b64 = private_jwk["d"]
        .as_str()
        .ok_or_else(|| AppError::Internal("client key missing d parameter".into()))?;
    let d_bytes = URL_SAFE_NO_PAD
        .decode(d_b64)
        .map_err(|_| AppError::Internal("invalid client key d parameter".into()))?;
    let signing_key = SigningKey::from_slice(&d_bytes[..])
        .map_err(|e| AppError::Internal(format!("invalid client signing key: {e}")))?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let header = serde_json::json!({
        "alg": "ES256",
        "typ": "JWT",
        "kid": kid,
    });

    let claims = serde_json::json!({
        "iss": client_id,
        "sub": client_id,
        "aud": issuer,
        "iat": now,
        "exp": now + ASSERTION_TTL_SECS,
        "jti": uuid::Uuid::new_v4().to_string(),
    });

    let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
    let claims_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    let message = format!("{header_b64}.{claims_b64}");
    let signature: p256::ecdsa::Signature = signing_key.sign(message.as_bytes());

    Ok(format!(
        "{message}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    fn decode_part(jwt: &str, index: usize) -> serde_json::Value {
        let part = jwt.split('.').nth(index).unwrap();
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
    }

    #[test]
    fn assertion_carries_the_required_claims() {
        let key = crate::oauth::client_keys::generate_client_key("client-a").unwrap();
        let jwt = build(
            &key.private_jwk,
            &key.kid,
            "https://app.example.com/client-metadata.json",
            "https://pds.example.com",
        )
        .unwrap();

        let header = decode_part(&jwt, 0);
        assert_eq!(header["alg"], "ES256");
        assert_eq!(header["kid"], key.kid);
        assert!(
            header["jwk"].is_null(),
            "a client assertion must not inline its key"
        );

        let claims = decode_part(&jwt, 1);
        assert_eq!(
            claims["iss"],
            "https://app.example.com/client-metadata.json"
        );
        assert_eq!(
            claims["sub"],
            "https://app.example.com/client-metadata.json"
        );
        assert_eq!(claims["aud"], "https://pds.example.com");
        assert!(claims["jti"].as_str().is_some_and(|s| !s.is_empty()));
        let iat = claims["iat"].as_u64().unwrap();
        let exp = claims["exp"].as_u64().unwrap();
        assert_eq!(exp - iat, 60);
    }

    #[test]
    fn each_assertion_has_a_distinct_jti() {
        let key = crate::oauth::client_keys::generate_client_key("client-a").unwrap();
        let a = build(&key.private_jwk, &key.kid, "https://a", "https://b").unwrap();
        let b = build(&key.private_jwk, &key.kid, "https://a", "https://b").unwrap();
        assert_ne!(decode_part(&a, 1)["jti"], decode_part(&b, 1)["jti"]);
    }

    #[test]
    fn a_key_without_d_is_rejected() {
        let key = crate::oauth::client_keys::generate_client_key("client-a").unwrap();
        assert!(build(&key.public_jwk, &key.kid, "https://a", "https://b").is_err());
    }
}
