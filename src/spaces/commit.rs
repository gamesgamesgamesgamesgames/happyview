use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use hkdf::Hkdf;
use hmac::{Hmac, KeyInit, Mac};
use rand::Rng;
use sha2::Sha256;

use crate::error::AppError;

pub struct SignedCommit {
    pub ver: u32,
    pub hash: [u8; 32],
    pub ikm: [u8; 32],
    /// `sign(context)` by the repo's signing key.
    ///
    /// Covers `(space, author, rev, ikm)` but not `hash`, which keeps a leaked
    /// commit deniable: it proves the author signed a context, not what they
    /// wrote. `mac` binds the hash to that context.
    pub sig: Vec<u8>,
    pub mac: [u8; 32],
    pub rev: String,
}

/// Decode a lexicon `bytes` field of a signed commit.
///
/// The lexicon types these as `bytes`, which atproto renders in JSON as
/// `{"$bytes": "<standard base64>"}`, not as a bare string and not base64url.
/// The reference fixtures omit padding while ZDS emits it, so both are accepted.
pub fn decode_lex_bytes(value: &serde_json::Value, field: &str) -> Result<Vec<u8>, AppError> {
    let raw = value
        .get(field)
        .and_then(|v| v.get("$bytes"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            AppError::Internal(format!("commit {field} is not a lexicon bytes value"))
        })?;
    STANDARD_NO_PAD
        .decode(raw.trim_end_matches('='))
        .map_err(|e| AppError::Internal(format!("commit {field} is not base64: {e}")))
}

/// Render a lexicon `bytes` field of a signed commit, unpadded as the
/// reference PDS emits it.
pub fn encode_lex_bytes(bytes: &[u8]) -> serde_json::Value {
    serde_json::json!({ "$bytes": STANDARD_NO_PAD.encode(bytes) })
}

/// A signing key resolved from a DID document.
///
/// atproto accounts sign with secp256k1 or NIST P-256, so a repo host must
/// verify both. HappyView's own service key is P-256.
pub enum SpaceVerifyingKey {
    P256(p256::ecdsa::VerifyingKey),
    K256(k256::ecdsa::VerifyingKey),
}

impl SpaceVerifyingKey {
    pub fn verify(&self, message: &[u8], sig: &[u8]) -> Result<(), AppError> {
        use p256::ecdsa::signature::Verifier;
        let ok = match self {
            SpaceVerifyingKey::P256(key) => p256::ecdsa::Signature::from_slice(sig)
                .ok()
                .is_some_and(|s| key.verify(message, &s).is_ok()),
            SpaceVerifyingKey::K256(key) => k256::ecdsa::Signature::from_slice(sig)
                .ok()
                .is_some_and(|s| {
                    // Accept either S form; atproto normalizes to low-S but not
                    // every signer does.
                    key.verify(message, &s).is_ok() || key.verify(message, &s.normalize_s()).is_ok()
                }),
        };

        if ok {
            Ok(())
        } else {
            Err(AppError::Auth(
                "commit signature verification failed".into(),
            ))
        }
    }
}

pub fn build_context(space_uri: &str, author_did: &str, rev: &str, ikm: &[u8; 32]) -> Vec<u8> {
    let tag = b"atproto-space-v1";
    let space_bytes = space_uri.as_bytes();
    let author_bytes = author_did.as_bytes();
    let rev_bytes = rev.as_bytes();

    let mut ctx = Vec::with_capacity(
        tag.len() + 2 + space_bytes.len() + 2 + author_bytes.len() + 2 + rev_bytes.len() + 2 + 32,
    );

    ctx.extend_from_slice(tag);

    // TLS 1.3 variable-length encoding: big-endian uint16 length prefix
    ctx.extend_from_slice(&(space_bytes.len() as u16).to_be_bytes());
    ctx.extend_from_slice(space_bytes);

    ctx.extend_from_slice(&(author_bytes.len() as u16).to_be_bytes());
    ctx.extend_from_slice(author_bytes);

    ctx.extend_from_slice(&(rev_bytes.len() as u16).to_be_bytes());
    ctx.extend_from_slice(rev_bytes);

    ctx.extend_from_slice(&(ikm.len() as u16).to_be_bytes());
    ctx.extend_from_slice(ikm);

    ctx
}

/// Derive the MAC key from the per-commit `ikm` and context.
///
/// The `ikm` is the HKDF pseudorandom key: Expand only, no Extract step,
/// because `ikm` is already 32 uniformly random bytes. This matches
/// `expand(sha256, ikm, info, 32)` in `@atproto/crypto`'s `hkdfSha256`.
/// `Hkdf::new` would run Extract first and derive a key no other
/// implementation produces, so our MACs would verify only against our own.
fn derive_mac_key(ikm: &[u8; 32], ctx: &[u8]) -> Result<[u8; 32], AppError> {
    let hk = Hkdf::<Sha256>::from_prk(ikm)
        .map_err(|e| AppError::Internal(format!("HKDF from_prk failed: {e}")))?;
    let mut derived = [0u8; 32];
    hk.expand(ctx, &mut derived)
        .map_err(|e| AppError::Internal(format!("HKDF expand failed: {e}")))?;
    Ok(derived)
}

/// `mac = HMAC-SHA256(HKDF-Expand(ikm, context, 32), hash)`.
fn compute_mac(ikm: &[u8; 32], ctx: &[u8], hash: &[u8; 32]) -> Result<[u8; 32], AppError> {
    let key = derive_mac_key(ikm, ctx)?;
    let mut hasher = <Hmac<Sha256> as KeyInit>::new_from_slice(&key)
        .map_err(|e| AppError::Internal(format!("HMAC init failed: {e}")))?;
    hasher.update(hash);
    Ok(hasher.finalize().into_bytes().into())
}

pub fn sign_commit(
    hash: &[u8; 32],
    space_uri: &str,
    author_did: &str,
    rev: &str,
    signing_key: &p256::ecdsa::SigningKey,
) -> Result<SignedCommit, AppError> {
    use p256::ecdsa::signature::Signer;

    let mut ikm = [0u8; 32];
    rand::rng().fill_bytes(&mut ikm);

    let ctx = build_context(space_uri, author_did, rev, &ikm);
    let mac = compute_mac(&ikm, &ctx, hash)?;

    // Signs the context, not the hash; see `SignedCommit::sig`.
    let sig: p256::ecdsa::Signature = signing_key.sign(&ctx);

    Ok(SignedCommit {
        ver: 1,
        hash: *hash,
        ikm,
        sig: sig.to_bytes().to_vec(),
        mac,
        rev: rev.to_string(),
    })
}

/// Verify a commit's authenticity, then its integrity.
///
/// The signature establishes who produced this context. The MAC only means
/// something after that, because it is keyed from the public `ikm`: anyone
/// holding the commit can compute a valid MAC for any hash.
pub fn verify_commit(
    commit: &SignedCommit,
    space_uri: &str,
    author_did: &str,
    key: &SpaceVerifyingKey,
) -> Result<(), AppError> {
    if commit.ver != 1 {
        return Err(AppError::BadRequest(format!(
            "unsupported commit version: {}",
            commit.ver
        )));
    }

    let ctx = build_context(space_uri, author_did, &commit.rev, &commit.ikm);

    key.verify(&ctx, &commit.sig)?;

    let expected = compute_mac(&commit.ikm, &ctx, &commit.hash)?;
    if !crate::constant_time::ct_eq(&expected, &commit.mac) {
        return Err(AppError::Auth(
            "commit MAC verification failed — repo hash mismatch".into(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------
    // Interop vectors
    //
    // Generated by scripts/gen-space-vectors.mjs from the same @noble/hashes
    // primitives @atproto/crypto builds on. They pin the wire format to the
    // reference implementation, which a sign/verify roundtrip cannot do: our
    // own code always agrees with itself.
    // ---------------------------------------------------------------------

    #[derive(serde::Deserialize)]
    struct CommitVector {
        space: String,
        author: String,
        rev: String,
        ikm_hex: String,
        hash_hex: String,
        context_hex: String,
        derived_key_hex: String,
        mac_hex: String,
    }

    #[derive(serde::Deserialize)]
    struct CommitVectorFile {
        vectors: Vec<CommitVector>,
    }

    fn load_vectors() -> Vec<CommitVector> {
        let raw = include_str!("../../tests/fixtures/space_commit_vectors.json");
        let parsed: CommitVectorFile = serde_json::from_str(raw).expect("vector fixture parses");
        assert!(!parsed.vectors.is_empty(), "fixture must carry vectors");
        parsed.vectors
    }

    fn from_hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
            .collect()
    }

    fn hex_encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn context_matches_reference_vectors() {
        for v in load_vectors() {
            let ikm: [u8; 32] = from_hex(&v.ikm_hex).try_into().expect("32-byte ikm");
            let ctx = build_context(&v.space, &v.author, &v.rev, &ikm);
            assert_eq!(
                hex_encode(&ctx),
                v.context_hex,
                "context mismatch for space {}",
                v.space
            );
        }
    }

    #[test]
    fn mac_matches_reference_vectors() {
        for v in load_vectors() {
            let ikm: [u8; 32] = from_hex(&v.ikm_hex).try_into().expect("32-byte ikm");
            let hash: [u8; 32] = from_hex(&v.hash_hex).try_into().expect("32-byte hash");
            let ctx = build_context(&v.space, &v.author, &v.rev, &ikm);

            let derived = derive_mac_key(&ikm, &ctx).expect("derive");
            assert_eq!(
                hex_encode(&derived),
                v.derived_key_hex,
                "derived key mismatch for space {}: HKDF must be expand-only",
                v.space
            );

            let mac = compute_mac(&ikm, &ctx, &hash).expect("mac");
            assert_eq!(
                hex_encode(&mac),
                v.mac_hex,
                "mac mismatch for space {}",
                v.space
            );
        }
    }

    #[test]
    fn context_string_format() {
        let ctx = build_context(
            "at://did:plc:abc/space/com.example.forum/main",
            "did:plc:testuser",
            "3k2abc",
            &[0xAA; 32],
        );
        // Starts with protocol tag
        assert!(ctx.starts_with(b"atproto-space-v1"));
    }

    #[test]
    fn context_includes_all_fields() {
        let space = "at://did:plc:abc/space/com.example.forum/main";
        let author = "did:plc:testuser";
        let rev = "3k2abc";
        let ikm = [0xBB; 32];
        let ctx = build_context(space, author, rev, &ikm);

        // Context must contain the space URI, author, rev, and ikm
        assert!(ctx.windows(space.len()).any(|w| w == space.as_bytes()));
        assert!(ctx.windows(author.len()).any(|w| w == author.as_bytes()));
        assert!(ctx.windows(rev.len()).any(|w| w == rev.as_bytes()));
        assert!(ctx.windows(32).any(|w| w == ikm));
    }

    #[test]
    fn context_includes_author_did() {
        let space = "at://did:plc:abc/space/com.example.forum/main";
        let author = "did:plc:user1";
        let rev = "3k2abc";
        let ikm = [0xBB; 32];
        let ctx = build_context(space, author, rev, &ikm);

        assert!(ctx.starts_with(b"atproto-space-v1"));
        assert!(ctx.windows(author.len()).any(|w| w == author.as_bytes()));

        // Author must appear after space and before rev in the byte stream
        let space_pos = ctx
            .windows(space.len())
            .position(|w| w == space.as_bytes())
            .unwrap();
        let author_pos = ctx
            .windows(author.len())
            .position(|w| w == author.as_bytes())
            .unwrap();
        let rev_pos = ctx
            .windows(rev.len())
            .position(|w| w == rev.as_bytes())
            .unwrap();
        assert!(space_pos < author_pos);
        assert!(author_pos < rev_pos);
    }

    /// Fixed key so failures are reproducible.
    fn test_key() -> p256::ecdsa::SigningKey {
        p256::ecdsa::SigningKey::from_slice(&[0x42u8; 32]).expect("valid test key")
    }

    fn test_verifier() -> SpaceVerifyingKey {
        SpaceVerifyingKey::P256(*test_key().verifying_key())
    }

    #[test]
    fn sign_produces_verifiable_signature() {
        let space = "at://did:plc:abc/space/com.example.forum/main";
        let commit = sign_commit(
            &[0xCCu8; 32],
            space,
            "did:plc:testuser",
            "3k2rev1",
            &test_key(),
        )
        .unwrap();

        assert_eq!(commit.ver, 1);
        assert!(!commit.sig.is_empty(), "commit must carry a signature");
        verify_commit(&commit, space, "did:plc:testuser", &test_verifier()).expect("verifies");
    }

    #[test]
    fn verify_rejects_signature_from_another_key() {
        // The MAC still checks out; only the signature tells this author's
        // commit apart from a forgery.
        let other = p256::ecdsa::SigningKey::from_slice(&[0x43u8; 32]).unwrap();
        let wrong = SpaceVerifyingKey::P256(*other.verifying_key());

        let space = "at://did:plc:abc/space/com.example.forum/main";
        let commit = sign_commit(
            &[0xCCu8; 32],
            space,
            "did:plc:testuser",
            "3k2rev1",
            &test_key(),
        )
        .unwrap();

        assert!(verify_commit(&commit, space, "did:plc:testuser", &wrong).is_err());
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let hash = [0xCC; 32];
        let space = "at://did:plc:abc/space/com.example.forum/main";

        let commit = sign_commit(&hash, space, "did:plc:testuser", "3k2rev1", &test_key()).unwrap();

        assert_eq!(commit.hash, hash);
        assert_eq!(commit.rev, "3k2rev1");
        assert_eq!(commit.mac.len(), 32);
        assert_eq!(commit.ver, 1);

        assert!(verify_commit(&commit, space, "did:plc:testuser", &test_verifier()).is_ok());
    }

    #[test]
    fn commit_has_version() {
        let hash = [0xCC; 32];
        let space = "at://did:plc:abc/space/com.example.forum/main";
        let commit = sign_commit(&hash, space, "did:plc:testuser", "rev1", &test_key()).unwrap();
        assert_eq!(commit.ver, 1);
    }

    #[test]
    fn verify_rejects_wrong_author() {
        let hash = [0xAA; 32];
        let space = "at://did:plc:abc/space/com.example.forum/main";

        let commit = sign_commit(&hash, space, "did:plc:user1", "rev1", &test_key()).unwrap();
        assert!(verify_commit(&commit, space, "did:plc:user1", &test_verifier()).is_ok());
        assert!(verify_commit(&commit, space, "did:plc:user2", &test_verifier()).is_err());
    }

    #[test]
    fn verify_rejects_tampered_hash() {
        let hash = [0xEE; 32];
        let space = "at://did:plc:abc/space/com.example.forum/main";

        let mut commit =
            sign_commit(&hash, space, "did:plc:testuser", "rev1", &test_key()).unwrap();
        commit.hash[0] ^= 0xFF; // tamper
        assert!(verify_commit(&commit, space, "did:plc:testuser", &test_verifier()).is_err());
    }

    #[test]
    fn verify_rejects_tampered_ikm() {
        let hash = [0xEE; 32];
        let space = "at://did:plc:abc/space/com.example.forum/main";

        let mut commit =
            sign_commit(&hash, space, "did:plc:testuser", "rev1", &test_key()).unwrap();
        commit.ikm[0] ^= 0xFF; // tamper — changes both the HKDF salt and the context
        assert!(verify_commit(&commit, space, "did:plc:testuser", &test_verifier()).is_err());
    }

    #[test]
    fn verify_rejects_tampered_rev() {
        let hash = [0xEE; 32];
        let space = "at://did:plc:abc/space/com.example.forum/main";

        let mut commit =
            sign_commit(&hash, space, "did:plc:testuser", "rev1", &test_key()).unwrap();
        commit.rev = "rev2".into();
        assert!(verify_commit(&commit, space, "did:plc:testuser", &test_verifier()).is_err());
    }

    #[test]
    fn verify_rejects_wrong_space() {
        let hash = [0xFF; 32];

        let commit = sign_commit(
            &hash,
            "at://did:plc:abc/space/com.example.forum/main",
            "did:plc:user",
            "rev1",
            &test_key(),
        )
        .unwrap();
        assert!(
            verify_commit(
                &commit,
                "at://did:plc:xyz/space/com.example.forum/other",
                "did:plc:user",
                &test_verifier(),
            )
            .is_err()
        );
    }

    #[test]
    fn different_ikm_per_commit() {
        let hash = [0xAA; 32];
        let space = "at://did:plc:abc/space/com.example.forum/main";

        let c1 = sign_commit(&hash, space, "did:plc:testuser", "rev1", &test_key()).unwrap();
        let c2 = sign_commit(&hash, space, "did:plc:testuser", "rev1", &test_key()).unwrap();

        // Each call generates fresh ikm
        assert_ne!(c1.ikm, c2.ikm);
        assert_ne!(c1.mac, c2.mac);
        // But both verify
        assert!(verify_commit(&c1, space, "did:plc:testuser", &test_verifier()).is_ok());
        assert!(verify_commit(&c2, space, "did:plc:testuser", &test_verifier()).is_ok());
    }

    #[test]
    fn verify_rejects_unknown_version() {
        let hash = [0xCC; 32];
        let space = "at://did:plc:abc/space/com.example.forum/main";

        let mut commit =
            sign_commit(&hash, space, "did:plc:testuser", "rev1", &test_key()).unwrap();
        commit.ver = 2;
        assert!(verify_commit(&commit, space, "did:plc:testuser", &test_verifier()).is_err());
    }

    #[test]
    fn verify_rejects_tampered_mac() {
        let hash = [0xCC; 32];
        let space = "at://did:plc:abc/space/com.example.forum/main";

        let mut commit =
            sign_commit(&hash, space, "did:plc:testuser", "rev1", &test_key()).unwrap();
        assert!(verify_commit(&commit, space, "did:plc:testuser", &test_verifier()).is_ok());
        commit.mac[0] ^= 0xFF;
        assert!(verify_commit(&commit, space, "did:plc:testuser", &test_verifier()).is_err());
    }
}
