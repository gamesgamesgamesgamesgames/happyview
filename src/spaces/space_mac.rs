//! Proposal 0016's commit authentication, from the syncer's side.
//!
//! A 0016 commit carries no signature. The syncer encapsulates against the
//! author's `#atproto_kem` key, sends the ephemeral half in
//! `Atproto-Space-Want-Mac`, and the repo host answers with an HMAC over the
//! commit in `Atproto-Space-Mac`. A leaked response proves nothing to a third
//! party, since the syncer could have computed the MAC itself.
//!
//! Everything here must match `@atproto/space`'s `kem.ts` and `mac.ts` byte for
//! byte; `tests/fixtures/space_mac_vectors.json` pins it to the reference.

use hkdf::Hkdf;
use hmac::{Hmac, KeyInit, Mac};
use p256::elliptic_curve::sec1::ToSec1Point;
use p256::{PublicKey, SecretKey};
use sha2::Sha256;

use crate::error::AppError;

pub const COMMIT_MAC_TYPE: &str = "atproto-space-repo";
/// Commits share their MAC version with the commit format.
pub const COMMIT_MAC_VERSION: i64 = 1;

pub const WANT_MAC_HEADER: &str = "atproto-space-want-mac";
pub const MAC_HEADER: &str = "atproto-space-mac";

/// A 0016 commit: the repo's set hash at a revision, with nothing that
/// authenticates it. The MAC arrives beside it, in the response headers.
#[derive(Debug, Clone)]
pub struct Commit {
    pub ver: u32,
    pub repo: String,
    pub space: String,
    pub rev: String,
    /// sha256 of the LtHash state, which the old commit called `hash`.
    pub data: [u8; 32],
}

/// The syncer's half of a key exchange: what to send, and the secret the host
/// will MAC with.
pub struct Encapsulation {
    /// The ephemeral public key as a `did:key`.
    pub key_share: String,
    pub shared_secret: [u8; 32],
}

/// Derive a shared secret against an author's KEM key with a fresh ephemeral
/// key. Use one per request.
pub fn encapsulate(author_kem: &PublicKey) -> Encapsulation {
    use rand::Rng;
    // Rejection-sample a scalar; a 32-byte string outside [1, n) is
    // astronomically rare.
    let ephemeral = loop {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        if let Ok(key) = SecretKey::from_slice(&bytes) {
            break key;
        }
    };
    encapsulate_with(&ephemeral, author_kem)
}

fn encapsulate_with(ephemeral: &SecretKey, author_kem: &PublicKey) -> Encapsulation {
    let shared = p256::ecdh::diffie_hellman(ephemeral.to_nonzero_scalar(), author_kem.as_affine());
    let ephemeral_public = ephemeral.public_key();
    Encapsulation {
        key_share: did_key(&ephemeral_public),
        shared_secret: derive_shared_secret(
            shared.raw_secret_bytes().as_slice(),
            &ephemeral_public,
            author_kem,
        ),
    }
}

/// DHKEM(P-256, HKDF-SHA256) from RFC 9180 §4.1, with both public keys in
/// uncompressed form as the reference takes them from `parseDidKey`.
fn derive_shared_secret(dh: &[u8], syncer: &PublicKey, author: &PublicKey) -> [u8; 32] {
    // "KEM" || I2OSP(0x0010, 2) identifies DHKEM(P-256, HKDF-SHA256).
    let prefix: &[u8] = b"HPKE-v1KEM\x00\x10";

    let mut input = prefix.to_vec();
    input.extend_from_slice(b"eae_prk");
    input.extend_from_slice(dh);

    let mut info = 32u16.to_be_bytes().to_vec();
    info.extend_from_slice(prefix);
    info.extend_from_slice(b"shared_secret");
    info.extend_from_slice(syncer.to_sec1_point(false).as_bytes());
    info.extend_from_slice(author.to_sec1_point(false).as_bytes());

    // Extract and expand, with no salt: unlike the old commit MAC, the input
    // here is not already a uniformly random key.
    let mut out = [0u8; 32];
    Hkdf::<Sha256>::new(None, &input)
        .expand(&info, &mut out)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    out
}

/// ```text
/// context = "atproto-space-repo-v1"
///    || uint16be(len(space)) || space
///    || uint16be(len(repo))  || repo
///    || uint16be(len(rev))   || rev
/// mac = HMAC-SHA256(shared_secret, context || data)
/// ```
pub fn commit_mac(shared_secret: &[u8; 32], commit: &Commit) -> Result<[u8; 32], AppError> {
    let mut message = format!("{COMMIT_MAC_TYPE}-v{}", commit.ver).into_bytes();
    for field in [&commit.space, &commit.repo, &commit.rev] {
        let len = u16::try_from(field.len())
            .map_err(|_| AppError::BadRequest("commit field is too long to authenticate".into()))?;
        message.extend_from_slice(&len.to_be_bytes());
        message.extend_from_slice(field.as_bytes());
    }
    message.extend_from_slice(&commit.data);

    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(shared_secret)
        .expect("HMAC takes a key of any length");
    mac.update(&message);
    Ok(mac.finalize().into_bytes().into())
}

/// Check a 0016 commit against the MAC its response carried, and that it
/// describes the repo we asked for.
///
/// Once this passes, `data` is the host's authenticated claim about the repo.
/// It still has to be compared against the records to mean they agree.
pub fn verify_commit(
    commit: &Commit,
    space_uri: &str,
    author_did: &str,
    shared_secret: &[u8; 32],
    mac: &[u8; 32],
) -> Result<(), AppError> {
    if commit.ver != 1 {
        return Err(AppError::BadRequest(format!(
            "unsupported commit version: {}",
            commit.ver
        )));
    }
    if commit.repo != author_did || commit.space != space_uri {
        return Err(AppError::Auth(
            "commit describes a different repo than the one requested".into(),
        ));
    }
    let expected = commit_mac(shared_secret, commit)?;
    if !crate::constant_time::ct_eq(&expected, mac) {
        return Err(AppError::Auth("commit MAC verification failed".into()));
    }
    Ok(())
}

/// The `Atproto-Space-Want-Mac` request header for a commit.
pub fn want_mac_header(key_share: &str) -> Result<String, AppError> {
    let key_share = sfv::StringRef::from_str(key_share)
        .map_err(|_| AppError::Internal("key share is not a structured string".into()))?;
    let mut dict = sfv::DictSerializer::new();
    _ = dict.bare_item(sfv::key_ref("type"), sfv::string_ref(COMMIT_MAC_TYPE));
    _ = dict.bare_item(sfv::key_ref("ver"), sfv::integer(COMMIT_MAC_VERSION));
    _ = dict.bare_item(sfv::key_ref("key_share"), key_share);
    dict.finish()
        .ok_or_else(|| AppError::Internal("empty MAC request header".into()))
}

/// The MAC from an `Atproto-Space-Mac` response header for a commit.
pub fn parse_mac_header(value: &str) -> Result<[u8; 32], AppError> {
    let malformed = |why: &str| AppError::Auth(format!("{MAC_HEADER} header {why}"));
    let dict: sfv::Dictionary = sfv::Parser::new(value)
        .parse()
        .map_err(|_| malformed("is malformed"))?;
    let item = |name: &str| match dict.get(name) {
        Some(sfv::ListEntry::Item(item)) => Some(&item.bare_item),
        _ => None,
    };

    if item("type").and_then(|v| v.as_string()).map(|s| s.as_str()) != Some(COMMIT_MAC_TYPE) {
        return Err(malformed(&format!("does not name type {COMMIT_MAC_TYPE}")));
    }
    if item("ver").and_then(|v| v.as_integer()) != Some(sfv::integer(COMMIT_MAC_VERSION)) {
        return Err(malformed("names an unsupported version"));
    }
    item("mac")
        .and_then(|v| v.as_byte_sequence())
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
        .ok_or_else(|| malformed("carries no 32-byte mac"))
}

/// The author's `#atproto_kem` key, or `None` when their DID document
/// publishes none, as for every account created before 0016.
pub fn author_kem_key(
    did_doc: &crate::profile::DidDocument,
) -> Result<Option<PublicKey>, AppError> {
    let Some(vm) = did_doc
        .verification_method
        .iter()
        .find(|v| v.id.ends_with("#atproto_kem"))
    else {
        return Ok(None);
    };
    let multibase = vm.public_key_multibase.as_deref().ok_or_else(|| {
        AppError::Auth("#atproto_kem verification method missing publicKeyMultibase".into())
    })?;
    let (_, bytes) = multibase::decode(multibase)
        .map_err(|e| AppError::Auth(format!("invalid #atproto_kem multibase: {e}")))?;
    match bytes.as_slice() {
        [0x80, 0x24, point @ ..] => PublicKey::from_sec1_bytes(point)
            .map(Some)
            .map_err(|_| AppError::Auth("invalid #atproto_kem P-256 key".into())),
        _ => Err(AppError::Auth("#atproto_kem must be a P-256 key".into())),
    }
}

fn did_key(key: &PublicKey) -> String {
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

    #[derive(serde::Deserialize)]
    struct VectorFile {
        vectors: Vec<Vector>,
    }

    #[derive(serde::Deserialize)]
    struct Vector {
        author_kem_private_hex: String,
        author_kem_did: String,
        ephemeral_private_hex: String,
        ephemeral_did: String,
        shared_secret_hex: String,
        commit: VectorCommit,
        commit_mac_hex: String,
        want_mac_header: String,
        mac_header: String,
    }

    #[derive(serde::Deserialize)]
    struct VectorCommit {
        ver: u32,
        repo: String,
        space: String,
        rev: String,
        data_hex: String,
    }

    fn vectors() -> Vec<Vector> {
        let raw = include_str!("../../tests/fixtures/space_mac_vectors.json");
        let file: VectorFile = serde_json::from_str(raw).expect("vector fixture parses");
        assert!(!file.vectors.is_empty(), "fixture must carry vectors");
        file.vectors
    }

    fn from_hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
            .collect()
    }

    fn secret(hex: &str) -> SecretKey {
        SecretKey::from_slice(&from_hex(hex)).unwrap()
    }

    fn commit(v: &Vector) -> Commit {
        Commit {
            ver: v.commit.ver,
            repo: v.commit.repo.clone(),
            space: v.commit.space.clone(),
            rev: v.commit.rev.clone(),
            data: from_hex(&v.commit.data_hex).try_into().unwrap(),
        }
    }

    #[test]
    fn the_shared_secret_matches_the_reference() {
        for v in vectors() {
            let author = secret(&v.author_kem_private_hex).public_key();
            assert_eq!(did_key(&author), v.author_kem_did);

            let enc = encapsulate_with(&secret(&v.ephemeral_private_hex), &author);
            assert_eq!(enc.key_share, v.ephemeral_did);
            assert_eq!(enc.shared_secret.to_vec(), from_hex(&v.shared_secret_hex));
        }
    }

    #[test]
    fn the_commit_mac_matches_the_reference() {
        for v in vectors() {
            let ss: [u8; 32] = from_hex(&v.shared_secret_hex).try_into().unwrap();
            let mac = commit_mac(&ss, &commit(&v)).unwrap();
            assert_eq!(mac.to_vec(), from_hex(&v.commit_mac_hex));
        }
    }

    #[test]
    fn the_headers_match_the_reference() {
        for v in vectors() {
            assert_eq!(
                want_mac_header(&v.ephemeral_did).unwrap(),
                v.want_mac_header
            );
            assert_eq!(
                parse_mac_header(&v.mac_header).unwrap().to_vec(),
                from_hex(&v.commit_mac_hex)
            );
        }
    }

    #[test]
    fn a_reference_commit_verifies_end_to_end() {
        for v in vectors() {
            let author = secret(&v.author_kem_private_hex).public_key();
            let enc = encapsulate_with(&secret(&v.ephemeral_private_hex), &author);
            let mac = parse_mac_header(&v.mac_header).unwrap();
            verify_commit(
                &commit(&v),
                &v.commit.space,
                &v.commit.repo,
                &enc.shared_secret,
                &mac,
            )
            .expect("a reference commit must verify");
        }
    }

    #[test]
    fn a_commit_for_another_repo_or_space_does_not_verify() {
        let v = &vectors()[0];
        let ss: [u8; 32] = from_hex(&v.shared_secret_hex).try_into().unwrap();
        let mac = parse_mac_header(&v.mac_header).unwrap();
        let c = commit(v);
        assert!(verify_commit(&c, &c.space, "did:plc:someoneelse", &ss, &mac).is_err());
        assert!(verify_commit(&c, "ats://did:plc:x/a.b.c/d", &c.repo, &ss, &mac).is_err());

        let mut tampered = c.clone();
        tampered.data[0] ^= 1;
        assert!(verify_commit(&tampered, &c.space, &c.repo, &ss, &mac).is_err());
    }

    #[test]
    fn a_mac_header_for_a_record_or_another_version_is_refused() {
        let mac = ":k1aEm3P01mYRrvVsSAipAafQPjxUrvIgeTC1LnBDb28=:";
        assert!(
            parse_mac_header(&format!("type=\"atproto-space-record\", ver=1, mac={mac}")).is_err()
        );
        assert!(
            parse_mac_header(&format!("type=\"atproto-space-repo\", ver=2, mac={mac}")).is_err()
        );
        assert!(parse_mac_header("type=\"atproto-space-repo\", ver=1").is_err());
    }

    #[test]
    fn fresh_encapsulations_differ_but_agree_with_the_author() {
        let author_secret = secret(&"5c".repeat(32));
        let author = author_secret.public_key();
        let a = encapsulate(&author);
        let b = encapsulate(&author);
        assert_ne!(a.key_share, b.key_share, "each request needs its own key");

        // The author's side, as a repo host computes it.
        let syncer = PublicKey::from_sec1_bytes(
            &multibase::decode(a.key_share.strip_prefix("did:key:").unwrap())
                .unwrap()
                .1[2..],
        )
        .unwrap();
        let dh = p256::ecdh::diffie_hellman(author_secret.to_nonzero_scalar(), syncer.as_affine());
        assert_eq!(
            derive_shared_secret(dh.raw_secret_bytes().as_slice(), &syncer, &author),
            a.shared_secret
        );
    }
}
