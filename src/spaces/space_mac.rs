//! Proposal 0016's commit authentication, from the syncer's side.
//!
//! A 0016 commit carries no signature. The syncer encapsulates against the
//! author's `#atproto_kem` key, sends the ephemeral half, and the repo host
//! answers with an HMAC over the commit. A leaked response proves nothing to a
//! third party, since the syncer could have computed the MAC itself.
//!
//! Two revisions of the exchange are in use, and a syncer asks in both:
//!
//! - The Nov 12 text: an RFC 9421 `Accept-Signature` request tagged
//!   `atproto-space-commit`, answered with an `Atproto-Space-Commit` header
//!   and an `hmac-sha256` signature over it, keyed by HPKE secret export.
//! - The Oct 29 text: `Atproto-Space-Want-Mac`, answered with
//!   `Atproto-Space-Mac`, keyed by the DHKEM secret alone.
//!
//! Both are keyed by the same ephemeral key share, so asking in both gives a
//! host nothing it couldn't get from either.
//!
//! Everything here must match `@atproto/space`'s `kem.ts` and `mac.ts` byte for
//! byte; `tests/fixtures/space_mac_vectors.json` pins the Oct 29 exchange to
//! its reference, and the tests below pin the Nov 12 one.

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
    /// The DHKEM secret, which the Oct 29 exchange MACs with.
    pub shared_secret: [u8; 32],
    /// The HPKE exported secret, which the Nov 12 exchange MACs with.
    pub export_secret: [u8; 32],
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
    let shared_secret = derive_shared_secret(
        shared.raw_secret_bytes().as_slice(),
        &ephemeral_public,
        author_kem,
    );
    Encapsulation {
        key_share: did_key(&ephemeral_public),
        shared_secret,
        export_secret: export_secret(&shared_secret),
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
    check_commit_names(commit, space_uri, author_did)?;
    let expected = commit_mac(shared_secret, commit)?;
    if !crate::constant_time::ct_eq(&expected, mac) {
        return Err(AppError::Auth("commit MAC verification failed".into()));
    }
    Ok(())
}

/// Check an authenticated commit describes the repo we asked for: a version
/// we read, the author's repo, in the space asked for.
fn check_commit_names(commit: &Commit, space_uri: &str, author_did: &str) -> Result<(), AppError> {
    if commit.ver != 1 {
        return Err(AppError::BadRequest(format!(
            "unsupported commit version: {}",
            commit.ver
        )));
    }
    // The host writes the space in its form, which may not be the one asked.
    if commit.repo != author_did || !crate::spaces::same_space_uri(&commit.space, space_uri) {
        return Err(AppError::Auth(
            "commit describes a different repo than the one requested".into(),
        ));
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

// ---- The Nov 12 exchange ----

/// The profile a commit MAC is requested and answered under, and the header
/// it covers.
pub const COMMIT_MAC_PROFILE: &str = "atproto-space-commit";
pub const ACCEPT_SIGNATURE_HEADER: &str = "accept-signature";
pub const SIGNATURE_INPUT_HEADER: &str = "signature-input";
pub const SIGNATURE_HEADER: &str = "signature";
const MAC_ALG: &str = "hmac-sha256";
/// The label this syncer requests its MAC under. A host may answer under
/// another; the MAC is found by its tag.
const MAC_LABEL: &str = "mac";

/// HPKE (RFC 9180) base-mode key schedule for DHKEM(P-256, HKDF-SHA256),
/// HKDF-SHA256 and the export-only AEAD, then a 32-byte export with an empty
/// exporter context, under `info` "atproto-mac".
fn export_secret(kem_secret: &[u8; 32]) -> [u8; 32] {
    // "HPKE" || KEM 0x0010 || KDF 0x0001 || AEAD 0xffff
    let suite: &[u8] = b"HPKE\x00\x10\x00\x01\xff\xff";
    let labeled = |label: &str, ikm: &[u8]| {
        let mut v = b"HPKE-v1".to_vec();
        v.extend_from_slice(suite);
        v.extend_from_slice(label.as_bytes());
        v.extend_from_slice(ikm);
        v
    };
    let extract = |salt: &[u8], label: &str, ikm: &[u8]| {
        let (prk, _) = Hkdf::<Sha256>::extract(Some(salt), &labeled(label, ikm));
        prk
    };
    let expand = |prk: &[u8], label: &str, info: &[u8]| {
        let mut full = 32u16.to_be_bytes().to_vec();
        full.extend_from_slice(&labeled(label, info));
        let mut out = [0u8; 32];
        Hkdf::<Sha256>::from_prk(prk)
            .expect("an extracted PRK is long enough")
            .expand(&full, &mut out)
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        out
    };

    let psk_id_hash = extract(&[], "psk_id_hash", &[]);
    let info_hash = extract(&[], "info_hash", b"atproto-mac");
    let mut context = vec![0x00]; // mode_base
    context.extend_from_slice(&psk_id_hash);
    context.extend_from_slice(&info_hash);
    let secret = extract(kem_secret, "secret", &[]);
    let exporter_secret = expand(&secret, "exp", &context);
    expand(&exporter_secret, "sec", &[])
}

/// The covered components of a commit MAC: the `Atproto-Space-Commit` header,
/// flagged `sf`.
fn commit_mac_components() -> sfv::InnerList {
    let mut sf = sfv::Parameters::new();
    sf.insert(sfv::key_ref("sf").to_owned(), sfv::BareItem::Boolean(true));
    sfv::InnerList::new(vec![sfv::Item::with_params(
        sfv::string_ref(COMMIT_MAC_PROFILE).to_owned(),
        sf,
    )])
}

fn serialize_inner_list(list: &sfv::InnerList) -> Option<String> {
    use sfv::FieldType;
    vec![sfv::ListEntry::InnerList(list.clone())].serialize()
}

/// The `Accept-Signature` request header asking for a commit MAC keyed by
/// `key_share`, e.g.
/// `mac=("atproto-space-commit";sf);keyid="did:key:zDna...";tag="atproto-space-commit"`.
pub fn accept_signature_header(key_share: &str) -> Result<String, AppError> {
    let key_share = sfv::StringRef::from_str(key_share)
        .map_err(|_| AppError::Internal("key share is not a structured string".into()))?;
    let mut request = commit_mac_components();
    request.params.insert(
        sfv::key_ref("keyid").to_owned(),
        key_share.to_owned().into(),
    );
    request.params.insert(
        sfv::key_ref("tag").to_owned(),
        sfv::string_ref(COMMIT_MAC_PROFILE).to_owned().into(),
    );
    let params = serialize_inner_list(&request)
        .ok_or_else(|| AppError::Internal("empty MAC request".into()))?;
    Ok(format!("{MAC_LABEL}={params}"))
}

/// The response headers a Nov 12 host answers a commit MAC request with.
pub struct CommitMacHeaders<'a> {
    pub commit: &'a str,
    pub signature_input: &'a str,
    pub signature: &'a str,
}

/// The signature base (RFC 9421 §2.5) for a commit MAC over `covered`, the
/// `Atproto-Space-Commit` header as re-serialized from its parsed form.
fn commit_signature_base(covered: &str, signature_params: &str) -> String {
    format!(
        "\"{COMMIT_MAC_PROFILE}\";sf: {}\n\"@signature-params\": {signature_params}",
        covered.trim()
    )
}

/// Check a Nov 12 commit MAC: find the one signature tagged
/// `atproto-space-commit`, check it covers exactly the `Atproto-Space-Commit`
/// header under `hmac-sha256` and our key share, verify the HMAC, and check the
/// header describes `commit`, which describes the repo we asked for.
pub fn verify_commit_signature(
    commit: &Commit,
    space_uri: &str,
    author_did: &str,
    exchange: &Encapsulation,
    headers: &CommitMacHeaders<'_>,
) -> Result<(), AppError> {
    let refused = |why: &str| AppError::Auth(format!("commit MAC {why}"));
    let parse_dict = |value: &str, name: &str| -> Result<sfv::Dictionary, AppError> {
        sfv::Parser::new(value)
            .parse()
            .map_err(|_| refused(&format!("has a malformed {name} header")))
    };
    let inputs = parse_dict(headers.signature_input, SIGNATURE_INPUT_HEADER)?;
    let signatures = parse_dict(headers.signature, SIGNATURE_HEADER)?;

    let tagged: Vec<(&sfv::Key, &sfv::InnerList)> = inputs
        .iter()
        .filter_map(|(label, entry)| match entry {
            sfv::ListEntry::InnerList(list)
                if list
                    .params
                    .get("tag")
                    .and_then(|t| t.as_string())
                    .map(|s| s.as_str())
                    == Some(COMMIT_MAC_PROFILE) =>
            {
                Some((label, list))
            }
            _ => None,
        })
        .collect();
    let (label, input) = match tagged.as_slice() {
        [one] => *one,
        [] => {
            return Err(refused(&format!(
                "tagged \"{COMMIT_MAC_PROFILE}\" is missing"
            )));
        }
        _ => {
            return Err(refused(&format!(
                "tagged \"{COMMIT_MAC_PROFILE}\" appears more than once"
            )));
        }
    };
    let mac = match signatures.get(label.as_str()) {
        Some(sfv::ListEntry::Item(item)) if item.params.is_empty() => item
            .bare_item
            .as_byte_sequence()
            .ok_or_else(|| refused("is not a byte sequence"))?,
        _ => return Err(refused("is missing its signature")),
    };

    if input.items != commit_mac_components().items {
        return Err(refused(&format!(
            "must cover exactly (\"{COMMIT_MAC_PROFILE}\";sf)"
        )));
    }
    if let Some(alg) = input.params.get("alg")
        && alg.as_string().map(|s| s.as_str()) != Some(MAC_ALG)
    {
        return Err(refused(&format!("algorithm must be {MAC_ALG}")));
    }
    if let Some(keyid) = input.params.get("keyid")
        && keyid.as_string().map(|s| s.as_str()) != Some(exchange.key_share.as_str())
    {
        return Err(refused("names another key share"));
    }

    let fields = parse_dict(headers.commit, COMMIT_MAC_PROFILE)?;
    use sfv::FieldType;
    let covered = fields
        .serialize()
        .ok_or_else(|| refused(&format!("has an empty {COMMIT_MAC_PROFILE} header")))?;
    let signature_params =
        serialize_inner_list(input).ok_or_else(|| refused("has empty parameters"))?;
    let base = commit_signature_base(&covered, &signature_params);
    let mut hmac = <Hmac<Sha256> as KeyInit>::new_from_slice(&exchange.export_secret)
        .expect("HMAC takes a key of any length");
    hmac.update(base.as_bytes());
    let expected: [u8; 32] = hmac.finalize().into_bytes().into();
    if !crate::constant_time::ct_eq(&expected, mac) {
        return Err(AppError::Auth("commit MAC verification failed".into()));
    }

    let item = |name: &str| match fields.get(name) {
        Some(sfv::ListEntry::Item(item)) => Some(&item.bare_item),
        _ => None,
    };
    let text = |name: &str| item(name).and_then(|v| v.as_string()).map(|s| s.as_str());
    let matches = item("ver").and_then(|v| v.as_integer()) == Some(sfv::integer(commit.ver.into()))
        && text("repo") == Some(commit.repo.as_str())
        && text("space") == Some(commit.space.as_str())
        && text("rev") == Some(commit.rev.as_str())
        && item("data").and_then(|v| v.as_byte_sequence()) == Some(commit.data.as_slice());
    if !matches {
        return Err(AppError::Auth("commit does not match its MAC".into()));
    }
    check_commit_names(commit, space_uri, author_did)
}

/// The headers asking for a commit MAC keyed by `exchange`, in both
/// revisions: a host answers the one it reads and ignores the other.
pub fn commit_mac_request_headers(
    exchange: &Encapsulation,
) -> Result<Vec<(axum::http::HeaderName, axum::http::HeaderValue)>, AppError> {
    let value = |v: String| {
        axum::http::HeaderValue::from_str(&v)
            .map_err(|_| AppError::Internal("unsendable MAC request".into()))
    };
    Ok(vec![
        (
            axum::http::HeaderName::from_static(ACCEPT_SIGNATURE_HEADER),
            value(accept_signature_header(&exchange.key_share)?)?,
        ),
        (
            axum::http::HeaderName::from_static(WANT_MAC_HEADER),
            value(want_mac_header(&exchange.key_share)?)?,
        ),
    ])
}

/// Authenticate a 0016 commit by the MAC its response carried: the Nov 12
/// signature when the host sent one, otherwise the Oct 29 header.
pub fn verify_commit_response(
    commit: &Commit,
    space_uri: &str,
    author_did: &str,
    exchange: &Encapsulation,
    headers: &axum::http::HeaderMap,
) -> Result<(), AppError> {
    let text = |name: &str| -> Result<Option<String>, AppError> {
        let values = headers
            .get_all(name)
            .iter()
            .map(|v| {
                v.to_str()
                    .map_err(|_| AppError::Auth(format!("{name} header is not text")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((!values.is_empty()).then(|| values.join(", ")))
    };
    if let (Some(commit_header), Some(signature_input), Some(signature)) = (
        text(COMMIT_MAC_PROFILE)?,
        text(SIGNATURE_INPUT_HEADER)?,
        text(SIGNATURE_HEADER)?,
    ) {
        return verify_commit_signature(
            commit,
            space_uri,
            author_did,
            exchange,
            &CommitMacHeaders {
                commit: &commit_header,
                signature_input: &signature_input,
                signature: &signature,
            },
        );
    }
    let mac_header = text(MAC_HEADER)?.ok_or_else(|| {
        AppError::Auth(format!(
            "the repo host sent a commit without the MAC it was asked for \
             ({COMMIT_MAC_PROFILE} signature or {MAC_HEADER} header)"
        ))
    })?;
    let mac = parse_mac_header(&mac_header)?;
    verify_commit(commit, space_uri, author_did, &exchange.shared_secret, &mac)
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
    fn a_commit_verifies_for_its_space_named_in_either_form() {
        let v = &vectors()[0];
        let ss: [u8; 32] = from_hex(&v.shared_secret_hex).try_into().unwrap();
        let mac = parse_mac_header(&v.mac_header).unwrap();
        let c = commit(v);
        let other_form = if c.space.starts_with("at:space/") {
            crate::spaces::UriForm::Legacy.write(&c.space)
        } else {
            crate::spaces::UriForm::AtSpace.write(&c.space)
        };
        assert_ne!(other_form, c.space);
        verify_commit(&c, &other_form, &c.repo, &ss, &mac)
            .expect("the host may name the space in the other form");
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

    // ---- The Nov 12 exchange, pinned to `kem.test.ts` and `mac.test.ts` ----

    /// The export-only DHKEM(P-256, HKDF-SHA256) entry of the HPKE test
    /// vectors, with the secret exported under `info` "atproto-mac" as
    /// `kem.test.ts` expects it.
    #[test]
    fn the_export_secret_matches_hpke() {
        let enc = PublicKey::from_sec1_bytes(&from_hex(
            "04d804370b7e24b94749eb1dc8df6d4d4a5d75f9effad01739ebcad5c54a40d57aaa8b4190fc124dbde2e4f1e1d1b012a3bc4038157dc29b55533a932306d8d38d",
        ))
        .unwrap();
        let author = secret("62c3868357a464f8461d03aa0182c7cebcde841036aea7230ddc7339f1088346");
        let dh = p256::ecdh::diffie_hellman(author.to_nonzero_scalar(), enc.as_affine());
        let kem =
            derive_shared_secret(dh.raw_secret_bytes().as_slice(), &enc, &author.public_key());
        assert_eq!(
            export_secret(&kem).to_vec(),
            from_hex("2060e0558d1b34c10086bc137f849bb79ba59fcdc30544418e35c9cd301f0d0d")
        );
    }

    #[test]
    fn an_encapsulation_carries_both_secrets() {
        let v = &vectors()[0];
        let author = secret(&v.author_kem_private_hex).public_key();
        let enc = encapsulate_with(&secret(&v.ephemeral_private_hex), &author);
        assert_eq!(enc.export_secret, export_secret(&enc.shared_secret));
        assert_ne!(enc.export_secret, enc.shared_secret);
    }

    const KEY_SHARE: &str = "did:key:zDnaExample";
    const G2_SPACE: &str = "at:space/did:example:space/app.bsky.group/test";
    const G2_PARAMS: &str =
        r#"("atproto-space-commit";sf);keyid="did:key:zDnaExample";tag="atproto-space-commit""#;

    fn g2_exchange() -> Encapsulation {
        Encapsulation {
            key_share: KEY_SHARE.into(),
            shared_secret: [7; 32],
            export_secret: [9; 32],
        }
    }

    fn g2_commit() -> Commit {
        Commit {
            ver: 1,
            repo: "did:example:alice".into(),
            space: G2_SPACE.into(),
            rev: "3kbcq3p7ad400".into(),
            data: [1; 32],
        }
    }

    fn g2_covered() -> String {
        format!(
            r#"ver=1, repo="did:example:alice", space="{G2_SPACE}", rev="3kbcq3p7ad400", data=:AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=:"#
        )
    }

    /// A host's answer, as `createCommitMacHeaders` writes it: the covered
    /// header, and an HMAC under `secret` over the base `mac.test.ts` spells
    /// out, under `label` and `params`.
    fn g2_answer(covered: &str, label: &str, params: &str, secret: &[u8; 32]) -> (String, String) {
        use base64::Engine;
        let base =
            format!("\"atproto-space-commit\";sf: {covered}\n\"@signature-params\": {params}");
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret).unwrap();
        mac.update(base.as_bytes());
        let mac = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
        (format!("{label}={params}"), format!("{label}=:{mac}:"))
    }

    fn g2_verify(commit: &Commit, covered: &str, input: &str, sig: &str) -> Result<(), AppError> {
        verify_commit_signature(
            commit,
            &commit.space,
            &commit.repo,
            &g2_exchange(),
            &CommitMacHeaders {
                commit: covered,
                signature_input: input,
                signature: sig,
            },
        )
    }

    #[test]
    fn the_accept_signature_header_matches_the_reference() {
        assert_eq!(
            accept_signature_header(KEY_SHARE).unwrap(),
            format!("mac={G2_PARAMS}")
        );
    }

    #[test]
    fn a_reference_commit_mac_verifies() {
        let (input, sig) = g2_answer(&g2_covered(), "sig1", G2_PARAMS, &[9; 32]);
        g2_verify(&g2_commit(), &g2_covered(), &input, &sig).expect("verifies");
        // Under another label, beside another signature, with whitespace the
        // header's re-serialization drops.
        let (input, sig) = g2_answer(&g2_covered(), "m", G2_PARAMS, &[9; 32]);
        let input = format!(r#"other=("content-digest");tag="other", {input}"#);
        let sig = format!("other=:AAAA:, {sig}");
        let spaced = g2_covered().replace(", ", ",   ");
        g2_verify(&g2_commit(), &spaced, &input, &sig).expect("verifies");
    }

    #[test]
    fn a_commit_mac_with_other_parameters_verifies() {
        for params in [
            r#"("atproto-space-commit";sf);tag="atproto-space-commit";created=1700000000;keyid="did:key:zDnaExample""#.to_string(),
            r#"("atproto-space-commit";sf);tag="atproto-space-commit""#.to_string(),
            format!(r#"{G2_PARAMS};alg="hmac-sha256""#),
        ] {
            let (input, sig) = g2_answer(&g2_covered(), "mac", &params, &[9; 32]);
            g2_verify(&g2_commit(), &g2_covered(), &input, &sig)
                .unwrap_or_else(|e| panic!("{params}: {e}"));
        }
    }

    #[test]
    fn a_commit_mac_is_refused_unless_it_is_this_requests() {
        let refused = |params: &str, secret: &[u8; 32]| {
            let (input, sig) = g2_answer(&g2_covered(), "mac", params, secret);
            g2_verify(&g2_commit(), &g2_covered(), &input, &sig).is_err()
        };
        assert!(refused(G2_PARAMS, &[8; 32]), "another secret");
        assert!(refused(G2_PARAMS, &[7; 32]), "the Oct 29 secret");
        assert!(refused(
            r#"("atproto-space-commit";sf);keyid="did:key:zDnaOther";tag="atproto-space-commit""#,
            &[9; 32]
        ));
        assert!(refused(
            &format!(r#"{G2_PARAMS};alg="hmac-sha512""#),
            &[9; 32]
        ));
        assert!(refused(
            r#"("atproto-space-commit");keyid="did:key:zDnaExample";tag="atproto-space-commit""#,
            &[9; 32]
        ));
        assert!(refused(
            r#"("atproto-space-commit";sf "authorization");tag="atproto-space-commit""#,
            &[9; 32]
        ));
        assert!(refused(
            r#"("atproto-space-record";sf);tag="atproto-space-record""#,
            &[9; 32]
        ));

        // Two signatures with the tag.
        let (input, sig) = g2_answer(&g2_covered(), "a", G2_PARAMS, &[9; 32]);
        let (input2, sig2) = g2_answer(&g2_covered(), "b", G2_PARAMS, &[9; 32]);
        assert!(
            g2_verify(
                &g2_commit(),
                &g2_covered(),
                &format!("{input}, {input2}"),
                &format!("{sig}, {sig2}")
            )
            .is_err()
        );

        // A tampered header.
        let (input, sig) = g2_answer(&g2_covered(), "mac", G2_PARAMS, &[9; 32]);
        let mut later = g2_commit();
        later.rev = "3kbcq3p7ad401".into();
        assert!(
            g2_verify(
                &later,
                &g2_covered().replace("ad400", "ad401"),
                &input,
                &sig
            )
            .is_err()
        );
    }

    #[test]
    fn a_commit_mac_must_describe_the_commit_and_the_repo_asked_for() {
        let (input, sig) = g2_answer(&g2_covered(), "mac", G2_PARAMS, &[9; 32]);
        let mut others = Vec::new();
        let mut c = g2_commit();
        c.ver = 2;
        others.push(c);
        let mut c = g2_commit();
        c.repo = "did:example:bob".into();
        others.push(c);
        let mut c = g2_commit();
        c.space = format!("{G2_SPACE}2");
        others.push(c);
        let mut c = g2_commit();
        c.rev = "3kbcq3p7ad401".into();
        others.push(c);
        let mut c = g2_commit();
        c.data = [2; 32];
        others.push(c);
        for c in others {
            assert!(g2_verify(&c, &g2_covered(), &input, &sig).is_err(), "{c:?}");
        }

        let c = g2_commit();
        let headers = CommitMacHeaders {
            commit: &g2_covered(),
            signature_input: &input,
            signature: &sig,
        };
        let ex = g2_exchange();
        assert!(verify_commit_signature(&c, &c.space, "did:example:bob", &ex, &headers).is_err());
        assert!(
            verify_commit_signature(
                &c,
                "at:space/did:example:space/app.bsky.group/other",
                &c.repo,
                &ex,
                &headers
            )
            .is_err()
        );
        // The space asked for in the other form is the same space.
        verify_commit_signature(
            &c,
            &crate::spaces::UriForm::Legacy.write(&c.space),
            &c.repo,
            &ex,
            &headers,
        )
        .expect("the same space");
    }

    fn header_map(pairs: &[(&'static str, &str)]) -> axum::http::HeaderMap {
        let mut map = axum::http::HeaderMap::new();
        for (name, value) in pairs {
            map.append(*name, axum::http::HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn a_response_is_verified_in_the_revision_its_host_answered_in() {
        let (input, sig) = g2_answer(&g2_covered(), "mac", G2_PARAMS, &[9; 32]);
        let c = g2_commit();
        let ex = g2_exchange();
        let g2 = header_map(&[
            (COMMIT_MAC_PROFILE, &g2_covered()),
            (SIGNATURE_INPUT_HEADER, &input),
            (SIGNATURE_HEADER, &sig),
        ]);
        verify_commit_response(&c, &c.space, &c.repo, &ex, &g2).expect("Nov 12");

        use base64::Engine;
        let g1_mac = commit_mac(&ex.shared_secret, &c).unwrap();
        let g1 = format!(
            "type=\"atproto-space-repo\", ver=1, mac=:{}:",
            base64::engine::general_purpose::STANDARD.encode(g1_mac)
        );
        verify_commit_response(
            &c,
            &c.space,
            &c.repo,
            &ex,
            &header_map(&[(MAC_HEADER, &g1)]),
        )
        .expect("Oct 29");

        // Neither.
        assert!(verify_commit_response(&c, &c.space, &c.repo, &ex, &header_map(&[])).is_err());
        // A Nov 12 answer that fails isn't rescued by an Oct 29 one beside it.
        let (bad_input, bad_sig) = g2_answer(&g2_covered(), "mac", G2_PARAMS, &[8; 32]);
        let both = header_map(&[
            (COMMIT_MAC_PROFILE, &g2_covered()),
            (SIGNATURE_INPUT_HEADER, &bad_input),
            (SIGNATURE_HEADER, &bad_sig),
            (MAC_HEADER, &g1),
        ]);
        assert!(verify_commit_response(&c, &c.space, &c.repo, &ex, &both).is_err());
    }

    #[test]
    fn both_revisions_are_asked_for() {
        let ex = g2_exchange();
        let headers = commit_mac_request_headers(&ex).unwrap();
        let names: Vec<_> = headers.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, [ACCEPT_SIGNATURE_HEADER, WANT_MAC_HEADER]);
        assert_eq!(headers[0].1, format!("mac={G2_PARAMS}").as_str());
        assert_eq!(headers[1].1, want_mac_header(KEY_SHARE).unwrap().as_str());
    }
}
