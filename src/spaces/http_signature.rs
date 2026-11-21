//! Proof that a request comes from the holder of a space credential's key.
//!
//! A space credential reads a whole space, so as a bearer token it would be a
//! shared secret: a repo host handed one to serve one repo could replay it
//! against every other host in the space. Instead each credential is bound to a
//! P-256 key the syncer holds (`cnf.kid`), and each request carries an
//! [RFC 9421](https://www.rfc-editor.org/rfc/rfc9421) HTTP Message Signature by
//! that key over the `Authorization` field and the `Atproto-Space-Audience`
//! field naming the DID the request is for.
//!
//! Proposal 0016 fixes the signature's `tag` as `atproto-space-credential`,
//! its covered components as exactly `("authorization"
//! "atproto-space-audience")`, and its algorithm as `ecdsa-p256-sha256`,
//! accepting high-S signatures.
//!
//! Which signature is the credential's differs between revisions. The Oct 29
//! text names it by its label, `atproto-space-credential`. The Nov 12 text
//! (proposals #143) finds it by `tag`, whatever its label, because
//! intermediaries may relabel signatures, and its reference signs under `sig`.
//! [`verify`] accepts both: exactly one signature carrying the tag, or, when
//! none does, the one under the Oct 29 label.

use axum::http::HeaderMap;
use axum::http::StatusCode;
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};

use crate::error::AppError;

/// The label of the signature this protocol reads, among any others present.
pub const SIGNATURE_LABEL: &str = "atproto-space-credential";
/// The `tag` parameter the signature must carry.
pub const SIGNATURE_TAG: &str = "atproto-space-credential";
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

/// Verify a request's `atproto-space-credential` signature against the key
/// its credential is bound to.
///
/// The audience is checked for presence here and against the request's target
/// by the handler, which is the only place that knows the target.
pub fn verify(headers: &HeaderMap, bound_key: &str) -> Result<(), SignatureError> {
    let authorization = exactly_one(headers, "authorization")?;
    let audience = exactly_one(headers, AUDIENCE_HEADER)?;

    let (input, signature) = select(headers)?;
    let signature = match signature.bare_item.as_byte_sequence() {
        Some(bytes) if signature.params.is_empty() => bytes.to_vec(),
        _ => return Err(malformed()),
    };

    let expected = ["authorization", AUDIENCE_HEADER];
    let covers_exactly = input.items.len() == expected.len()
        && input.items.iter().zip(expected).all(|(item, want)| {
            item.params.is_empty() && item.bare_item.as_string().map(|s| s.as_str()) == Some(want)
        });
    if !covers_exactly {
        return Err(SignatureError(format!(
            "signature must cover exactly {expected:?}, in order"
        )));
    }

    // Selected by its label alone, an Oct 29 signature may omit the tag, but
    // may not carry another one.
    if let Some(tag) = input.params.get("tag")
        && tag.as_string().map(|s| s.as_str()) != Some(SIGNATURE_TAG)
    {
        return Err(SignatureError(format!(
            "signature tag must be \"{SIGNATURE_TAG}\""
        )));
    }
    if let Some(alg) = input.params.get("alg")
        && alg.as_string().map(|s| s.as_str()) != Some(SIGNATURE_ALG)
    {
        return Err(SignatureError(format!(
            "signature algorithm must be {SIGNATURE_ALG}"
        )));
    }
    if let Some(keyid) = input.params.get("keyid")
        && keyid.as_string().map(|s| s.as_str()) != Some(bound_key)
    {
        return Err(SignatureError(
            "signature keyid does not match the credential key".into(),
        ));
    }
    let verifying_key = parse_did_key(bound_key)?;

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
        .map_err(|_| invalid())
}

/// Whether `did` is a P-256 `did:key`, the only key a credential can be bound
/// to.
pub fn is_p256_did_key(did: &str) -> bool {
    parse_did_key(did).is_ok()
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

/// A signature dictionary field, which may carry other labels, and may
/// arrive split across several field lines.
fn dictionary(headers: &HeaderMap, name: &str) -> Result<sfv::Dictionary, SignatureError> {
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
    sfv::Parser::new(&joined).parse().map_err(|_| malformed())
}

/// The credential's signature among those present: the only one tagged
/// [`SIGNATURE_TAG`], under any label (Nov 12), or, when none is tagged, the
/// one labelled [`SIGNATURE_LABEL`] (Oct 29). Two tagged signatures are
/// refused rather than guessed between.
fn select(headers: &HeaderMap) -> Result<(sfv::InnerList, sfv::Item), SignatureError> {
    let mut inputs = dictionary(headers, "signature-input")?;
    let mut signatures = dictionary(headers, "signature")?;
    let tagged: Vec<String> = inputs
        .iter()
        .filter_map(|(label, entry)| match entry {
            sfv::ListEntry::InnerList(list)
                if list
                    .params
                    .get("tag")
                    .and_then(|t| t.as_string())
                    .map(|s| s.as_str())
                    == Some(SIGNATURE_TAG) =>
            {
                Some(label.as_str().to_string())
            }
            _ => None,
        })
        .collect();
    let label = match tagged.as_slice() {
        [label] => label.clone(),
        [] => SIGNATURE_LABEL.to_string(),
        _ => {
            return Err(SignatureError(format!(
                "request carries more than one signature tagged \"{SIGNATURE_TAG}\""
            )));
        }
    };
    let input = inputs.shift_remove(label.as_str()).ok_or_else(malformed)?;
    let signature = signatures
        .shift_remove(label.as_str())
        .ok_or_else(malformed)?;
    match (input, signature) {
        (sfv::ListEntry::InnerList(input), sfv::ListEntry::Item(signature)) => {
            Ok((input, signature))
        }
        _ => Err(malformed()),
    }
}

fn signature_base(authorization: &str, audience: &str, params: &str) -> String {
    [
        format!("\"authorization\": {}", authorization.trim()),
        format!("\"{AUDIENCE_HEADER}\": {}", audience.trim()),
        format!("\"@signature-params\": {params}"),
    ]
    .join("\n")
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
    SignatureError("missing or malformed space credential signature".into())
}

fn not_p256() -> SignatureError {
    SignatureError("signature key must be a P-256 did:key".into())
}

fn invalid() -> SignatureError {
    SignatureError("invalid HTTP message signature".into())
}

/// Sign a request as a syncer would, returning the fields to send.
pub fn sign(key: &SigningKey, authorization: &str, audience: &str) -> HeaderMap {
    use base64::Engine;
    use p256::ecdsa::signature::Signer;

    let params = format!("(\"authorization\" \"{AUDIENCE_HEADER}\");tag=\"{SIGNATURE_TAG}\"");
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
    set(AUDIENCE_HEADER, audience.to_string());
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

    const AUTHORIZATION: &str = "Atproto-Space-Credential credential";
    const AUDIENCE: &str = "did:example:repo";
    const TAGGED: &str =
        r#"("authorization" "atproto-space-audience");tag="atproto-space-credential""#;

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
        sign(key, AUTHORIZATION, AUDIENCE)
    }

    fn signature_bytes(headers: &HeaderMap) -> Vec<u8> {
        use base64::Engine;
        let value = headers["signature"].to_str().unwrap();
        let b64 = value
            .strip_prefix("atproto-space-credential=:")
            .and_then(|v| v.strip_suffix(':'))
            .unwrap();
        base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap()
    }

    fn set_signature_bytes(headers: &mut HeaderMap, bytes: &[u8]) {
        use base64::Engine;
        let value = format!(
            "atproto-space-credential=:{}:",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        );
        set(headers, "signature", &value);
    }

    fn signed_with_input(key: &SigningKey, input: &str) -> HeaderMap {
        let base = format!(
            "\"authorization\": {AUTHORIZATION}\n\"atproto-space-audience\": {AUDIENCE}\n\"@signature-params\": {input}"
        );
        let sig: Signature = p256::ecdsa::signature::Signer::sign(key, base.as_bytes());
        let mut headers = credential_headers(key);
        set(
            &mut headers,
            "signature-input",
            &format!("atproto-space-credential={input}"),
        );
        set_signature_bytes(&mut headers, &sig.to_bytes());
        headers
    }

    // Produced by Node's crypto following `createSpaceSig` in `@atproto/space`
    // (atproto e72a939e), so it does not depend on this module's signing.
    const VECTOR_DID_KEY: &str = "did:key:zDnaej7saZxXxqdMGBKq1XeSVQ8vGwGKiJ4rydFPbKSxBY62t";

    #[test]
    fn verifies_a_credential_signature_made_elsewhere() {
        let mut headers = HeaderMap::new();
        set(&mut headers, "authorization", AUTHORIZATION);
        set(&mut headers, AUDIENCE_HEADER, AUDIENCE);
        set(
            &mut headers,
            "signature-input",
            &format!("atproto-space-credential={TAGGED}"),
        );
        set(
            &mut headers,
            "signature",
            "atproto-space-credential=:bSs3BExnPjJT7Y2N9b9pzfp18Gyc6eSjYl80O8aAMFPB6uvesxzPqOg9yz+DaxGp4guG9p789eR3QozdnyNzRQ==:",
        );
        verify(&headers, VECTOR_DID_KEY).unwrap();
    }

    #[test]
    fn a_did_key_round_trips_to_the_same_public_key() {
        let key = key();
        let did = did_key(key.verifying_key());
        assert!(did.starts_with("did:key:zDna"), "{did}");
        assert_eq!(&parse_did_key(&did).unwrap(), key.verifying_key());
        assert!(is_p256_did_key(&did));
        assert!(!is_p256_did_key("did:plc:abc"));
    }

    #[test]
    fn signs_what_it_verifies() {
        let key = key();
        let did = did_key(key.verifying_key());
        verify(&credential_headers(&key), &did).unwrap();
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
        verify(&headers, &did).unwrap();
    }

    #[test]
    fn accepts_other_parameters_in_any_order_and_other_labels() {
        let key = key();
        let did = did_key(key.verifying_key());
        let input = format!(
            r#"("authorization" "atproto-space-audience");alg="ecdsa-p256-sha256";keyid="{did}";tag="atproto-space-credential";created=1738368000"#
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
            &format!(r#"other=("authorization");keyid="other", atproto-space-credential={input}"#),
        );
        use base64::Engine;
        set(
            &mut headers,
            "signature",
            &format!(
                "other=:YWJj:, atproto-space-credential=:{}:",
                base64::engine::general_purpose::STANDARD.encode(sig.to_bytes())
            ),
        );
        verify(&headers, &did).unwrap();
    }

    #[test]
    fn rejects_a_wrong_tag() {
        let key = key();
        let did = did_key(key.verifying_key());
        for input in [
            r#"("authorization" "atproto-space-audience");tag="atproto-space""#,
            r#"("authorization" "atproto-space-audience");tag=1"#,
        ] {
            let err = verify(&signed_with_input(&key, input), &did).unwrap_err();
            assert!(err.0.contains("tag"), "{input}: {err}");
        }
        verify(&signed_with_input(&key, TAGGED), &did).unwrap();
    }

    /// Selected by the Oct 29 label, a signature may omit the tag.
    #[test]
    fn accepts_an_untagged_signature_under_the_october_29_label() {
        let key = key();
        let did = did_key(key.verifying_key());
        verify(
            &signed_with_input(&key, r#"("authorization" "atproto-space-audience")"#),
            &did,
        )
        .unwrap();
    }

    /// Relabel a signature's two members, as an intermediary may.
    fn relabelled(mut headers: HeaderMap, label: &str) -> HeaderMap {
        for name in ["signature-input", "signature"] {
            let value = headers[name].to_str().unwrap().replacen(
                "atproto-space-credential=",
                &format!("{label}="),
                1,
            );
            set(&mut headers, name, &value);
        }
        headers
    }

    /// Since Nov 12 the signature is found by its tag, whatever its label;
    /// the reference signs under `sig`.
    #[test]
    fn finds_a_tagged_signature_under_any_label() {
        let key = key();
        let did = did_key(key.verifying_key());
        for label in ["sig", "atproto-space", "sig1"] {
            verify(&relabelled(credential_headers(&key), label), &did)
                .unwrap_or_else(|e| panic!("{label}: {e}"));
        }
    }

    /// Without the tag, only the Oct 29 label selects a signature.
    #[test]
    fn rejects_an_untagged_signature_under_another_label() {
        let key = key();
        let did = did_key(key.verifying_key());
        let headers = relabelled(
            signed_with_input(&key, r#"("authorization" "atproto-space-audience")"#),
            "sig",
        );
        assert!(verify(&headers, &did).is_err());
    }

    /// Two signatures carrying the tag are refused rather than chosen between.
    #[test]
    fn rejects_two_tagged_signatures() {
        let key = key();
        let did = did_key(key.verifying_key());
        let headers = credential_headers(&key);
        let mut doubled = HeaderMap::new();
        for name in ["authorization", AUDIENCE_HEADER] {
            doubled.insert(name, headers[name].clone());
        }
        for name in ["signature-input", "signature"] {
            let value = headers[name].to_str().unwrap();
            let copy = value.replacen("atproto-space-credential=", "sig=", 1);
            set(&mut doubled, name, &format!("{value}, {copy}"));
        }
        let err = verify(&doubled, &did).unwrap_err();
        assert!(err.0.contains("more than one"), "{err}");
    }

    #[test]
    fn rejects_changed_covered_fields() {
        let key = key();
        let did = did_key(key.verifying_key());
        for name in ["authorization", AUDIENCE_HEADER] {
            let mut headers = credential_headers(&key);
            let changed = format!("{}-changed", headers[name].to_str().unwrap());
            set(&mut headers, name, &changed);
            assert!(verify(&headers, &did).is_err(), "{name}");
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
            let err = verify(&headers, &did).unwrap_err();
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
            assert!(verify(&headers, &did).is_err(), "{name}");
        }
    }

    #[test]
    fn rejects_another_signing_key() {
        let key = key();
        let other = self::key();
        let headers = credential_headers(&other);
        assert!(verify(&headers, &did_key(key.verifying_key())).is_err());
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
        assert!(verify(&headers, &did).unwrap_err().0.contains("keyid"));
    }

    #[test]
    fn rejects_other_components() {
        let key = key();
        let did = did_key(key.verifying_key());
        for components in [
            r#"("authorization");tag="atproto-space-credential""#,
            r#"("atproto-space-audience" "authorization");tag="atproto-space-credential""#,
            r#"("authorization" "atproto-space-audience" "content-type");tag="atproto-space-credential""#,
            r#"("authorization";sf "atproto-space-audience");tag="atproto-space-credential""#,
        ] {
            let mut headers = credential_headers(&key);
            set(
                &mut headers,
                "signature-input",
                &format!("atproto-space-credential={components}"),
            );
            let err = verify(&headers, &did).unwrap_err();
            assert!(err.0.contains("must cover exactly"), "{components}: {err}");
        }
        let mut headers = credential_headers(&key);
        set(
            &mut headers,
            "signature-input",
            "atproto-space-credential=not-a-list",
        );
        assert!(verify(&headers, &did).is_err());
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
        assert!(verify(&headers, &did).is_err());
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
        assert!(verify(&headers, &did).unwrap_err().0.contains("algorithm"));
    }

    #[test]
    fn rejects_a_secp256k1_key() {
        let k256 = "did:key:zQ3shokFTS3brHcDQrn82RUDfCZESWL1ZdCEJwekUDPQiYBme";
        let key = key();
        assert!(
            verify(&credential_headers(&key), k256)
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
                &format!("atproto-space-credential={signature}"),
            );
            assert!(verify(&headers, &did).is_err(), "{signature}");
        }
    }

    #[test]
    fn rejects_a_der_encoded_signature() {
        let key = key();
        let did = did_key(key.verifying_key());
        let mut headers = credential_headers(&key);
        let sig = Signature::from_slice(&signature_bytes(&headers)).unwrap();
        set_signature_bytes(&mut headers, sig.to_der().as_bytes());
        assert!(verify(&headers, &did).is_err());
    }
}
