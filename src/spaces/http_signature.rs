//! Proof that a request comes from the holder of a space credential's key.
//!
//! A space credential reads a whole space, so as a bearer token it would be a
//! shared secret: a repo host handed one to serve one repo could replay it
//! against every other host in the space. Instead each credential is bound to a
//! P-256 key the syncer holds (`cnf.kid`), and each request carries an
//! [RFC 9421](https://www.rfc-editor.org/rfc/rfc9421) HTTP Message Signature by
//! that key over the `Authorization` field and, when a credential is used, the
//! `Atproto-Space-Audience` field naming the DID the request is for.
//!
//! The same construction proves possession when the credential is requested:
//! the delegation token is signed over `Authorization` alone, and the signature's
//! `keyid` becomes the credential's `cnf.kid`.

use axum::http::HeaderMap;
use axum::http::StatusCode;
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};

use crate::error::AppError;

/// The label of the signature this protocol reads, among any others present.
pub const SIGNATURE_LABEL: &str = "atproto-space";
/// The one algorithm accepted. Naming it is optional.
pub const SIGNATURE_ALG: &str = "ecdsa-p256-sha256";
/// The field naming the DID a credential-authenticated request is addressed to.
pub const AUDIENCE_HEADER: &str = "atproto-space-audience";

/// Why a signature was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureError(pub String);

impl std::fmt::Display for SignatureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<SignatureError> for AppError {
    fn from(e: SignatureError) -> Self {
        AppError::XrpcError {
            status: StatusCode::UNAUTHORIZED,
            code: "BadSpaceSignature",
            message: e.0,
        }
    }
}

/// Verify a request's `atproto-space` signature, returning the `did:key` that
/// made it.
///
/// With `bound_key`, the request uses a credential bound to that key: the
/// signature must cover `Authorization` and `Atproto-Space-Audience`. Without
/// it, the request exchanges a delegation token: the signature covers
/// `Authorization` alone and names its key in `keyid`.
///
/// The audience is checked for presence here and against the request's target
/// by the handler, which is the only place that knows the target.
pub fn verify(headers: &HeaderMap, bound_key: Option<&str>) -> Result<String, SignatureError> {
    let authorization = exactly_one(headers, "authorization")?;
    let audience = match bound_key {
        Some(_) => Some(exactly_one(headers, AUDIENCE_HEADER)?),
        None => None,
    };

    let input = labelled(headers, "signature-input")?;
    let signature = labelled(headers, "signature")?;
    let sfv::ListEntry::InnerList(input) = input else {
        return Err(malformed());
    };
    let sfv::ListEntry::Item(signature) = signature else {
        return Err(malformed());
    };
    let signature = match signature.bare_item.as_byte_sequence() {
        Some(bytes) if signature.params.is_empty() => bytes.to_vec(),
        _ => return Err(malformed()),
    };

    let expected: &[&str] = match bound_key {
        Some(_) => &["authorization", AUDIENCE_HEADER],
        None => &["authorization"],
    };
    let covers_exactly = input.items.len() == expected.len()
        && input.items.iter().zip(expected).all(|(item, want)| {
            item.params.is_empty() && item.bare_item.as_string().map(|s| s.as_str()) == Some(*want)
        });
    if !covers_exactly {
        return Err(SignatureError(format!(
            "signature must cover exactly {expected:?}, in order"
        )));
    }

    if let Some(alg) = input.params.get("alg")
        && alg.as_string().map(|s| s.as_str()) != Some(SIGNATURE_ALG)
    {
        return Err(SignatureError(format!(
            "signature algorithm must be {SIGNATURE_ALG}"
        )));
    }
    let keyid = input.params.get("keyid");
    let signing_key = match (bound_key, keyid) {
        (Some(bound), None) => bound.to_string(),
        (Some(bound), Some(keyid)) => {
            if keyid.as_string().map(|s| s.as_str()) != Some(bound) {
                return Err(SignatureError(
                    "signature keyid does not match the credential key".into(),
                ));
            }
            bound.to_string()
        }
        (None, Some(keyid)) => keyid
            .as_string()
            .map(|s| s.as_str().to_string())
            .ok_or_else(not_p256)?,
        (None, None) => return Err(not_p256()),
    };
    let verifying_key = parse_did_key(&signing_key)?;

    use sfv::FieldType;
    let params = vec![sfv::ListEntry::InnerList(input)]
        .serialize()
        .ok_or_else(malformed)?;
    let base = signature_base(authorization, audience, &params);

    if signature.len() != 64 {
        return Err(invalid());
    }
    let signature = Signature::from_slice(&signature).map_err(|_| invalid())?;
    use p256::ecdsa::signature::Verifier;
    verifying_key
        .verify(base.as_bytes(), &signature)
        .map_err(|_| invalid())?;

    Ok(signing_key)
}

fn exactly_one<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str, SignatureError> {
    let mut values = headers.get_all(name).iter();
    match (values.next(), values.next()) {
        (Some(value), None) => value
            .to_str()
            .ok()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                SignatureError(format!("request requires exactly one \"{name}\" field"))
            }),
        _ => Err(SignatureError(format!(
            "request requires exactly one \"{name}\" field"
        ))),
    }
}

/// This protocol's member of a signature dictionary field, which may carry
/// other labels, and may arrive split across several field lines.
fn labelled(headers: &HeaderMap, name: &str) -> Result<sfv::ListEntry, SignatureError> {
    let joined = headers
        .get_all(name)
        .iter()
        .map(|v| v.to_str().map_err(|_| malformed()))
        .collect::<Result<Vec<_>, _>>()?
        .join(", ");
    if joined.is_empty() {
        return Err(SignatureError(
            "missing or malformed signature headers".into(),
        ));
    }
    let mut dictionary: sfv::Dictionary =
        sfv::Parser::new(&joined).parse().map_err(|_| malformed())?;
    dictionary
        .shift_remove(SIGNATURE_LABEL)
        .ok_or_else(malformed)
}

fn signature_base(authorization: &str, audience: Option<&str>, params: &str) -> String {
    let mut lines = vec![format!("\"authorization\": {}", authorization.trim())];
    if let Some(audience) = audience {
        lines.push(format!("\"{AUDIENCE_HEADER}\": {}", audience.trim()));
    }
    lines.push(format!("\"@signature-params\": {params}"));
    lines.join("\n")
}

fn parse_did_key(did: &str) -> Result<VerifyingKey, SignatureError> {
    let multibase = did.strip_prefix("did:key:").ok_or_else(not_p256)?;
    let (_, bytes) = multibase::decode(multibase).map_err(|_| not_p256())?;
    match bytes.as_slice() {
        [0x80, 0x24, point @ ..] => VerifyingKey::from_sec1_bytes(point).map_err(|_| not_p256()),
        _ => Err(not_p256()),
    }
}

fn malformed() -> SignatureError {
    SignatureError("missing or malformed atproto-space signature".into())
}

fn not_p256() -> SignatureError {
    SignatureError("signature key must be a P-256 did:key".into())
}

fn invalid() -> SignatureError {
    SignatureError("invalid HTTP message signature".into())
}

/// Sign a request as a syncer would, returning the fields to send.
pub fn sign(key: &SigningKey, authorization: &str, audience: Option<&str>) -> HeaderMap {
    use base64::Engine;
    use p256::ecdsa::signature::Signer;

    let params = match audience {
        Some(_) => format!("(\"authorization\" \"{AUDIENCE_HEADER}\")"),
        None => format!(
            "(\"authorization\");keyid=\"{}\"",
            did_key(key.verifying_key())
        ),
    };
    let signature: Signature =
        key.sign(signature_base(authorization, audience, &params).as_bytes());

    let mut headers = HeaderMap::new();
    let mut set = |name: &'static str, value: String| {
        headers.insert(
            name,
            value
                .parse()
                .expect("signature fields are valid header values"),
        );
    };
    set("authorization", authorization.to_string());
    if let Some(audience) = audience {
        set(AUDIENCE_HEADER, audience.to_string());
    }
    set("signature-input", format!("{SIGNATURE_LABEL}={params}"));
    set(
        "signature",
        format!(
            "{SIGNATURE_LABEL}=:{}:",
            base64::engine::general_purpose::STANDARD.encode(signature.to_bytes())
        ),
    );
    headers
}

/// The `did:key` for a P-256 public key.
pub fn did_key(key: &VerifyingKey) -> String {
    let mut bytes = vec![0x80, 0x24];
    bytes.extend_from_slice(key.to_sec1_point(true).as_bytes());
    format!(
        "did:key:{}",
        multibase::encode(multibase::Base::Base58Btc, bytes)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    const AUTHORIZATION: &str = "Atproto-Space credential";
    const AUDIENCE: &str = "did:example:repo";

    fn key() -> SigningKey {
        use rand::Rng;
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        SigningKey::from_slice(&bytes).unwrap()
    }

    fn set(headers: &mut HeaderMap, name: &'static str, value: &str) {
        headers.insert(name, HeaderValue::from_str(value).unwrap());
    }

    fn credential_headers(key: &SigningKey) -> HeaderMap {
        sign(key, AUTHORIZATION, Some(AUDIENCE))
    }

    fn signature_bytes(headers: &HeaderMap) -> Vec<u8> {
        use base64::Engine;
        let value = headers["signature"].to_str().unwrap();
        let b64 = value
            .strip_prefix("atproto-space=:")
            .and_then(|v| v.strip_suffix(':'))
            .unwrap();
        base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap()
    }

    fn set_signature_bytes(headers: &mut HeaderMap, bytes: &[u8]) {
        use base64::Engine;
        let value = format!(
            "atproto-space=:{}:",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        );
        set(headers, "signature", &value);
    }

    // Vectors produced by Node's crypto from the reference construction, so they
    // do not depend on this module's signing.
    const VECTOR_DID_KEY: &str = "did:key:zDnaeQiK4oVWdmHGDc9HvuLvvyrvppkzFYkPakrr7iXyP1eXX";

    #[test]
    fn verifies_a_credential_signature_made_elsewhere() {
        let mut headers = HeaderMap::new();
        set(&mut headers, "authorization", "Atproto-Space credential");
        set(&mut headers, AUDIENCE_HEADER, "did:example:repo");
        set(
            &mut headers,
            "signature-input",
            r#"atproto-space=("authorization" "atproto-space-audience")"#,
        );
        set(
            &mut headers,
            "signature",
            "atproto-space=:TfMj1VojnwgRBDLpQSEltRHERV1/OQmWLtuCrGzN4kN6pCcCI6fWdxtQmalZhHMSnJpKsln34Fa98iTds/1jeg==:",
        );
        assert_eq!(
            verify(&headers, Some(VECTOR_DID_KEY)).unwrap(),
            VECTOR_DID_KEY
        );
    }

    #[test]
    fn verifies_a_delegation_signature_made_elsewhere() {
        let mut headers = HeaderMap::new();
        set(&mut headers, "authorization", "Bearer delegation");
        set(
            &mut headers,
            "signature-input",
            &format!(
                r#"atproto-space=("authorization");keyid="{VECTOR_DID_KEY}";alg="ecdsa-p256-sha256""#
            ),
        );
        set(
            &mut headers,
            "signature",
            "atproto-space=:47+mFjZje8laMDP30+OIC99ILM5XWpMeu4Oz2UkhvwRvRG9d3QCYxUlcdS7Hk2sbFoDJy+pfCpKU+tsPdVo1Yw==:",
        );
        assert_eq!(verify(&headers, None).unwrap(), VECTOR_DID_KEY);
    }

    #[test]
    fn a_did_key_round_trips_to_the_same_public_key() {
        let key = key();
        let did = did_key(key.verifying_key());
        assert!(did.starts_with("did:key:zDna"), "{did}");
        assert_eq!(&parse_did_key(&did).unwrap(), key.verifying_key());
    }

    #[test]
    fn signs_what_it_verifies() {
        let key = key();
        let did = did_key(key.verifying_key());
        assert_eq!(verify(&credential_headers(&key), Some(&did)).unwrap(), did);
        let delegation = sign(&key, "Bearer delegation", None);
        assert_eq!(verify(&delegation, None).unwrap(), did);
    }

    #[test]
    fn a_delegation_signature_is_not_a_credential_signature() {
        let key = key();
        let did = did_key(key.verifying_key());
        let delegation = sign(&key, "Bearer delegation", None);
        assert!(verify(&delegation, Some(&did)).is_err());
        assert!(verify(&credential_headers(&key), None).is_err());
    }

    #[test]
    fn accepts_a_high_s_signature() {
        let key = key();
        let did = did_key(key.verifying_key());
        let mut headers = credential_headers(&key);
        let sig = Signature::from_slice(&signature_bytes(&headers)).unwrap();
        let (r, s) = sig.split_scalars();
        let high = Signature::from_scalars(r, -*s).unwrap();
        assert_ne!(high.to_bytes(), sig.to_bytes());
        set_signature_bytes(&mut headers, &high.to_bytes());
        assert_eq!(verify(&headers, Some(&did)).unwrap(), did);
    }

    #[test]
    fn accepts_other_parameters_in_any_order_and_other_labels() {
        let key = key();
        let did = did_key(key.verifying_key());
        let input = format!(
            r#"("authorization" "atproto-space-audience");alg="ecdsa-p256-sha256";keyid="{did}";created=1738368000"#
        );
        let base = format!(
            "\"authorization\": {AUTHORIZATION}\n\"atproto-space-audience\": {AUDIENCE}\n\"@signature-params\": {input}"
        );
        let sig: Signature = p256::ecdsa::signature::Signer::sign(&key, base.as_bytes());
        let mut headers = HeaderMap::new();
        set(&mut headers, "authorization", AUTHORIZATION);
        set(&mut headers, AUDIENCE_HEADER, AUDIENCE);
        set(
            &mut headers,
            "signature-input",
            &format!(r#"other=("authorization");keyid="other", atproto-space={input}"#),
        );
        use base64::Engine;
        set(
            &mut headers,
            "signature",
            &format!(
                "other=:YWJj:, atproto-space=:{}:",
                base64::engine::general_purpose::STANDARD.encode(sig.to_bytes())
            ),
        );
        assert_eq!(verify(&headers, Some(&did)).unwrap(), did);
    }

    #[test]
    fn rejects_changed_covered_fields() {
        let key = key();
        let did = did_key(key.verifying_key());
        for name in ["authorization", AUDIENCE_HEADER] {
            let mut headers = credential_headers(&key);
            let changed = format!("{}-changed", headers[name].to_str().unwrap());
            set(&mut headers, name, &changed);
            assert!(verify(&headers, Some(&did)).is_err(), "{name}");
        }
    }

    #[test]
    fn rejects_duplicate_covered_fields() {
        let key = key();
        let did = did_key(key.verifying_key());
        for name in ["authorization", AUDIENCE_HEADER] {
            let mut headers = credential_headers(&key);
            let value = headers[name].clone();
            headers.append(name, value);
            let err = verify(&headers, Some(&did)).unwrap_err();
            assert!(err.0.contains("exactly one"), "{name}: {err}");
        }
    }

    #[test]
    fn rejects_a_missing_field() {
        let key = key();
        let did = did_key(key.verifying_key());
        for name in [
            "authorization",
            AUDIENCE_HEADER,
            "signature-input",
            "signature",
        ] {
            let mut headers = credential_headers(&key);
            headers.remove(name);
            assert!(verify(&headers, Some(&did)).is_err(), "{name}");
        }
    }

    #[test]
    fn rejects_an_audience_the_signature_does_not_cover() {
        let key = key();
        let did = did_key(key.verifying_key());
        let mut headers = sign(&key, AUTHORIZATION, None);
        set(&mut headers, AUDIENCE_HEADER, AUDIENCE);
        assert!(
            verify(&headers, Some(&did))
                .unwrap_err()
                .0
                .contains("must cover")
        );
    }

    #[test]
    fn rejects_another_signing_key() {
        let key = key();
        let other = self::key();
        let headers = credential_headers(&other);
        assert!(verify(&headers, Some(&did_key(key.verifying_key()))).is_err());
    }

    #[test]
    fn rejects_a_keyid_that_differs_from_the_bound_key() {
        let key = key();
        let did = did_key(key.verifying_key());
        let other = did_key(self::key().verifying_key());
        let mut headers = credential_headers(&key);
        let input = format!(
            "{};keyid=\"{other}\"",
            headers["signature-input"].to_str().unwrap()
        );
        set(&mut headers, "signature-input", &input);
        assert!(
            verify(&headers, Some(&did))
                .unwrap_err()
                .0
                .contains("keyid")
        );
    }

    #[test]
    fn rejects_a_missing_or_invalid_delegation_keyid() {
        let key = key();
        for params in ["", ";keyid", ";keyid=123", ";keyid=\"not-a-key\""] {
            let mut headers = sign(&key, "Bearer delegation", None);
            set(
                &mut headers,
                "signature-input",
                &format!(r#"atproto-space=("authorization"){params}"#),
            );
            assert!(verify(&headers, None).is_err(), "{params:?}");
        }
    }

    #[test]
    fn rejects_other_components() {
        let key = key();
        let did = did_key(key.verifying_key());
        for components in [
            r#"("atproto-space-audience")"#,
            r#"("atproto-space-audience" "authorization")"#,
            r#"("authorization" "atproto-space-audience" "content-type")"#,
            r#"("authorization" "authorization" "atproto-space-audience")"#,
            r#"("authorization";sf "atproto-space-audience")"#,
            "not-a-list",
        ] {
            let mut headers = credential_headers(&key);
            set(
                &mut headers,
                "signature-input",
                &format!("atproto-space={components}"),
            );
            assert!(verify(&headers, Some(&did)).is_err(), "{components}");
        }
    }

    #[test]
    fn rejects_changed_signature_parameters() {
        let key = key();
        let did = did_key(key.verifying_key());
        let mut headers = credential_headers(&key);
        let input = format!(
            "{};alg=\"ecdsa-p256-sha256\"",
            headers["signature-input"].to_str().unwrap()
        );
        set(&mut headers, "signature-input", &input);
        assert!(verify(&headers, Some(&did)).is_err());
    }

    #[test]
    fn rejects_other_algorithms() {
        let key = key();
        let did = did_key(key.verifying_key());
        let mut headers = credential_headers(&key);
        let input = format!(
            "{};alg=\"ecdsa-p384-sha384\"",
            headers["signature-input"].to_str().unwrap()
        );
        set(&mut headers, "signature-input", &input);
        assert!(
            verify(&headers, Some(&did))
                .unwrap_err()
                .0
                .contains("algorithm")
        );
    }

    #[test]
    fn rejects_a_secp256k1_key() {
        let k256 = "did:key:zQ3shokFTS3brHcDQrn82RUDfCZESWL1ZdCEJwekUDPQiYBme";
        let key = key();
        assert!(
            verify(&credential_headers(&key), Some(k256))
                .unwrap_err()
                .0
                .contains("P-256")
        );
    }

    #[test]
    fn rejects_malformed_signatures() {
        let key = key();
        let did = did_key(key.verifying_key());
        for signature in ["not-a-byte-sequence", ":YWJj:", ":!!!:"] {
            let mut headers = credential_headers(&key);
            set(
                &mut headers,
                "signature",
                &format!("atproto-space={signature}"),
            );
            assert!(verify(&headers, Some(&did)).is_err(), "{signature}");
        }
    }

    #[test]
    fn rejects_a_der_encoded_signature() {
        let key = key();
        let did = did_key(key.verifying_key());
        let mut headers = credential_headers(&key);
        let sig = Signature::from_slice(&signature_bytes(&headers)).unwrap();
        set_signature_bytes(&mut headers, sig.to_der().as_bytes());
        assert!(verify(&headers, Some(&did)).is_err());
    }
}
