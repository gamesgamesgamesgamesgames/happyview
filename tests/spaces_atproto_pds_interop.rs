//! Interop against atproto-pds, the Rust PDS from atproto-crates.
//!
//! Ignored by default; they need the stack running:
//!
//!   docker compose -f docker-compose.atproto-pds.yml up -d --wait
//!   cargo test --test spaces_atproto_pds_interop -- --ignored --nocapture
//!   docker compose -f docker-compose.atproto-pds.yml down -v
//!
//! Runs natively, with no amd64 emulation, so these can share a run with the
//! rest of the suite. Multi-account, so every test makes its own account.

mod interop_support;

use happyview::spaces::commit::{SpaceVerifyingKey, verify_commit};
use happyview::spaces::lthash::{LtHashState, record_element};
use happyview::spaces::native_client::{
    OpAction, RepoOp, collect_repo_ops, latest_op_per_record, parse_signed_commit,
};
use happyview::spaces::pds_support::{self, DetectionTier};
use happyview::spaces::types::Policy;
use interop_support::Sample;
use serde_json::{Value, json};
use uuid::Uuid;

const PDS: &str = "http://localhost:2590";
const PLC: &str = "http://localhost:2591";
const NOTE: &str = "com.example.note";

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn status_of(method: &str) -> u16 {
    client()
        .get(format!("{PDS}/xrpc/{method}"))
        .send()
        .await
        .expect("request to atproto-pds failed; is it running?")
        .status()
        .as_u16()
}

async fn post(token: Option<&str>, method: &str, body: Value) -> (u16, Value) {
    let mut req = client().post(format!("{PDS}/xrpc/{method}")).json(&body);
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    let resp = req
        .send()
        .await
        .expect("request to atproto-pds failed; is it running?");
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

struct Account {
    did: String,
    token: String,
}

impl Account {
    async fn new() -> Self {
        // atproto-pds names each account's signing key `sk-<unix millis>`, so
        // two accounts created in the same millisecond collide on a unique
        // index and one fails with a 500. Parallel tests hit that reliably, so
        // accounts are created one at a time, a couple of milliseconds apart.
        static CREATING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let _one_at_a_time = CREATING.lock().await;
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;

        let handle = format!("iv{}.test", &Uuid::new_v4().simple().to_string()[..12]);
        let (status, body) = post(
            None,
            "com.atproto.server.createAccount",
            json!({
                "handle": handle,
                "email": format!("{handle}@test.com"),
                "password": "password123",
            }),
        )
        .await;
        assert!(status < 300, "createAccount failed ({status}): {body}");
        Self {
            did: body["did"].as_str().expect("did").to_string(),
            token: body["accessJwt"].as_str().expect("accessJwt").to_string(),
        }
    }

    async fn get(&self, method: &str, params: &[(&str, String)]) -> (u16, Value) {
        let resp = client()
            .get(format!("{PDS}/xrpc/{method}"))
            .query(params)
            .bearer_auth(&self.token)
            .send()
            .await
            .expect("request to atproto-pds failed");
        let status = resp.status().as_u16();
        (status, resp.json().await.unwrap_or(Value::Null))
    }

    async fn post(&self, method: &str, body: Value) -> (u16, Value) {
        post(Some(&self.token), method, body).await
    }

    /// A space in the pre-split shape this server takes: one `policy`.
    async fn new_space(&self) -> String {
        let (status, body) = self
            .post(
                "com.atproto.simplespace.createSpace",
                json!({
                    "type": "com.example.forum",
                    "skey": "self",
                    "policy": { "$type": "com.atproto.simplespace.defs#memberListPolicy" },
                    "appAccess": { "$type": "com.atproto.simplespace.defs#open" },
                }),
            )
            .await;
        assert!(status < 300, "createSpace failed ({status}): {body}");
        body["uri"].as_str().expect("space uri").to_string()
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
async fn detection_reports_atproto_pds_as_supported() {
    let got = pds_support::probe(&client(), PDS).await;
    assert!(got.supported, "missing: {:?}", got.missing);
    assert_eq!(got.tier, DetectionTier::Descriptor);
}

#[tokio::test]
#[ignore]
async fn the_probe_tier_would_reach_the_same_answer() {
    // The descriptor answers first; the probe must still agree if the
    // descriptor fails to parse.
    let real = status_of("com.atproto.space.listSpaces").await;
    let control = status_of("com.atproto.space.thisMethodDoesNotExist").await;
    assert_eq!(real, 401);
    assert_ne!(real, control);
}

// ---------------------------------------------------------------------------
// Commits, the oplog, and the migration handoff
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn a_commit_written_by_atproto_pds_verifies_and_agrees_with_our_set_hash() {
    let account = Account::new().await;
    let space = account.new_space().await;

    account.put_note(&space, "a", "a").await;
    account.put_note(&space, "b", "b").await;
    let b2 = account.put_note(&space, "b", "b2").await;
    account.delete_note(&space, "a").await;

    let mut expected = LtHashState::new();
    expected.add(&record_element(NOTE, "b", b2["cid"].as_str().unwrap()));

    let commit = parse_signed_commit(&account.latest_commit(&space).await).expect("parses");
    verify_commit(
        &commit,
        &space,
        &account.did,
        &author_key(&account.did).await,
    )
    .expect("a commit written by atproto-pds must verify with our own code");
    assert_eq!(commit.hash, expected.hash());
}

#[tokio::test]
#[ignore]
async fn a_commit_from_another_repo_does_not_verify_as_ours() {
    let alice = Account::new().await;
    let bob = Account::new().await;
    let space = alice.new_space().await;
    alice.put_note(&space, "a", "a").await;

    let commit = parse_signed_commit(&alice.latest_commit(&space).await).unwrap();
    assert!(verify_commit(&commit, &space, &bob.did, &author_key(&alice.did).await).is_err());
    assert!(verify_commit(&commit, &space, &alice.did, &author_key(&bob.did).await).is_err());
}

#[tokio::test]
#[ignore]
async fn replaying_the_atproto_pds_oplog_across_pages_reproduces_its_commit() {
    let account = Account::new().await;
    let space = account.new_space().await;

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

    let cursor = ops.last().unwrap().rev.clone();
    account.put_note(&space, "d", "d").await;
    let later = account.oplog(&space, Some(&cursor), 2).await;
    assert_eq!(later.len(), 1);
    assert_eq!(later[0].rkey, "d");
}

#[tokio::test]
#[ignore]
async fn a_page_ending_inside_a_batch_loses_nothing() {
    // The case pds.js gets wrong; see
    // `pdsjs_drops_ops_when_a_page_ends_inside_a_batch`.
    let account = Account::new().await;
    let space = account.new_space().await;

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
    assert_eq!(ops.len(), 3);
    let commit = parse_signed_commit(&account.latest_commit(&space).await).unwrap();
    assert_eq!(fold_latest(&ops).hash(), commit.hash);
}

#[tokio::test]
#[ignore]
async fn the_migration_replay_lands_on_atproto_pds_with_the_hash_it_expects() {
    let account = Account::new().await;
    let space = account.new_space().await;

    let records = interop_support::records_awaiting_migration(&space, &account.did, Sample::Full);
    let commit =
        interop_support::replay_as_migration(PDS, &account.token, &space, &account.did, &records)
            .await;

    let disagreements =
        interop_support::cid_disagreements(PDS, &account.token, &space, &account.did, &records)
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

// ---------------------------------------------------------------------------
// Where atproto-pds diverges from the current spec
//
// Not HappyView bugs: HappyView sends a PDS no simplespace calls. Pinned so a
// change in atproto-pds is noticed.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn atproto_pds_still_uses_the_pre_split_member_methods() {
    let body: Value = client()
        .get(format!("{PDS}/xrpc/community.lexicon.service.describe"))
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
    assert!(methods.contains(&"com.atproto.simplespace.addMember"));
    assert!(!methods.contains(&"com.atproto.simplespace.putMember"));
}

#[tokio::test]
#[ignore]
async fn atproto_pds_does_not_take_split_read_and_write_policies() {
    // This server refuses a request that carries only the split fields, where
    // pds.js accepts them and stores member-list.
    let account = Account::new().await;
    let (status, body) = account
        .post(
            "com.atproto.simplespace.createSpace",
            json!({
                "type": "com.example.forum",
                "skey": "self",
                "readPolicy": Policy::Public,
                "writePolicy": Policy::Public,
                "appAccess": { "$type": "com.atproto.simplespace.defs#open" },
            }),
        )
        .await;
    assert_eq!(
        status, 400,
        "atproto-pds now accepts the split; check it stores it and drop this: {body}"
    );
    assert!(
        body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("policy"),
        "rejected, but not for lacking a policy: {body}"
    );
}
