use crate::error::AppError;

pub const CLIENT_ATTESTATION_TYP: &str = "atproto-client-attestation+jwt";

pub struct VerifiedAttestation {
    pub client_id: String,
}

pub async fn verify_client_attestation(
    token: &str,
    expected_aud: &str,
    http: &reqwest::Client,
) -> Result<VerifiedAttestation, AppError> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(AppError::Auth("invalid client attestation format".into()));
    }

    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    let header_bytes = URL_SAFE_NO_PAD
        .decode(parts[0])
        .map_err(|_| AppError::Auth("invalid attestation header encoding".into()))?;
    let header: serde_json::Value = serde_json::from_slice(&header_bytes)
        .map_err(|_| AppError::Auth("invalid attestation header".into()))?;

    if header["typ"].as_str() != Some(CLIENT_ATTESTATION_TYP) {
        return Err(AppError::Auth(format!(
            "attestation typ must be {CLIENT_ATTESTATION_TYP}"
        )));
    }

    let kid = header["kid"]
        .as_str()
        .ok_or_else(|| AppError::Auth("attestation header missing kid".into()))?;

    let payload_bytes = URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|_| AppError::Auth("invalid attestation payload encoding".into()))?;

    #[derive(serde::Deserialize)]
    struct AttestationClaims {
        iss: String,
        sub: String,
        aud: String,
        exp: u64,
    }

    let claims: AttestationClaims = serde_json::from_slice(&payload_bytes)
        .map_err(|_| AppError::Auth("invalid attestation payload".into()))?;

    if claims.iss != claims.sub {
        return Err(AppError::Auth("attestation iss must equal sub".into()));
    }

    if claims.aud != expected_aud {
        return Err(AppError::Auth("attestation aud mismatch".into()));
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    if now >= claims.exp {
        return Err(AppError::Auth("client attestation has expired".into()));
    }

    // Fetch client metadata
    let metadata: serde_json::Value = http
        .get(&claims.iss)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("failed to fetch client metadata: {e}")))?
        .json()
        .await
        .map_err(|e| AppError::Internal(format!("invalid client metadata: {e}")))?;

    // Resolve JWKS
    let jwks = if let Some(jwks) = metadata.get("jwks") {
        jwks.clone()
    } else if let Some(jwks_uri) = metadata["jwks_uri"].as_str() {
        http.get(jwks_uri)
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("failed to fetch JWKS: {e}")))?
            .json()
            .await
            .map_err(|e| AppError::Internal(format!("invalid JWKS: {e}")))?
    } else {
        return Err(AppError::Auth(
            "client metadata has no jwks or jwks_uri".into(),
        ));
    };

    // Find key by kid
    let keys = jwks["keys"]
        .as_array()
        .ok_or_else(|| AppError::Auth("JWKS missing keys array".into()))?;

    let key = keys
        .iter()
        .find(|k| k["kid"].as_str() == Some(kid))
        .ok_or_else(|| AppError::Auth(format!("no key matching kid '{kid}' in JWKS")))?;

    // Verify signature using the matched key
    let alg = header["alg"].as_str().unwrap_or("ES256");
    match alg {
        "ES256" => {
            let jwk = crate::spaces::credential::p256_jwk_to_verifying_key(key)?;
            let message = format!("{}.{}", parts[0], parts[1]);
            let sig_bytes = URL_SAFE_NO_PAD
                .decode(parts[2])
                .map_err(|_| AppError::Auth("invalid attestation signature encoding".into()))?;
            let sig = p256::ecdsa::Signature::from_bytes(sig_bytes.as_slice().into())
                .map_err(|_| AppError::Auth("invalid attestation signature format".into()))?;
            use p256::ecdsa::signature::Verifier;
            jwk.verify(message.as_bytes(), &sig)
                .map_err(|_| AppError::Auth("attestation signature verification failed".into()))?;
        }
        _ => {
            return Err(AppError::Auth(format!(
                "unsupported attestation alg: {alg}"
            )));
        }
    }

    Ok(VerifiedAttestation {
        client_id: claims.iss,
    })
}
