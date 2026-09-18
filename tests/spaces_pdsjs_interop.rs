//! Interop against pds.js, a single-account PDS that serves atproto spaces.
//!
//! Ignored by default; they need the stack running:
//!
//!   docker compose -f docker-compose.pdsjs.yml up -d --wait
//!   cargo test --test spaces_pdsjs_interop -- --ignored --nocapture
//!   docker compose -f docker-compose.pdsjs.yml down -v
//!
//! pds.js builds natively, with no amd64 emulation, so these can share a run with
//! the rest of the suite.
//!
//! It serves `community.lexicon.service.describe` and gates spaces behind
//! `PDS_ENABLE_SPACES`, so like ZDS it covers both answers of detection. It
//! answers to both spellings of the renamed methods.
//!
//! pds.js hosts one account, registered by the first test to need it, so the
//! tests share it and isolate themselves by giving every space its own key.

mod interop_support;

use interop_support::Sample;

use happyview::spaces::commit::{SpaceVerifyingKey, verify_commit};
use happyview::spaces::lthash::{LtHashState, record_element};
use happyview::spaces::native_client::{
    OpAction, RepoOp, collect_repo_ops, latest_op_per_record, parse_signed_commit,
};
use happyview::spaces::pds_support::{self, DetectionTier};
use happyview::spaces::types::Policy;
use serde_json::{Value, json};
use uuid::Uuid;

const PDSJS: &str = "http://localhost:2588";
const PDSJS_NO_SPACES: &str = "http://localhost:2589";
const PLC: &str = "http://localhost:2585";
const NOTE: &str = "com.example.note";

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn status_of(base: &str, method: &str) -> u16 {
    client()
        .get(format!("{base}/xrpc/{method}"))
        .send()
        .await
        .expect("request to pds.js failed; is it running?")
        .status()
        .as_u16()
}

/// Matches `PDS_PASSWORD` in docker-compose.pdsjs.yml.
const PASSWORD: &str = "password123";

/// The PDS's one account, signed in.
struct Account {
    did: String,
    token: String,
}

impl Account {
    async fn sign_in() -> Self {
        let did = interop_support::ensure_pdsjs_account(PDSJS, PLC, PASSWORD).await;
        let (status, body) = self::post(
            None,
            "com.atproto.server.createSession",
            json!({ "identifier": did, "password": PASSWORD }),
        )
        .await;
        assert_eq!(status, 200, "createSession failed: {body}");
        Self {
            did,
            token: body["accessJwt"].as_str().expect("accessJwt").to_string(),
        }
    }

    async fn get(&self, method: &str, params: &[(&str, String)]) -> (u16, Value) {
        let resp = client()
            .get(format!("{PDSJS}/xrpc/{method}"))
            .query(params)
            .bearer_auth(&self.token)
            .send()
            .await
            .expect("request to pds.js failed");
        let status = resp.status().as_u16();
        (status, resp.json().await.unwrap_or(Value::Null))
    }

    async fn post(&self, method: &str, body: Value) -> (u16, Value) {
        self::post(Some(&self.token), method, body).await
    }

    /// A space of its own, so tests sharing the account cannot see each other.
    async fn new_space(&self, extra: Value) -> String {
        let mut body = json!({
            "type": "com.example.forum",
            "skey": format!("t{}", &Uuid::new_v4().simple().to_string()[..12]),
        });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().cloned().unwrap_or_default());
        let (status, resp) = self.post("com.atproto.simplespace.createSpace", body).await;
        assert!(status < 300, "createSpace failed ({status}): {resp}");
        resp["uri"].as_str().expect("space uri").to_string()
    }

    async fn write(&self, method: &str, space: &str, mut body: Value) -> Value {
        body["space"] = json!(space);
        body["repo"] = json!(self.did);
        let (status, resp) = self.post(method, body).await;
        assert!(status < 300, "{method} failed ({status}): {resp}");
        resp
    }

    async fn put_note(&self, space: &str, rkey: &str, text: &str) -> Value {
        self.write(
            "com.atproto.space.putRecord",
            space,
            json!({ "collection": NOTE, "rkey": rkey, "record": { "$type": NOTE, "text": text } }),
        )
        .await
    }

    async fn delete_note(&self, space: &str, rkey: &str) {
        self.write(
            "com.atproto.space.deleteRecord",
            space,
            json!({ "collection": NOTE, "rkey": rkey }),
        )
        .await;
    }

    async fn latest_commit(&self, space: &str) -> Value {
        let (status, body) = self
            .get(
                "com.atproto.space.getLatestCommit",
                &[("space", space.to_string()), ("repo", self.did.clone())],
            )
            .await;
        assert_eq!(status, 200, "getLatestCommit failed: {body}");
        body["commit"].clone()
    }

    async fn oplog(&self, space: &str, since: Option<&str>, page_size: u32) -> Vec<RepoOp> {
        collect_repo_ops(space, &self.did, since, |mut params| async move {
            params.push(("limit", page_size.to_string()));
            let (status, body) = self.get("com.atproto.space.listRepoOps", &params).await;
            assert_eq!(status, 200, "listRepoOps failed: {body}");
            Ok(body)
        })
        .await
        .expect("the oplog collects")
    }
}

async fn post(token: Option<&str>, method: &str, body: Value) -> (u16, Value) {
    let mut req = client().post(format!("{PDSJS}/xrpc/{method}")).json(&body);
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    let resp = req.send().await.expect("request to pds.js failed");
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

/// The account's `#atproto` signing key, from its DID document in the directory.
async fn author_key(did: &str) -> SpaceVerifyingKey {
    let doc: Value = client()
        .get(format!("{PLC}/{did}"))
        .send()
        .await
        .expect("PLC request failed")
        .json()
        .await
        .expect("PLC json");
    let multibase = doc["verificationMethod"]
        .as_array()
        .expect("verificationMethod")
        .iter()
        .find(|v| v["id"].as_str().unwrap_or_default().ends_with("#atproto"))
        .and_then(|v| v["publicKeyMultibase"].as_str())
        .expect("an #atproto key");
    happyview::spaces::credential::multikey_to_space_key(multibase).expect("the key decodes")
}

fn fold_latest(ops: &[RepoOp]) -> LtHashState {
    let mut fold = LtHashState::new();
    for op in latest_op_per_record(ops) {
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
    fold
}

// ---------------------------------------------------------------------------
// Capability detection
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn detection_reports_pdsjs_with_spaces_as_supported() {
    let got = pds_support::probe(&client(), PDSJS).await;
    assert!(got.supported, "missing: {:?}", got.missing);
    assert_eq!(got.tier, DetectionTier::Descriptor);
}

#[tokio::test]
#[ignore]
async fn detection_reports_pdsjs_without_spaces_as_unsupported() {
    let got = pds_support::probe(&client(), PDSJS_NO_SPACES).await;
    assert!(!got.supported);
    assert_eq!(got.tier, DetectionTier::Descriptor);
    assert!(!got.missing.is_empty());
}

#[tokio::test]
#[ignore]
async fn the_probe_tier_would_reach_the_same_answers_for_pdsjs() {
    // pds.js always serves a descriptor, so the probe does not normally run
    // against it. It still has to agree if the descriptor fails to parse.
    // pds.js answers an unknown method 501, not the reference PDS's 400, so this
    // also checks the probe compares against a control rather than a constant.
    let control = status_of(PDSJS, "com.atproto.space.thisMethodDoesNotExist").await;
    assert_eq!(control, 501);

    assert_eq!(status_of(PDSJS, "com.atproto.space.listSpaces").await, 401);
    assert_eq!(
        status_of(PDSJS_NO_SPACES, "com.atproto.space.listSpaces").await,
        status_of(PDSJS_NO_SPACES, "com.atproto.space.thisMethodDoesNotExist").await,
        "without spaces the probe method must look like a missing route"
    );
}

// ---------------------------------------------------------------------------
// Commits, the oplog, and the migration handoff
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn a_commit_written_by_pdsjs_verifies_and_agrees_with_our_set_hash() {
    let account = Account::sign_in().await;
    let space = account.new_space(json!({})).await;

    account.put_note(&space, "a", "a").await;
    account.put_note(&space, "b", "b").await;
    let b2 = account.put_note(&space, "b", "b2").await;
    account.delete_note(&space, "a").await;

    let mut expected = LtHashState::new();
    expected.add(&record_element(NOTE, "b", b2["cid"].as_str().unwrap()));

    let commit = parse_signed_commit(&account.latest_commit(&space).await).expect("parses");
    let key = author_key(&account.did).await;
    verify_commit(&commit, &space, &account.did, &key)
        .expect("a commit written by pds.js must verify with our own code");
    assert_eq!(commit.hash, expected.hash());
}

#[tokio::test]
#[ignore]
async fn replaying_the_pdsjs_oplog_across_pages_reproduces_its_commit() {
    let account = Account::sign_in().await;
    let space = account.new_space(json!({})).await;

    account.put_note(&space, "a", "a").await;
    account.put_note(&space, "b", "b").await;
    account.put_note(&space, "b", "b2").await;
    account.delete_note(&space, "a").await;
    account.put_note(&space, "c", "c").await;

    let ops = account.oplog(&space, None, 2).await;
    let actions: Vec<OpAction> = ops.iter().map(|o| o.action).collect();
    assert_eq!(
        actions,
        [
            OpAction::Create,
            OpAction::Create,
            OpAction::Update,
            OpAction::Delete,
            OpAction::Create
        ]
    );

    let commit = parse_signed_commit(&account.latest_commit(&space).await).unwrap();
    assert_eq!(fold_latest(&ops).hash(), commit.hash);

    // Resuming from a cursor sees only what came after it.
    let cursor = ops.last().unwrap().rev.clone();
    account.put_note(&space, "d", "d").await;
    let later = account.oplog(&space, Some(&cursor), 2).await;
    assert_eq!(later.len(), 1);
    assert_eq!(later[0].rkey, "d");
}

#[tokio::test]
#[ignore]
async fn the_migration_replay_lands_on_pdsjs_with_the_hash_it_expects() {
    // Plain JSON only. pds.js encodes `$link`, `$bytes` and large integers
    // differently from HappyView and the reference PDS (pinned below), so a repo
    // holding those cannot migrate to it.
    let account = Account::sign_in().await;
    let space = account.new_space(json!({})).await;

    let records =
        interop_support::records_awaiting_migration(&space, &account.did, Sample::PlainJson);
    let commit =
        interop_support::replay_as_migration(PDSJS, &account.token, &space, &account.did, &records)
            .await;

    let disagreements =
        interop_support::cid_disagreements(PDSJS, &account.token, &space, &account.did, &records)
            .await;
    assert!(disagreements.is_empty(), "{disagreements:#?}");

    let commit = parse_signed_commit(&commit).expect("parses");
    verify_commit(
        &commit,
        &space,
        &account.did,
        &author_key(&account.did).await,
    )
    .expect("authentic");
    assert_eq!(commit.hash, interop_support::migration_expects(&records));
}

#[tokio::test]
#[ignore]
async fn a_repo_with_typed_values_is_refused_rather_than_migrated_wrongly_to_pdsjs() {
    // pds.js accepts the replay, but its commit cannot match. The hash check
    // refuses the handoff, so the repo stays on HappyView.
    let account = Account::sign_in().await;
    let space = account.new_space(json!({})).await;

    let records = interop_support::records_awaiting_migration(&space, &account.did, Sample::Full);
    let commit =
        interop_support::replay_as_migration(PDSJS, &account.token, &space, &account.did, &records)
            .await;
    let commit = parse_signed_commit(&commit).expect("parses");
    assert_ne!(
        commit.hash,
        interop_support::migration_expects(&records),
        "pds.js now agrees on typed values; fold this into the migration test above"
    );
}

// ---------------------------------------------------------------------------
// Where pds.js diverges from the current spec
//
// None of these are HappyView bugs. They are pinned so a change in pds.js is
// noticed, and so nobody assumes an authority hosted on pds.js behaves like the
// reference. The simplespace ones do not affect HappyView, which sends a PDS no
// simplespace calls.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn pdsjs_serves_both_member_method_spellings() {
    // pds.js serves both spellings of the member method, so the allowance in
    // REQUIRED_METHODS is not what detection matches it on. atproto-pds and ZDS
    // serve the canonical name alone.
    let body: Value = client()
        .get(format!("{PDSJS}/xrpc/community.lexicon.service.describe"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let methods: Vec<&str> = body["methods"]
        .as_array()
        .expect("methods")
        .iter()
        .filter_map(|m| m["value"].as_str())
        .collect();
    assert!(methods.contains(&"com.atproto.simplespace.putMember"));
    assert!(methods.contains(&"com.atproto.simplespace.addMember"));
}

#[tokio::test]
#[ignore]
async fn pdsjs_answers_the_single_policy_field_from_the_read_policy_alone() {
    // pds.js stores readPolicy and writePolicy as asked, then answers with the
    // single `policy` field as well, taken from readPolicy and ignoring
    // writePolicy. A space that is public to read and member-list to write
    // comes back `policy: publicPolicy`, so a caller reading that field alone
    // reads the space as more open than it is. A `config` object carries the
    // same policy again as a bare string.
    //
    // The reference and atproto-pds answer with the split alone.
    let account = Account::sign_in().await;

    let space = account
        .new_space(json!({ "readPolicy": Policy::Public, "writePolicy": Policy::Public }))
        .await;
    let (status, body) = account
        .get("com.atproto.simplespace.getSpace", &[("space", space)])
        .await;
    assert_eq!(status, 200, "getSpace failed: {body}");
    let read: Policy = serde_json::from_value(body["readPolicy"].clone()).expect("readPolicy");
    let write: Policy = serde_json::from_value(body["writePolicy"].clone()).expect("writePolicy");
    assert_eq!(read, Policy::Public, "{body}");
    assert_eq!(write, Policy::Public, "{body}");
    let mirrored: Policy = serde_json::from_value(body["policy"].clone()).expect("policy");
    assert_eq!(mirrored, Policy::Public, "{body}");
    assert_eq!(body["config"]["policy"], json!("public"), "{body}");

    let space = account
        .new_space(json!({ "readPolicy": Policy::Public, "writePolicy": Policy::MemberList }))
        .await;
    let (status, body) = account
        .get("com.atproto.simplespace.getSpace", &[("space", space)])
        .await;
    assert_eq!(status, 200, "getSpace failed: {body}");
    let write: Policy = serde_json::from_value(body["writePolicy"].clone()).expect("writePolicy");
    assert_eq!(write, Policy::MemberList, "{body}");
    let mirrored: Policy = serde_json::from_value(body["policy"].clone()).expect("policy");
    assert_eq!(
        mirrored,
        Policy::Public,
        "the single field no longer follows readPolicy: {body}"
    );
}

#[tokio::test]
#[ignore]
async fn pdsjs_encodes_links_bytes_and_large_integers_differently_from_the_reference() {
    // Each of these gets one CID from HappyView and the reference PDS (the
    // migration test in spaces_reference_interop covers them) and another from
    // pds.js. A blob ref is a `$link`, so this covers any record with a blob.
    //
    // Causes, in pds.js's space write path: `{$link}` and `{$bytes}` are encoded
    // as plain maps rather than a CID and a byte string, and the DAG-CBOR encoder
    // has no length form past 32 bits.
    let account = Account::sign_in().await;
    let space = account.new_space(json!({})).await;

    let cases = [
        ("big", json!(9_007_199_254_740_991_i64)),
        (
            "link",
            json!({ "$link": "bafyreihn37cccblw3mmwmmm6jewugcohecjf5kpzuciirdvisasq7k3siu" }),
        ),
        ("bytes", json!({ "$bytes": "AAECAwQFBgc" })),
    ];
    for (rkey, value) in cases {
        let record = json!({ "$type": NOTE, "v": value });
        let ours = happyview::cid_verify::compute_record_cid(&record)
            .unwrap()
            .to_string();
        let theirs = account
            .write(
                "com.atproto.space.putRecord",
                &space,
                json!({ "collection": NOTE, "rkey": rkey, "record": record }),
            )
            .await["cid"]
            .as_str()
            .unwrap()
            .to_string();
        assert_ne!(
            ours, theirs,
            "pds.js now encodes {rkey} like the reference; drop it from this test"
        );
    }
}

#[tokio::test]
#[ignore]
async fn pdsjs_drops_ops_when_a_page_ends_inside_a_batch() {
    // pds.js pages by `rev > since` and ignores `cursor`. Ops written together
    // share a rev, so a page boundary inside a batch loses the rest of it: no
    // client can recover them by paging. The reference and ZDS page by
    // (rev, index) and do not.
    //
    // HappyView cannot fix this from the client side, but it does not index a
    // short repo: the fold disagrees with the commit, the sync reports
    // `Diverged`, and the cursor stays put. Syncing against pds.js is only
    // reliable while no batch outgrows a page, which holds at the default limit
    // of 100 with HappyView's batches of 50.
    let account = Account::sign_in().await;
    let space = account.new_space(json!({})).await;

    let writes: Vec<Value> = ["a", "b", "c"]
        .iter()
        .map(|rkey| {
            json!({
                "$type": "com.atproto.space.applyWrites#create",
                "collection": NOTE,
                "rkey": rkey,
                "value": { "$type": NOTE, "text": rkey },
            })
        })
        .collect();
    account
        .write(
            "com.atproto.space.applyWrites",
            &space,
            json!({ "writes": writes }),
        )
        .await;

    let ops = account.oplog(&space, None, 2).await;
    assert_eq!(
        ops.len(),
        2,
        "pds.js now pages within a batch; this divergence is fixed and the test can go"
    );

    let commit = parse_signed_commit(&account.latest_commit(&space).await).unwrap();
    assert_ne!(
        fold_latest(&ops).hash(),
        commit.hash,
        "the loss must be detectable"
    );
}
