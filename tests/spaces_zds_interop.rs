//! Interop against ZDS, a real independent PDS that serves atproto spaces.
//!
//! Ignored by default; they need the stack running:
//!
//!   docker compose -f docker-compose.zds.yml up -d --wait
//!   cargo test --test spaces_zds_interop -- --ignored --nocapture
//!   docker compose -f docker-compose.zds.yml down -v
//!
//! ZDS serves `community.lexicon.service.describe`, so it exercises tier 1 of
//! detection, and its `ZDS_PERMISSIONED_DATA` flag lets the same build stand in
//! for both a spaces-capable and a spaces-less PDS.

mod interop_support;

use base64::Engine;
use happyview::spaces::commit::{SpaceVerifyingKey, verify_commit};
use happyview::spaces::lthash::{LtHashState, record_element};
use happyview::spaces::native_client::parse_signed_commit;
use happyview::spaces::types::{AppAccess, MemberAccess, Policy, ResolvedMember};
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

/// Which spaces alpha the ZDS under test speaks. The October 29 alpha replaced
/// `simplespace`'s read policy, write policy and app access with one `policy`
/// union under `space.*`, moved members to `memberspace.*`, and kept no alias.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Generation {
    Oct22,
    Oct29,
}

/// Read from ZDS's own method descriptor, so each test asks the host rather
/// than assuming the pinned image.
async fn generation() -> Generation {
    let (status, body) = get_json(
        &format!("{ZDS}/xrpc/community.lexicon.service.describe"),
        None,
    )
    .await;
    assert_eq!(status, 200, "describe failed: {body}");
    let has = |nsid: &str| {
        body["methods"]
            .as_array()
            .expect("methods")
            .iter()
            .any(|m| m["value"].as_str() == Some(nsid))
    };
    if has("com.atproto.space.createSpace") {
        Generation::Oct29
    } else {
        Generation::Oct22
    }
}

/// An October 29 space under `policy`, with the same type and key the October
/// 22 helper uses.
async fn create_space_with_policy(token: &str, policy: Value) -> String {
    let (status, body) = post_json(
        &format!("{ZDS}/xrpc/com.atproto.space.createSpace"),
        Some(token),
        json!({
            "spaceType": "com.example.forum",
            "skey": "self",
            "policy": policy,
        }),
    )
    .await;
    assert!(status < 300, "createSpace failed ({status}): {body}");
    body["uri"].as_str().expect("space uri").to_string()
}

/// A space created with HappyView's own policy and app-access types, so ZDS
/// accepting it checks how we encode the unions. October 22 only.
async fn create_space(token: &str, read: &Policy, write: &Policy, app: &AppAccess) -> String {
    let (status, body) = post_json(
        &format!("{ZDS}/xrpc/com.atproto.simplespace.createSpace"),
        Some(token),
        json!({
            // `spaceType` and `type` to handle backwards compat.
            "spaceType": "com.example.forum",
            "type": "com.example.forum",
            "skey": "self",
            "readPolicy": read,
            "writePolicy": write,
            "appAccess": app,
        }),
    )
    .await;
    assert!(status < 300, "createSpace failed ({status}): {body}");
    body["uri"].as_str().expect("space uri").to_string()
}

/// A space whose reads and writes are limited to members, on either
/// generation.
async fn default_space(token: &str) -> String {
    match generation().await {
        Generation::Oct22 => {
            create_space(
                token,
                &Policy::MemberList,
                &Policy::MemberList,
                &AppAccess::Open,
            )
            .await
        }
        Generation::Oct29 => {
            create_space_with_policy(token, json!({ "$type": MEMBERSPACE_POLICY })).await
        }
    }
}

const MEMBERSPACE_POLICY: &str = "com.atproto.memberspace.policy";

/// The member methods' namespace for this ZDS.
async fn members_ns() -> &'static str {
    match generation().await {
        Generation::Oct22 => "com.atproto.simplespace",
        Generation::Oct29 => "com.atproto.memberspace",
    }
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
async fn zds_advertises_put_member_under_its_current_name() {
    // Detection still accepts addMember for pds.js, but ZDS should satisfy the
    // required list with canonical spellings alone: `simplespace.putMember` on
    // the October 22 alpha, `memberspace.putMember` from October 29.
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

    let put_member = format!("{}.putMember", members_ns().await);
    assert!(
        methods.contains(&put_member.as_str()),
        "{put_member} missing from {methods:?}"
    );
}

// ---------------------------------------------------------------------------
// simplespace: HappyView's types against ZDS
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn our_policy_and_app_access_unions_round_trip_through_zds() {
    let (_, token) = new_account().await;
    if generation().await == Generation::Oct29 {
        // HappyView has no October 29 policy types yet, so the policy is
        // checked as JSON: what ZDS stores is what `getSpace` gives back.
        let policy = json!({
            "$type": MEMBERSPACE_POLICY,
            "publicRead": true,
            "allowedApps": ["https://app.example/client-metadata.json"],
        });
        let space = create_space_with_policy(&token, policy.clone()).await;
        let (status, body) = get_json(
            &format!(
                "{ZDS}/xrpc/com.atproto.space.getSpace?space={}",
                enc(&space)
            ),
            Some(&token),
        )
        .await;
        assert_eq!(status, 200, "getSpace failed: {body}");
        assert_eq!(body["uri"], json!(space));
        for (field, value) in policy.as_object().unwrap() {
            assert_eq!(&body["policy"][field], value, "policy.{field}: {body}");
        }
        return;
    }
    let read = Policy::Public;
    let write = Policy::ManagingApp {
        managing_app: "did:web:app.example#atproto_space".into(),
    };
    let app = AppAccess::AllowList {
        allowed: vec!["https://app.example/client-metadata.json".into()],
    };
    let space = create_space(&token, &read, &write, &app).await;

    let (status, body) = get_json(
        &format!(
            "{ZDS}/xrpc/com.atproto.simplespace.getSpace?space={}",
            enc(&space)
        ),
        Some(&token),
    )
    .await;
    assert_eq!(status, 200, "getSpace failed: {body}");

    let got_read: Policy = serde_json::from_value(body["readPolicy"].clone()).expect("readPolicy");
    let got_write: Policy =
        serde_json::from_value(body["writePolicy"].clone()).expect("writePolicy");
    let got_app: AppAccess = serde_json::from_value(body["appAccess"].clone()).expect("appAccess");
    assert_eq!(got_read, read);
    assert_eq!(got_write, write);
    assert_eq!(got_app, app);
}

#[tokio::test]
#[ignore]
async fn our_member_shape_round_trips_through_put_member_and_list_members() {
    let (_, token) = new_account().await;
    let space = default_space(&token).await;
    let (other, _) = new_account().await;

    let put = |access: MemberAccess| {
        let (space, token, other) = (space.clone(), token.clone(), other.clone());
        async move {
            let mut body = serde_json::to_value(ResolvedMember { did: other, access }).unwrap();
            body["space"] = json!(space);
            post_json(
                &format!("{ZDS}/xrpc/{}.putMember", members_ns().await),
                Some(&token),
                body,
            )
            .await
        }
    };

    let (status, resp) = put(MemberAccess::READ).await;
    assert!(status < 300, "putMember failed ({status}): {resp}");

    // putMember is an upsert: granting write afterwards must replace, not
    // duplicate or reject.
    let (status, resp) = put(MemberAccess {
        read: true,
        write: true,
        read_self: false,
    })
    .await;
    assert!(status < 300, "second putMember failed ({status}): {resp}");

    let (status, body) = get_json(
        &format!(
            "{ZDS}/xrpc/{}.listMembers?space={}",
            members_ns().await,
            enc(&space)
        ),
        Some(&token),
    )
    .await;
    assert_eq!(status, 200, "listMembers failed: {body}");

    let members: Vec<ResolvedMember> =
        serde_json::from_value(body["members"].clone()).expect("members parse as ours");
    let found: Vec<_> = members.iter().filter(|m| m.did == other).collect();
    assert_eq!(found.len(), 1, "putMember must upsert");
    assert!(found[0].access.read && found[0].access.write);
}

// ---------------------------------------------------------------------------
// Commits
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn a_commit_written_by_zds_verifies_and_agrees_with_our_set_hash() {
    let (did, token) = new_account().await;
    let space_uri = default_space(&token).await;

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
    let space_uri = default_space(&token).await;

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
    let space_uri = default_space(&token).await;

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
    let space = default_space(&token).await;

    let records =
        interop_support::records_awaiting_migration(&space, &did, interop_support::Sample::Full);
    let commit = interop_support::replay_as_migration(ZDS, &token, &space, &did, &records).await;

    let disagreements =
        interop_support::cid_disagreements(ZDS, &token, &space, &did, &records).await;
    assert!(disagreements.is_empty(), "{disagreements:#?}");

    let commit = parse_signed_commit(&commit).expect("parses");
    verify_commit(&commit, &space, &did, &author_key(&did).await).expect("authentic");
    assert_eq!(commit.hash, interop_support::migration_expects(&records));
}
