use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use k256::ecdsa::{Signature, SigningKey, VerifyingKey, signature::Signer, signature::Verifier};
use sha2::Sha256;

use crate::error::AppError;

pub struct SignedCommit {
    pub ver: u32,
    pub hash: [u8; 32],
    pub ikm: [u8; 32],
    pub sig: Vec<u8>,
    pub mac: [u8; 32],
    pub rev: String,
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

pub fn sign_commit(
    hash: &[u8; 32],
    space_uri: &str,
    author_did: &str,
    rev: &str,
    signing_key: &SigningKey,
) -> Result<SignedCommit, AppError> {
    let mut ikm = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut ikm);

    let ctx = build_context(space_uri, author_did, rev, &ikm);

    // sig covers space + author + rev + ikm, NOT the hash — prevents rebroadcast proof
    let sig: Signature = signing_key.sign(&ctx);

    // mac = HMAC-SHA256(HKDF-SHA256(ikm, ctx), hash)
    let hk = Hkdf::<Sha256>::new(None, &ikm);
    let mut derived_key = [0u8; 32];
    hk.expand(&ctx, &mut derived_key)
        .map_err(|e| AppError::Internal(format!("HKDF expand failed: {e}")))?;

    let mut mac_hasher = <Hmac<Sha256> as Mac>::new_from_slice(&derived_key)
        .map_err(|e| AppError::Internal(format!("HMAC init failed: {e}")))?;
    mac_hasher.update(hash);
    let mac: [u8; 32] = mac_hasher.finalize().into_bytes().into();

    Ok(SignedCommit {
        ver: 1,
        hash: *hash,
        ikm,
        sig: sig.to_bytes().to_vec(),
        mac,
        rev: rev.to_string(),
    })
}

pub fn verify_commit(
    commit: &SignedCommit,
    space_uri: &str,
    author_did: &str,
    verifying_key: &VerifyingKey,
) -> Result<(), AppError> {
    if commit.ver != 1 {
        return Err(AppError::BadRequest(format!(
            "unsupported commit version: {}",
            commit.ver
        )));
    }

    let ctx = build_context(space_uri, author_did, &commit.rev, &commit.ikm);

    let sig = Signature::from_bytes(commit.sig.as_slice().into())
        .map_err(|_| AppError::Auth("invalid commit signature format".into()))?;
    verifying_key
        .verify(&ctx, &sig)
        .map_err(|_| AppError::Auth("commit signature verification failed".into()))?;

    // Recompute and verify MAC
    let hk = Hkdf::<Sha256>::new(None, &commit.ikm);
    let mut derived_key = [0u8; 32];
    hk.expand(&ctx, &mut derived_key)
        .map_err(|e| AppError::Internal(format!("HKDF expand failed: {e}")))?;

    let mut mac_hasher = <Hmac<Sha256> as Mac>::new_from_slice(&derived_key)
        .map_err(|e| AppError::Internal(format!("HMAC init failed: {e}")))?;
    mac_hasher.update(&commit.hash);

    mac_hasher.verify_slice(&commit.mac).map_err(|_| {
        AppError::Auth("commit MAC verification failed — repo hash mismatch".into())
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::SigningKey;

    fn test_signing_key() -> SigningKey {
        let mut bytes = [0u8; 32];
        bytes[31] = 1; // valid non-zero scalar
        SigningKey::from_bytes((&bytes[..]).into()).unwrap()
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

    #[test]
    fn sign_and_verify_roundtrip() {
        let sk = test_signing_key();
        let vk = *sk.verifying_key();
        let hash = [0xCC; 32];
        let space = "at://did:plc:abc/space/com.example.forum/main";

        let commit = sign_commit(&hash, space, "did:plc:testuser", "3k2rev1", &sk).unwrap();

        assert_eq!(commit.hash, hash);
        assert_eq!(commit.rev, "3k2rev1");
        assert_eq!(commit.mac.len(), 32);
        assert!(!commit.sig.is_empty());
        assert_eq!(commit.ver, 1);

        assert!(verify_commit(&commit, space, "did:plc:testuser", &vk).is_ok());
    }

    #[test]
    fn commit_has_version() {
        let sk = test_signing_key();
        let hash = [0xCC; 32];
        let space = "at://did:plc:abc/space/com.example.forum/main";
        let commit = sign_commit(&hash, space, "did:plc:testuser", "rev1", &sk).unwrap();
        assert_eq!(commit.ver, 1);
    }

    #[test]
    fn verify_rejects_wrong_author() {
        let sk = test_signing_key();
        let vk = *sk.verifying_key();
        let hash = [0xAA; 32];
        let space = "at://did:plc:abc/space/com.example.forum/main";

        let commit = sign_commit(&hash, space, "did:plc:user1", "rev1", &sk).unwrap();
        assert!(verify_commit(&commit, space, "did:plc:user1", &vk).is_ok());
        assert!(verify_commit(&commit, space, "did:plc:user2", &vk).is_err());
    }

    #[test]
    fn verify_rejects_wrong_key() {
        let sk1 = test_signing_key();
        let mut bytes2 = [0u8; 32];
        bytes2[31] = 2;
        let sk2 = SigningKey::from_bytes((&bytes2[..]).into()).unwrap();
        let vk2 = *sk2.verifying_key();

        let hash = [0xDD; 32];
        let space = "at://did:plc:abc/space/com.example.forum/main";

        let commit = sign_commit(&hash, space, "did:plc:testuser", "rev1", &sk1).unwrap();
        assert!(verify_commit(&commit, space, "did:plc:testuser", &vk2).is_err());
    }

    #[test]
    fn verify_rejects_tampered_hash() {
        let sk = test_signing_key();
        let vk = *sk.verifying_key();
        let hash = [0xEE; 32];
        let space = "at://did:plc:abc/space/com.example.forum/main";

        let mut commit = sign_commit(&hash, space, "did:plc:testuser", "rev1", &sk).unwrap();
        commit.hash[0] ^= 0xFF; // tamper
        assert!(verify_commit(&commit, space, "did:plc:testuser", &vk).is_err());
    }

    #[test]
    fn verify_rejects_wrong_space() {
        let sk = test_signing_key();
        let vk = *sk.verifying_key();
        let hash = [0xFF; 32];

        let commit = sign_commit(
            &hash,
            "at://did:plc:abc/space/com.example.forum/main",
            "did:plc:user",
            "rev1",
            &sk,
        )
        .unwrap();
        assert!(
            verify_commit(
                &commit,
                "at://did:plc:xyz/space/com.example.forum/other",
                "did:plc:user",
                &vk
            )
            .is_err()
        );
    }

    #[test]
    fn different_ikm_per_commit() {
        let sk = test_signing_key();
        let hash = [0xAA; 32];
        let space = "at://did:plc:abc/space/com.example.forum/main";

        let c1 = sign_commit(&hash, space, "did:plc:testuser", "rev1", &sk).unwrap();
        let c2 = sign_commit(&hash, space, "did:plc:testuser", "rev1", &sk).unwrap();

        // Each call generates fresh ikm
        assert_ne!(c1.ikm, c2.ikm);
        // But both verify
        let vk = *sk.verifying_key();
        assert!(verify_commit(&c1, space, "did:plc:testuser", &vk).is_ok());
        assert!(verify_commit(&c2, space, "did:plc:testuser", &vk).is_ok());
    }

    #[test]
    fn verify_rejects_unknown_version() {
        let sk = test_signing_key();
        let vk = *sk.verifying_key();
        let hash = [0xCC; 32];
        let space = "at://did:plc:abc/space/com.example.forum/main";

        let mut commit = sign_commit(&hash, space, "did:plc:testuser", "rev1", &sk).unwrap();
        commit.ver = 2;
        assert!(verify_commit(&commit, space, "did:plc:testuser", &vk).is_err());
    }

    #[test]
    fn verify_rejects_tampered_mac() {
        let sk = test_signing_key();
        let vk = *sk.verifying_key();
        let hash = [0xCC; 32];
        let space = "at://did:plc:abc/space/com.example.forum/main";

        let mut commit = sign_commit(&hash, space, "did:plc:testuser", "rev1", &sk).unwrap();
        assert!(verify_commit(&commit, space, "did:plc:testuser", &vk).is_ok());
        commit.mac[0] ^= 0xFF;
        assert!(verify_commit(&commit, space, "did:plc:testuser", &vk).is_err());
    }
}
