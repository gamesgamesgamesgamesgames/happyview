//! Interop against ZDS, a real independent PDS that serves atproto spaces.
//!
//! Ignored by default; they need the stack running:
//!
//!   docker compose -f docker-compose.zds.yml up -d --wait
//!   cargo test --test spaces_zds_interop -- --ignored --nocapture
//!   docker compose -f docker-compose.zds.yml down -v
//!
//! Run these on their own. The upstream image is amd64-only and runs under
//! emulation on Apple Silicon, which starves the machine enough to time out the
//! Postgres pool the rest of the suite uses.
//!
//! ZDS serves `community.lexicon.service.describe`, so it exercises tier 1 of
//! detection, and its `ZDS_PERMISSIONED_DATA` flag lets the same build stand in
//! for both a spaces-capable and a spaces-less PDS.

mod interop_support;

use base64::Engine;
use happyview::spaces::commit::{SpaceVerifyingKey, verify_commit};
use happyview::spaces::lthash::{LtHashState, record_element};
use happyview::spaces::native_client::parse_signed_commit;
use serde_json::{Value, json};
use uuid::Uuid;

const ZDS: &str = "http://localhost:2586";
const ZDS_NO_SPACES: &str = "http://localhost:2587";
const PLC: &str = "http://localhost:2582";

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn get_json(url: &str, token: Option<&str>) -> (u16, Value) {
    let mut req = client().get(url);
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    let resp = req
        .send()
        .await
        .expect("request to ZDS failed; is it running?");
    let status = resp.status().as_u16();
    let body = resp.json().await.unwrap_or(Value::Null);
    (status, body)
}

async fn post_json(url: &str, token: Option<&str>, body: Value) -> (u16, Value) {
    let mut req = client().post(url).json(&body);
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    let resp = req
        .send()
        .await
        .expect("request to ZDS failed; is it running?");
    let status = resp.status().as_u16();
    let body = resp.json().await.unwrap_or(Value::Null);
    (status, body)
}

fn enc(value: &str) -> String {
    urlencoding::encode(value).into_owned()
}

/// A fresh account, returning `(did, accessJwt)`.
///
/// ZDS accepts a password session, so the harness needs no OAuth client.
async fn new_account() -> (String, String) {
    // ZDS caps a handle label at 18 characters, so a full UUID will not fit.
    let handle = format!("iv{}.test", &Uuid::new_v4().simple().to_string()[..12]);
    let (status, body) = post_json(
        &format!("{ZDS}/xrpc/com.atproto.server.createAccount"),
        None,
        json!({
            "handle": handle,
            "email": format!("{handle}@test.com"),
            "password": "password123",
        }),
    )
    .await;
    assert!(status < 300, "createAccount failed ({status}): {body}");

    (
        body["did"].as_str().expect("did").to_string(),
        body["accessJwt"].as_str().expect("accessJwt").to_string(),
    )
}

// ---------------------------------------------------------------------------
// Capability detection
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn detection_reports_a_spaces_capable_pds_as_supported() {
    let got = happyview::spaces::pds_support::probe(&client(), ZDS).await;
    assert!(got.supported, "missing: {:?}", got.missing);
    assert_eq!(
        got.tier,
        happyview::spaces::pds_support::DetectionTier::Descriptor,
        "ZDS serves a descriptor, so tier 1 should answer without probing"
    );
}

#[tokio::test]
#[ignore]
async fn detection_reports_the_same_build_with_spaces_off_as_unsupported() {
    // The same image, one env var apart.
    let got = happyview::spaces::pds_support::probe(&client(), ZDS_NO_SPACES).await;
    assert!(!got.supported);
    assert_eq!(
        got.tier,
        happyview::spaces::pds_support::DetectionTier::Descriptor,
        "a descriptor that omits the space methods is a confident no"
    );
    assert!(!got.missing.is_empty());
}

#[tokio::test]
#[ignore]
async fn zds_still_uses_the_pre_split_method_spellings() {
    // Detection only reports ZDS as supported because the required-method list
    // accepts alternate spellings. ZDS predates the 2026-09-10 rename and
    // offers addMember rather than putMember; without the allowance a fully
    // capable PDS would read as unsupported.
    //
    // If this starts failing, ZDS has caught up and the allowance can be
    // reconsidered. It is a prompt to revisit, not a breakage.
    let (status, body) = get_json(
        &format!("{ZDS}/xrpc/community.lexicon.service.describe"),
        None,
    )
    .await;
    assert_eq!(status, 200);

    let methods: Vec<&str> = body["methods"]
        .as_array()
        .expect("methods")
        .iter()
        .map(|m| m["value"].as_str().unwrap())
        .collect();

    assert!(methods.contains(&"com.atproto.simplespace.addMember"));
    assert!(!methods.contains(&"com.atproto.simplespace.putMember"));
}

// ---------------------------------------------------------------------------
// Commits
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn a_commit_written_by_zds_verifies_and_agrees_with_our_set_hash() {
    let (did, token) = new_account().await;

    // ZDS predates the readPolicy/writePolicy split and takes a single
    // `policy`. This test is about commits, not space creation.
    let (status, body) = post_json(
        &format!("{ZDS}/xrpc/com.atproto.simplespace.createSpace"),
        Some(&token),
        json!({
            "type": "com.example.forum",
            "skey": "self",
            "policy": { "$type": "com.atproto.simplespace.defs#memberListPolicy" },
            "appAccess": { "$type": "com.atproto.simplespace.defs#open" },
        }),
    )
    .await;
    assert!(status < 300, "createSpace failed ({status}): {body}");
    let space_uri = body["uri"].as_str().expect("space uri").to_string();

    // Two records, so the set hash is over more than a trivial case.
    let mut expected = LtHashState::new();
    for text in ["first", "second"] {
        let (status, body) = post_json(
            &format!("{ZDS}/xrpc/com.atproto.space.createRecord"),
            Some(&token),
            json!({
                "space": space_uri,
                "repo": did,
                "collection": "com.example.note",
                "record": { "$type": "com.example.note", "text": text },
            }),
        )
        .await;
        assert!(status < 300, "createRecord failed ({status}): {body}");

        let uri = body["uri"].as_str().expect("record uri");
        let cid = body["cid"].as_str().expect("record cid");
        let rkey = uri.rsplit('/').next().expect("rkey");
        expected.add(&record_element("com.example.note", rkey, cid));
    }

    let (status, body) = get_json(
        &format!(
            "{ZDS}/xrpc/com.atproto.space.getLatestCommit?space={}&repo={}",
            enc(&space_uri),
            enc(&did)
        ),
        Some(&token),
    )
    .await;
    assert_eq!(status, 200, "getLatestCommit failed: {body}");

    let commit = parse_signed_commit(&body["commit"]).expect("commit parses");
    assert_eq!(commit.ver, 1);

    let key = author_key(&did).await;
    verify_commit(&commit, &space_uri, &did, &key)
        .expect("a commit written by ZDS must verify with our own code");

    // The migration turns on this check: our element encoding and LtHash must
    // produce the digest an independent implementation did over the same
    // records.
    assert_eq!(
        commit.hash,
        expected.hash(),
        "our set hash disagrees with ZDS's over the same two records"
    );
}

/// The account's `#atproto` signing key, from the DID document ZDS published.
///
/// Resolved through the PLC directory rather than the PDS: `describeRepo`
/// returns a didDoc carrying only the service entry, and a commit is verified
/// against the author's key.
async fn author_key(did: &str) -> SpaceVerifyingKey {
    let (status, body) = get_json(&format!("{PLC}/{}", enc(did)), None).await;
    assert_eq!(status, 200, "PLC resolution failed: {body}");

    let vm = body["verificationMethod"]
        .as_array()
        .expect("verificationMethod")
        .iter()
        .find(|v| v["id"].as_str().unwrap_or_default().ends_with("#atproto"))
        .expect("an #atproto verification method");

    let multibase = vm["publicKeyMultibase"]
        .as_str()
        .expect("publicKeyMultibase");
    happyview::spaces::credential::multikey_to_space_key(multibase)
        .expect("the published key must decode")
}

#[tokio::test]
#[ignore]
async fn replaying_the_zds_oplog_across_pages_reproduces_its_commit() {
    // ZDS's cursor format ("rev/idx") differs from the reference PDS's, and it
    // sends no `action`. A page limit of 2 forces the collector to follow
    // cursors.
    let (did, token) = new_account().await;
    let (_, body) = post_json(
        &format!("{ZDS}/xrpc/com.atproto.simplespace.createSpace"),
        Some(&token),
        json!({
            "type": "com.example.forum",
            "skey": "self",
            "policy": { "$type": "com.atproto.simplespace.defs#memberListPolicy" },
            "appAccess": { "$type": "com.atproto.simplespace.defs#open" },
        }),
    )
    .await;
    let space_uri = body["uri"].as_str().expect("space uri").to_string();

    let write = |method: &'static str, body: Value| {
        let (space_uri, did, token) = (space_uri.clone(), did.clone(), token.clone());
        async move {
            let mut body = body;
            body["space"] = json!(space_uri);
            body["repo"] = json!(did);
            let (status, resp) = post_json(
                &format!("{ZDS}/xrpc/com.atproto.space.{method}"),
                Some(&token),
                body,
            )
            .await;
            assert!(status < 300, "{method} failed ({status}): {resp}");
        }
    };
    let note = |rkey: &str, text: &str| {
        json!({ "collection": "com.example.note", "rkey": rkey,
                "record": { "$type": "com.example.note", "text": text } })
    };
    write("putRecord", note("a", "a")).await;
    write("putRecord", note("b", "b")).await;
    write("putRecord", note("b", "b2")).await;
    write(
        "deleteRecord",
        json!({ "collection": "com.example.note", "rkey": "a" }),
    )
    .await;
    write("putRecord", note("c", "c")).await;

    let ops =
        happyview::spaces::native_client::collect_repo_ops(&space_uri, &did, None, |mut params| {
            let token = token.clone();
            async move {
                params.push(("limit", "2".to_string()));
                let resp = client()
                    .get(format!("{ZDS}/xrpc/com.atproto.space.listRepoOps"))
                    .query(&params)
                    .bearer_auth(&token)
                    .send()
                    .await
                    .expect("listRepoOps request");
                assert_eq!(resp.status().as_u16(), 200);
                Ok(resp.json::<Value>().await.expect("listRepoOps json"))
            }
        })
        .await
        .expect("the oplog collects");
    assert_eq!(ops.len(), 5, "every page must be followed");

    use happyview::spaces::native_client::{OpAction, latest_op_per_record};
    let mut fold = LtHashState::new();
    for op in latest_op_per_record(&ops) {
        if op.action != OpAction::Delete {
            assert!(
                op.value.is_some(),
                "a record's final op must carry its value: {op:?}"
            );
            fold.add(&record_element(
                &op.collection,
                &op.rkey,
                op.cid.as_deref().unwrap(),
            ));
        }
    }

    let (_, body) = get_json(
        &format!(
            "{ZDS}/xrpc/com.atproto.space.getLatestCommit?space={}&repo={}",
            enc(&space_uri),
            enc(&did)
        ),
        Some(&token),
    )
    .await;
    let commit = parse_signed_commit(&body["commit"]).expect("commit parses");
    assert_eq!(fold.hash(), commit.hash);
}

#[tokio::test]
#[ignore]
async fn zds_encodes_commit_bytes_as_lexicon_bytes_not_bare_strings() {
    // The lexicon types these fields as `bytes`, which atproto renders as
    // {"$bytes": "<standard base64>"}, not as a bare base64url string.
    let (did, token) = new_account().await;

    let (_, body) = post_json(
        &format!("{ZDS}/xrpc/com.atproto.simplespace.createSpace"),
        Some(&token),
        json!({
            "type": "com.example.forum",
            "skey": "self",
            "policy": { "$type": "com.atproto.simplespace.defs#memberListPolicy" },
            "appAccess": { "$type": "com.atproto.simplespace.defs#open" },
        }),
    )
    .await;
    let space_uri = body["uri"].as_str().expect("space uri").to_string();

    post_json(
        &format!("{ZDS}/xrpc/com.atproto.space.createRecord"),
        Some(&token),
        json!({
            "space": space_uri,
            "repo": did,
            "collection": "com.example.note",
            "record": { "$type": "com.example.note", "text": "x" },
        }),
    )
    .await;

    let (_, body) = get_json(
        &format!(
            "{ZDS}/xrpc/com.atproto.space.getLatestCommit?space={}&repo={}",
            enc(&space_uri),
            enc(&did)
        ),
        Some(&token),
    )
    .await;

    for field in ["hash", "ikm", "sig", "mac"] {
        let value = &body["commit"][field];
        assert!(
            value.get("$bytes").and_then(|b| b.as_str()).is_some(),
            "{field} should be a lexicon bytes value, got {value}"
        );
        // Standard alphabet, not base64url: decoding as url-safe would corrupt
        // any value containing + or /.
        let raw = value["$bytes"].as_str().unwrap();
        base64::engine::general_purpose::STANDARD_NO_PAD
            .decode(raw.trim_end_matches('='))
            .unwrap_or_else(|e| panic!("{field} is not standard base64: {e}"));
    }
}

#[tokio::test]
#[ignore]
async fn the_migration_replay_lands_on_zds_with_the_hash_it_expects() {
    let (did, token) = new_account().await;
    let (_, body) = post_json(
        &format!("{ZDS}/xrpc/com.atproto.simplespace.createSpace"),
        Some(&token),
        json!({
            "type": "com.example.forum",
            "skey": "self",
            "policy": { "$type": "com.atproto.simplespace.defs#memberListPolicy" },
            "appAccess": { "$type": "com.atproto.simplespace.defs#open" },
        }),
    )
    .await;
    let space = body["uri"].as_str().expect("space uri").to_string();

    let mut records =
        interop_support::records_awaiting_migration(&space, &did, interop_support::Sample::Full);
    // ZDS refuses unpadded `$bytes` (pinned below), and the sample is unpadded
    // as the reference PDS emits it. Padding names the same bytes, so the CIDs
    // the migration expects are unchanged.
    for r in &mut records {
        let raw = &mut r.record["raw"]["$bytes"];
        let unpadded = raw.as_str().unwrap().to_string();
        *raw = json!(format!(
            "{unpadded}{}",
            "=".repeat((4 - unpadded.len() % 4) % 4)
        ));
    }
    let commit = interop_support::replay_as_migration(ZDS, &token, &space, &did, &records).await;

    let disagreements =
        interop_support::cid_disagreements(ZDS, &token, &space, &did, &records).await;
    assert!(disagreements.is_empty(), "{disagreements:#?}");

    let commit = parse_signed_commit(&commit).expect("parses");
    verify_commit(&commit, &space, &did, &author_key(&did).await).expect("authentic");
    assert_eq!(commit.hash, interop_support::migration_expects(&records));
}

#[tokio::test]
#[ignore]
async fn zds_rejects_unpadded_lexicon_bytes_in_records() {
    // The reference PDS accepts `$bytes` with or without padding, and emits it
    // unpadded in commits. ZDS emits padding and refuses a record without it,
    // as outside the data model. HappyView replays record values as they
    // were written, so a repo holding unpadded bytes cannot migrate to ZDS:
    // applyWrites fails and the repo stays on HappyView.
    //
    // If this fails, ZDS has relaxed and the padding step in the migration test
    // above can go.
    let (did, token) = new_account().await;
    let (_, body) = post_json(
        &format!("{ZDS}/xrpc/com.atproto.simplespace.createSpace"),
        Some(&token),
        json!({
            "type": "com.example.forum",
            "skey": "self",
            "policy": { "$type": "com.atproto.simplespace.defs#memberListPolicy" },
            "appAccess": { "$type": "com.atproto.simplespace.defs#open" },
        }),
    )
    .await;
    let space_uri = body["uri"].as_str().expect("space uri").to_string();

    let put = |bytes: &'static str| {
        let (space_uri, did, token) = (space_uri.clone(), did.clone(), token.clone());
        async move {
            post_json(
                &format!("{ZDS}/xrpc/com.atproto.space.putRecord"),
                Some(&token),
                json!({
                    "space": space_uri,
                    "repo": did,
                    "collection": "com.example.note",
                    "rkey": if bytes.ends_with('=') { "padded" } else { "unpadded" },
                    "record": { "$type": "com.example.note", "raw": { "$bytes": bytes } },
                }),
            )
            .await
        }
    };
    let (unpadded, body) = put("AAECAwQFBgc").await;
    assert_eq!(unpadded, 400, "{body}");
    let (padded, body) = put("AAECAwQFBgc=").await;
    assert!(padded < 300, "{body}");
}
