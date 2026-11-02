//! Reading a permissioned repo from the user's own PDS.
//!
//! Used to verify a migration and to sync a repo once it is
//! [`native`](crate::spaces::host_mode::HostMode::Native).
//!
//! Everything here goes through the user's OAuth session (`repo::get_oauth_session`),
//! which is HappyView acting as an OAuth *client* against their PDS.

use crate::AppState;
use crate::error::AppError;
use crate::spaces::commit::{SignedCommit, SpaceVerifyingKey, decode_lex_bytes};
use crate::spaces::space_mac;
use atrium_xrpc::http::{HeaderName, HeaderValue};

/// What an oplog entry did to its record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpAction {
    Create,
    Update,
    Delete,
}

/// One entry from `listRepoOps`.
#[derive(Debug, Clone)]
pub struct RepoOp {
    pub rev: String,
    pub action: OpAction,
    pub collection: String,
    pub rkey: String,
    pub cid: Option<String>,
    pub prev: Option<String>,
    /// Inlined record value. Absent for a delete, when the caller asked for
    /// metadata only, or when a later op has superseded this one.
    pub value: Option<serde_json::Value>,
}

/// Parse one page of a `listRepoOps` response into its ops and next cursor.
///
/// An `opEntry` carries no action: it is implied by which CIDs are null. A null
/// `cid` is a delete, a null `prev` a create, and both present an update. Both
/// the reference PDS and ZDS send this shape.
///
/// A malformed entry fails the whole page. Skipping it would apply the rest,
/// and the set hash would then disagree for a reason no log explains.
pub fn parse_repo_ops_page(
    body: &serde_json::Value,
) -> Result<(Vec<RepoOp>, Option<String>), AppError> {
    let entries = body
        .get("ops")
        .and_then(|v| v.as_array())
        .ok_or_else(|| AppError::Internal("listRepoOps returned no ops array".into()))?;

    let str_field = |op: &serde_json::Value, field: &str| -> Result<String, AppError> {
        op.get(field)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| AppError::Internal(format!("listRepoOps entry is missing {field}")))
    };
    let nullable_cid = |op: &serde_json::Value, field: &str| {
        op.get(field).and_then(|v| v.as_str()).map(str::to_string)
    };

    let mut ops = Vec::with_capacity(entries.len());
    for op in entries {
        let cid = nullable_cid(op, "cid");
        let prev = nullable_cid(op, "prev");
        let action = match (&cid, &prev) {
            (None, Some(_)) => OpAction::Delete,
            (Some(_), None) => OpAction::Create,
            (Some(_), Some(_)) => OpAction::Update,
            (None, None) => {
                return Err(AppError::Internal(
                    "listRepoOps entry has neither cid nor prev".into(),
                ));
            }
        };
        ops.push(RepoOp {
            rev: str_field(op, "rev")?,
            action,
            collection: str_field(op, "collection")?,
            rkey: str_field(op, "rkey")?,
            cid,
            prev,
            value: op.get("value").filter(|v| !v.is_null()).cloned(),
        });
    }

    let cursor = body
        .get("cursor")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Ok((ops, cursor))
}

/// Walk every page of `listRepoOps` from `since`, via `fetch`.
///
/// `fetch` performs one request with the given query parameters. Kept separate
/// from the transport so the paging itself can be driven against a real host
/// without an OAuth session.
///
/// Reading only the first page does more than slow sync down: the next sync
/// starts from the same cursor, gets the same page, and never catches up.
pub async fn collect_repo_ops<F, Fut>(
    space_uri: &str,
    author_did: &str,
    since: Option<&str>,
    mut fetch: F,
) -> Result<Vec<RepoOp>, AppError>
where
    F: FnMut(Vec<(&'static str, String)>) -> Fut,
    Fut: std::future::Future<Output = Result<serde_json::Value, AppError>>,
{
    let mut all: Vec<RepoOp> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut params = vec![
            ("space", space_uri.to_string()),
            ("repo", author_did.to_string()),
        ];
        match &cursor {
            None => {
                if let Some(s) = since {
                    params.push(("since", s.to_string()));
                }
            }
            Some(c) => {
                params.push(("cursor", c.clone()));
                // Also the last rev seen, as `since`. The spec gives `cursor`
                // precedence, so a conforming host ignores it; pds.js ignores
                // `cursor` instead and pages by `since` alone, and without this
                // re-serves the first page forever.
                if let Some(last) = all.last() {
                    params.push(("since", last.rev.clone()));
                }
            }
        }

        let (ops, next) = parse_repo_ops_page(&fetch(params).await?)?;
        all.extend(ops);

        match next {
            Some(next) if Some(&next) == cursor.as_ref() => {
                return Err(AppError::Internal(
                    "listRepoOps returned the same cursor twice".into(),
                ));
            }
            Some(next) => cursor = Some(next),
            None => return Ok(all),
        }
    }
}

fn decode_32(value: &serde_json::Value, field: &str) -> Result<[u8; 32], AppError> {
    decode_lex_bytes(value, field)?
        .try_into()
        .map_err(|_| AppError::Internal(format!("commit {field} is not 32 bytes")))
}

/// Parse a `signedCommit` as it appears in a `getLatestCommit` response.
pub fn parse_signed_commit(commit: &serde_json::Value) -> Result<SignedCommit, AppError> {
    let ver = commit.get("ver").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    Ok(SignedCommit {
        ver,
        hash: decode_32(commit, "hash")?,
        ikm: decode_32(commit, "ikm")?,
        sig: decode_lex_bytes(commit, "sig")?,
        mac: decode_32(commit, "mac")?,
        rev: str_field(commit, "rev")?,
    })
}

/// Parse a proposal 0016 `commit`, which carries `data` where a signed commit
/// carried `hash`.
pub fn parse_mac_commit(commit: &serde_json::Value) -> Result<space_mac::Commit, AppError> {
    let ver = commit.get("ver").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    Ok(space_mac::Commit {
        ver,
        repo: str_field(commit, "repo")?,
        space: str_field(commit, "space")?,
        rev: str_field(commit, "rev")?,
        data: decode_32(commit, "data")?,
    })
}

fn str_field(commit: &serde_json::Value, field: &str) -> Result<String, AppError> {
    commit
        .get(field)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| AppError::Internal(format!("commit is missing {field}")))
}

/// A commit from a repo's host, after it has been authenticated.
#[derive(Debug, Clone)]
pub enum VerifiedCommit {
    /// The pre-0016 shape, signed by the author's `#atproto` key.
    Signed(SignedCommit),
    /// The 0016 shape, authenticated by the MAC its response carried.
    Mac(space_mac::Commit),
}

impl VerifiedCommit {
    pub fn rev(&self) -> &str {
        match self {
            Self::Signed(c) => &c.rev,
            Self::Mac(c) => &c.rev,
        }
    }

    /// The sha256 of the repo's LtHash state.
    pub fn hash(&self) -> &[u8; 32] {
        match self {
            Self::Signed(c) => &c.hash,
            Self::Mac(c) => &c.data,
        }
    }
}

/// Resolve the key a repo's commits are signed with: the **author's** account
/// key.
///
/// This is not `credential::resolve_space_key`, which prefers
/// `#atproto_space` and answers a different question. A space *credential* is
/// signed by the space authority; a repo *commit* is signed by the account whose
/// records it summarises, always with that account's `#atproto` key.
pub async fn author_signing_key(
    http: &reqwest::Client,
    plc_url: &str,
    author_did: &str,
) -> Result<SpaceVerifyingKey, AppError> {
    let did_doc = crate::profile::resolve_did_document(http, plc_url, author_did).await?;
    signing_key_from_doc(&did_doc, author_did)
}

fn signing_key_from_doc(
    did_doc: &crate::profile::DidDocument,
    author_did: &str,
) -> Result<SpaceVerifyingKey, AppError> {
    let vm = did_doc
        .verification_method
        .iter()
        .find(|v| v.id.ends_with("#atproto"))
        .ok_or_else(|| {
            AppError::Auth(format!(
                "author {author_did} has no #atproto verification method"
            ))
        })?;
    let multibase = vm.public_key_multibase.as_deref().ok_or_else(|| {
        AppError::Auth("#atproto verification method missing publicKeyMultibase".into())
    })?;
    crate::spaces::credential::multikey_to_space_key(multibase)
}

/// The current commit for a repo on its host, authenticated.
///
/// Hosts are mid-transition between two commit models, so this accepts both
/// and lets the host's answer pick the check. When the author publishes a
/// `#atproto_kem` key we always ask for a MAC: under OAuth 0016 makes it
/// optional, but it is what ties the answer to the author's DID document, as
/// the signature did. A host that predates 0016 ignores the header.
pub async fn get_verified_commit(
    state: &AppState,
    session: &crate::HappyViewOAuthSession,
    space_uri: &str,
    author_did: &str,
) -> Result<VerifiedCommit, AppError> {
    let did_doc =
        crate::profile::resolve_did_document(&state.http, &state.config.plc_url, author_did)
            .await?;
    let exchange = space_mac::author_kem_key(&did_doc)?.map(|key| space_mac::encapsulate(&key));

    let params = [
        ("space", space_uri.to_string()),
        // `repo`, not `did`: ZDS answers InvalidRequest "Missing repo"
        // without it.
        ("repo", author_did.to_string()),
    ];
    let request = crate::repo::pds::pds_get_json(
        state,
        session,
        "com.atproto.space.getLatestCommit",
        &params,
    );
    let (body, mac_header) = match &exchange {
        Some(exchange) => {
            let want = space_mac::want_mac_header(&exchange.key_share)?;
            let (body, received) = crate::http_retry::with_header_exchange(
                (
                    HeaderName::from_static(space_mac::WANT_MAC_HEADER),
                    HeaderValue::from_str(&want)
                        .map_err(|_| AppError::Internal("unsendable MAC request".into()))?,
                ),
                HeaderName::from_static(space_mac::MAC_HEADER),
                request,
            )
            .await;
            (body?, received)
        }
        None => (request.await?, None),
    };

    let commit = body
        .get("commit")
        .ok_or_else(|| AppError::Internal("getLatestCommit returned no commit".into()))?;
    verify_remote_commit(
        commit,
        space_uri,
        author_did,
        &did_doc,
        exchange.as_ref(),
        mac_header.as_ref(),
    )
}

/// Authenticate a commit as one of the two shapes a host may answer with.
pub fn verify_remote_commit(
    commit: &serde_json::Value,
    space_uri: &str,
    author_did: &str,
    did_doc: &crate::profile::DidDocument,
    exchange: Option<&space_mac::Encapsulation>,
    mac_header: Option<&HeaderValue>,
) -> Result<VerifiedCommit, AppError> {
    if commit.get("data").is_none() {
        let commit = parse_signed_commit(commit)?;
        let key = signing_key_from_doc(did_doc, author_did)?;
        crate::spaces::commit::verify_commit(&commit, space_uri, author_did, &key)?;
        return Ok(VerifiedCommit::Signed(commit));
    }

    let commit = parse_mac_commit(commit)?;
    let exchange = exchange.ok_or_else(|| {
        AppError::Auth(format!(
            "author {author_did} publishes no #atproto_kem key, so their host's commit \
             cannot be authenticated"
        ))
    })?;
    let mac_header = mac_header.ok_or_else(|| {
        AppError::Auth(format!(
            "the repo host sent a commit without the {} header it was asked for",
            space_mac::MAC_HEADER
        ))
    })?;
    let mac =
        space_mac::parse_mac_header(mac_header.to_str().map_err(|_| {
            AppError::Auth(format!("{} header is not text", space_mac::MAC_HEADER))
        })?)?;
    space_mac::verify_commit(
        &commit,
        space_uri,
        author_did,
        &exchange.shared_secret,
        &mac,
    )?;
    Ok(VerifiedCommit::Mac(commit))
}

/// A repo's full operation log since `since`, inlining record values.
pub async fn list_repo_ops(
    state: &AppState,
    session: &crate::HappyViewOAuthSession,
    space_uri: &str,
    author_did: &str,
    since: Option<&str>,
) -> Result<Vec<RepoOp>, AppError> {
    collect_repo_ops(space_uri, author_did, since, |params| async move {
        crate::repo::pds::pds_get_json(state, session, "com.atproto.space.listRepoOps", &params)
            .await
    })
    .await
}

/// The last op for each record, in log order.
///
/// A window of the oplog can touch one record several times, and the host
/// withholds the value of every op but the latest. Applying only each record's
/// final op is what the index needs, and it is the only order in which every op
/// applied carries a value.
pub fn latest_op_per_record(ops: &[RepoOp]) -> Vec<&RepoOp> {
    let mut last: std::collections::HashMap<(&str, &str), usize> = std::collections::HashMap::new();
    for (i, op) in ops.iter().enumerate() {
        last.insert((op.collection.as_str(), op.rkey.as_str()), i);
    }
    ops.iter()
        .enumerate()
        .filter(|(i, op)| last[&(op.collection.as_str(), op.rkey.as_str())] == *i)
        .map(|(_, op)| op)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spaces::commit::{encode_lex_bytes, sign_commit, verify_commit};
    use crate::spaces::lthash::{LtHashState, record_element};

    const SPACE: &str = "at://did:plc:auth/space/com.example.forum/main";
    const AUTHOR: &str = "did:plc:author";

    fn commit_json(c: &SignedCommit) -> serde_json::Value {
        serde_json::json!({
            "ver": c.ver,
            "hash": encode_lex_bytes(&c.hash),
            "ikm": encode_lex_bytes(&c.ikm),
            "sig": encode_lex_bytes(&c.sig),
            "mac": encode_lex_bytes(&c.mac),
            "rev": c.rev,
        })
    }

    /// A page in the shape both real hosts send: no `action`, and a superseded
    /// op with its value withheld.
    fn oplog_page() -> serde_json::Value {
        serde_json::json!({
            "ops": [
                { "rev": "3a", "collection": "com.example.note", "rkey": "a", "cid": "bafya", "prev": null },
                { "rev": "3b", "collection": "com.example.note", "rkey": "b", "cid": "bafyb", "prev": null,
                  "value": { "$type": "com.example.note", "text": "b" } },
                { "rev": "3c", "collection": "com.example.note", "rkey": "b", "cid": "bafyb2", "prev": "bafyb",
                  "value": { "$type": "com.example.note", "text": "b2" } },
                { "rev": "3d", "collection": "com.example.note", "rkey": "a", "cid": null, "prev": "bafya" },
            ]
        })
    }

    #[test]
    fn an_ops_action_is_implied_by_which_cids_are_null() {
        let (ops, cursor) = parse_repo_ops_page(&oplog_page()).expect("parses");
        let actions: Vec<OpAction> = ops.iter().map(|o| o.action).collect();
        assert_eq!(
            actions,
            [
                OpAction::Create,
                OpAction::Create,
                OpAction::Update,
                OpAction::Delete
            ]
        );
        assert_eq!(ops.len(), 4, "no entry may be dropped");
        assert!(cursor.is_none());
    }

    #[test]
    fn a_malformed_op_fails_the_page_rather_than_being_skipped() {
        let mut page = oplog_page();
        page["ops"][1].as_object_mut().unwrap().remove("rkey");
        assert!(parse_repo_ops_page(&page).is_err());

        let mut page = oplog_page();
        page["ops"][0]["cid"] = serde_json::Value::Null;
        assert!(
            parse_repo_ops_page(&page).is_err(),
            "an op with neither cid nor prev describes nothing"
        );
    }

    #[test]
    fn only_each_records_final_op_is_applied() {
        let (ops, _) = parse_repo_ops_page(&oplog_page()).unwrap();
        let latest: Vec<&str> = latest_op_per_record(&ops)
            .iter()
            .map(|o| o.rev.as_str())
            .collect();
        // `a` was created (value withheld) then deleted; `b` created then updated.
        assert_eq!(latest, ["3c", "3d"]);
    }

    #[tokio::test]
    async fn every_page_is_collected_following_the_cursor() {
        let requests = std::sync::Mutex::new(Vec::new());
        let ops = collect_repo_ops(SPACE, AUTHOR, Some("3start"), |params| {
            let page = requests.lock().unwrap().len();
            requests.lock().unwrap().push(params);
            async move {
                Ok(match page {
                    0 => serde_json::json!({ "ops": [
                        { "rev": "3a", "collection": "c.n", "rkey": "a", "cid": "bafya", "prev": null }
                    ], "cursor": "3a/0" }),
                    _ => serde_json::json!({ "ops": [
                        { "rev": "3b", "collection": "c.n", "rkey": "b", "cid": "bafyb", "prev": null }
                    ] }),
                })
            }
        })
        .await
        .expect("collects");

        assert_eq!(ops.len(), 2);
        let requests = requests.into_inner().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].contains(&("since", "3start".to_string())));
        assert!(requests[1].contains(&("cursor", "3a/0".to_string())));
        assert!(
            requests[1].contains(&("since", "3a".to_string())),
            "a continuation also carries the last rev, for hosts that ignore cursor"
        );
    }

    #[tokio::test]
    async fn a_cursor_that_does_not_advance_is_an_error_not_a_hang() {
        let result = collect_repo_ops(SPACE, AUTHOR, None, |_| async {
            Ok(serde_json::json!({ "ops": [], "cursor": "stuck" }))
        })
        .await;
        assert!(result.is_err());
    }

    #[test]
    fn a_commit_from_the_wire_verifies_against_the_signing_key() {
        let signing = p256::ecdsa::SigningKey::from_slice(&[0x61u8; 32]).unwrap();
        let key = SpaceVerifyingKey::P256(*signing.verifying_key());

        let original = sign_commit(&[0xAAu8; 32], SPACE, AUTHOR, "3krev", &signing).unwrap();
        let parsed = parse_signed_commit(&commit_json(&original)).expect("parses");

        assert_eq!(parsed.rev, original.rev);
        assert_eq!(parsed.hash, original.hash);
        verify_commit(&parsed, SPACE, AUTHOR, &key).expect("a round-tripped commit must verify");
    }

    #[test]
    fn a_commit_missing_a_field_is_rejected_rather_than_defaulted() {
        // A commit we cannot fully parse is not evidence of anything, and
        // defaulting a field would let it verify against the wrong bytes.
        for field in ["hash", "ikm", "sig", "mac", "rev"] {
            let signing = p256::ecdsa::SigningKey::from_slice(&[0x61u8; 32]).unwrap();
            let c = sign_commit(&[0xAAu8; 32], SPACE, AUTHOR, "3krev", &signing).unwrap();
            let mut json = commit_json(&c);
            json.as_object_mut().unwrap().remove(field);
            assert!(
                parse_signed_commit(&json).is_err(),
                "a commit without {field} must not parse"
            );
        }
    }

    #[test]
    fn a_hash_disagreeing_with_our_records_is_detectable() {
        // The migration's agreement check: the hash the PDS signed is compared
        // against a fold over the records we sent.
        let signing = p256::ecdsa::SigningKey::from_slice(&[0x61u8; 32]).unwrap();
        let key = SpaceVerifyingKey::P256(*signing.verifying_key());

        let mut ours = LtHashState::new();
        ours.add(&record_element("com.example.note", "a", "bafyaaa"));
        ours.add(&record_element("com.example.note", "b", "bafybbb"));

        // The PDS reports a repo holding only the first record.
        let mut theirs = LtHashState::new();
        theirs.add(&record_element("com.example.note", "a", "bafyaaa"));

        let commit = sign_commit(&theirs.hash(), SPACE, AUTHOR, "3krev", &signing).unwrap();

        // The commit is authentic but does not describe the record set we
        // replayed.
        verify_commit(&commit, SPACE, AUTHOR, &key).expect("authentic");
        assert_ne!(
            commit.hash,
            ours.hash(),
            "a set-hash mismatch must be visible after the signature verifies"
        );
    }

    /// A DID document for `AUTHOR` publishing the given keys as multikeys.
    fn did_doc(
        signing: Option<&p256::ecdsa::SigningKey>,
        kem: Option<&p256::SecretKey>,
    ) -> crate::profile::DidDocument {
        use p256::elliptic_curve::sec1::ToSec1Point;
        let multikey = |point: &[u8]| {
            let mut bytes = vec![0x80, 0x24];
            bytes.extend_from_slice(point);
            multibase::encode(multibase::Base::Base58Btc, bytes)
        };
        let mut methods = Vec::new();
        if let Some(key) = signing {
            methods.push(serde_json::json!({
                "id": format!("{AUTHOR}#atproto"),
                "type": "Multikey",
                "publicKeyMultibase": multikey(key.verifying_key().to_sec1_point(true).as_bytes()),
            }));
        }
        if let Some(key) = kem {
            methods.push(serde_json::json!({
                "id": format!("{AUTHOR}#atproto_kem"),
                "type": "Multikey",
                "publicKeyMultibase": multikey(key.public_key().to_sec1_point(true).as_bytes()),
            }));
        }
        serde_json::from_value(serde_json::json!({ "verificationMethod": methods })).unwrap()
    }

    fn kem_key() -> p256::SecretKey {
        p256::SecretKey::from_slice(&[0x11u8; 32]).unwrap()
    }

    /// A 0016 commit, and the MAC header its host would send for `exchange`.
    fn mac_commit(
        exchange: &space_mac::Encapsulation,
        data: [u8; 32],
    ) -> (serde_json::Value, HeaderValue) {
        use base64::Engine;
        let commit = space_mac::Commit {
            ver: 1,
            repo: AUTHOR.into(),
            space: SPACE.into(),
            rev: "3krev".into(),
            data,
        };
        let mac = space_mac::commit_mac(&exchange.shared_secret, &commit).unwrap();
        let header = format!(
            "type=\"atproto-space-repo\", ver=1, mac=:{}:",
            base64::engine::general_purpose::STANDARD.encode(mac)
        );
        let json = serde_json::json!({
            "ver": 1,
            "repo": AUTHOR,
            "space": SPACE,
            "rev": "3krev",
            "data": encode_lex_bytes(&data),
        });
        (json, HeaderValue::from_str(&header).unwrap())
    }

    #[test]
    fn a_0016_commit_verifies_by_its_mac() {
        let doc = did_doc(None, Some(&kem_key()));
        let exchange = space_mac::encapsulate(&space_mac::author_kem_key(&doc).unwrap().unwrap());
        let (json, header) = mac_commit(&exchange, [0xAA; 32]);

        let commit =
            verify_remote_commit(&json, SPACE, AUTHOR, &doc, Some(&exchange), Some(&header))
                .expect("a MACed commit must verify");
        assert!(matches!(commit, VerifiedCommit::Mac(_)));
        assert_eq!(commit.hash(), &[0xAA; 32]);
        assert_eq!(commit.rev(), "3krev");
    }

    #[test]
    fn a_0016_commit_is_refused_without_a_mac_or_a_kem_key() {
        let doc = did_doc(None, Some(&kem_key()));
        let exchange = space_mac::encapsulate(&space_mac::author_kem_key(&doc).unwrap().unwrap());
        let (json, header) = mac_commit(&exchange, [0xAA; 32]);

        // The host ignored the request for a MAC.
        assert!(verify_remote_commit(&json, SPACE, AUTHOR, &doc, Some(&exchange), None).is_err());
        // The author publishes no KEM key, so no MAC could be asked for.
        assert!(
            verify_remote_commit(
                &json,
                SPACE,
                AUTHOR,
                &did_doc(None, None),
                None,
                Some(&header)
            )
            .is_err()
        );
        // A MAC computed under another exchange is not this request's.
        let other = space_mac::encapsulate(&space_mac::author_kem_key(&doc).unwrap().unwrap());
        assert!(
            verify_remote_commit(&json, SPACE, AUTHOR, &doc, Some(&other), Some(&header)).is_err()
        );
    }

    #[test]
    fn a_signed_commit_still_verifies_when_a_mac_was_asked_for() {
        // A host that predates 0016 ignores the request header and answers with
        // the signed shape.
        let signing = p256::ecdsa::SigningKey::from_slice(&[0x61u8; 32]).unwrap();
        let doc = did_doc(Some(&signing), Some(&kem_key()));
        let exchange = space_mac::encapsulate(&space_mac::author_kem_key(&doc).unwrap().unwrap());
        let signed = sign_commit(&[0xAAu8; 32], SPACE, AUTHOR, "3krev", &signing).unwrap();

        let commit = verify_remote_commit(
            &commit_json(&signed),
            SPACE,
            AUTHOR,
            &doc,
            Some(&exchange),
            None,
        )
        .expect("a signed commit must verify");
        assert!(matches!(commit, VerifiedCommit::Signed(_)));
        assert_eq!(commit.hash(), &[0xAA; 32]);
    }

    #[test]
    fn a_matching_hash_confirms_the_handoff() {
        let signing = p256::ecdsa::SigningKey::from_slice(&[0x61u8; 32]).unwrap();
        let key = SpaceVerifyingKey::P256(*signing.verifying_key());

        let mut fold = LtHashState::new();
        fold.add(&record_element("com.example.note", "a", "bafyaaa"));

        let commit = sign_commit(&fold.hash(), SPACE, AUTHOR, "3krev", &signing).unwrap();
        verify_commit(&commit, SPACE, AUTHOR, &key).expect("authentic");
        assert_eq!(commit.hash, fold.hash());
    }
}
