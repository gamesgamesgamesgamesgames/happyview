//! Interop against the Bluesky reference PDS with permissioned-data support.
//!
//! Ignored by default; they need the stack running:
//!
//!   docker compose -f docker-compose.spaces-alpha.yml up -d --wait
//!   cargo test --test spaces_reference_interop -- --ignored --nocapture
//!   docker compose -f docker-compose.spaces-alpha.yml down -v
//!
//! Run these on their own. Like ZDS, the image is amd64-only; see
//! `spaces_zds_interop` for why that matters.
//!
//! This image is the spec-current reference, so it is where HappyView's own
//! wire shapes (split policies, `putMember`, the oplog) are checked. The other
//! hosts predate the 2026-09-10 renames. It serves no capability descriptor, so
//! it is the only real host that exercises the probe tier of detection.

mod interop_support;

use happyview::spaces::commit::{SpaceVerifyingKey, verify_commit};
use happyview::spaces::lthash::{LtHashState, record_element};
use happyview::spaces::native_client::{
    OpAction, collect_repo_ops, latest_op_per_record, parse_signed_commit,
};
use happyview::spaces::pds_support::{self, DetectionTier};
use happyview::spaces::types::{AppAccess, MemberAccess, Policy, ResolvedMember};
use serde_json::{Value, json};
use uuid::Uuid;

const PDS: &str = "http://localhost:2583";
const PLC: &str = "http://localhost:2584";
const NOTE: &str = "com.example.note";

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn status(path: &str) -> u16 {
    client()
        .get(format!("{PDS}/xrpc/{path}"))
        .send()
        .await
        .expect("request to the alpha PDS failed; is it running?")
        .status()
        .as_u16()
}

async fn get_json(method: &str, token: &str, params: &[(&str, String)]) -> (u16, Value) {
    let resp = client()
        .get(format!("{PDS}/xrpc/{method}"))
        .query(params)
        .bearer_auth(token)
        .send()
        .await
        .expect("request to the alpha PDS failed; is it running?");
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

async fn post_json(method: &str, token: Option<&str>, body: Value) -> (u16, Value) {
    let mut req = client().post(format!("{PDS}/xrpc/{method}")).json(&body);
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    let resp = req
        .send()
        .await
        .expect("request to the alpha PDS failed; is it running?");
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

/// A signed-in account on the reference PDS.
struct Account {
    did: String,
    token: String,
}

impl Account {
    /// A fresh account. The reference PDS accepts a password session as well as
    /// OAuth, so no OAuth client is needed to drive it.
    async fn new() -> Self {
        let handle = format!("iv{}.test", &Uuid::new_v4().simple().to_string()[..12]);
        let (status, body) = post_json(
            "com.atproto.server.createAccount",
            None,
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

    async fn create_space(&self, read: &Policy, write: &Policy, app: &AppAccess) -> String {
        let (status, body) = post_json(
            "com.atproto.simplespace.createSpace",
            Some(&self.token),
            json!({
                "type": "com.example.forum",
                "skey": "self",
                // Serialized from HappyView's own types, so acceptance here is a
                // check on how we encode the unions, not just on the host.
                "readPolicy": read,
                "writePolicy": write,
                "appAccess": app,
            }),
        )
        .await;
        assert!(status < 300, "createSpace failed ({status}): {body}");
        body["uri"].as_str().expect("space uri").to_string()
    }

    async fn default_space(&self) -> String {
        self.create_space(&Policy::MemberList, &Policy::MemberList, &AppAccess::Open)
            .await
    }

    async fn write(&self, method: &str, space: &str, mut body: Value) -> Value {
        body["space"] = json!(space);
        body["repo"] = json!(self.did);
        let (status, resp) = post_json(method, Some(&self.token), body).await;
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
        let (status, body) = get_json(
            "com.atproto.space.getLatestCommit",
            &self.token,
            &[("space", space.to_string()), ("repo", self.did.clone())],
        )
        .await;
        assert_eq!(status, 200, "getLatestCommit failed: {body}");
        body["commit"].clone()
    }

    /// Every op since `since`, paged with `page_size` through HappyView's own
    /// collector.
    async fn oplog(
        &self,
        space: &str,
        since: Option<&str>,
        page_size: u32,
    ) -> Vec<happyview::spaces::native_client::RepoOp> {
        collect_repo_ops(space, &self.did, since, |mut params| async move {
            params.push(("limit", page_size.to_string()));
            let (status, body) =
                get_json("com.atproto.space.listRepoOps", &self.token, &params).await;
            assert_eq!(status, 200, "listRepoOps failed: {body}");
            Ok(body)
        })
        .await
        .expect("the oplog collects")
    }
}

/// The account's `#atproto` signing key, from its DID document in the directory.
async fn author_key(did: &str) -> SpaceVerifyingKey {
    let resp = client()
        .get(format!("{PLC}/{did}"))
        .send()
        .await
        .expect("PLC request failed");
    assert_eq!(
        resp.status().as_u16(),
        200,
        "PLC resolution failed for {did}"
    );
    let doc: Value = resp.json().await.unwrap();

    let multibase = doc["verificationMethod"]
        .as_array()
        .expect("verificationMethod")
        .iter()
        .find(|v| v["id"].as_str().unwrap_or_default().ends_with("#atproto"))
        .and_then(|v| v["publicKeyMultibase"].as_str())
        .expect("an #atproto key");
    happyview::spaces::credential::multikey_to_space_key(multibase).expect("the key decodes")
}

// ---------------------------------------------------------------------------
// Capability detection
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn detection_reports_the_reference_pds_as_supported_by_probing() {
    // With no descriptor to read, detection must still recognise this host by
    // probing.
    let got = pds_support::probe(&client(), PDS).await;
    assert!(got.supported, "missing: {:?}", got.missing);
    assert_eq!(got.tier, DetectionTier::Probe);
}

#[tokio::test]
#[ignore]
async fn reference_pds_does_not_serve_the_capability_descriptor() {
    // The reference PDS implements no `community.lexicon.service.describe`, so
    // describe-only detection would report "no spaces" for it. This is why
    // detection has the XRPC probe fallback.
    //
    // The status is 400, not 404, so detection must treat any non-200 as "no
    // answer, fall through" rather than special-casing 404.
    let code = status("community.lexicon.service.describe").await;
    assert_ne!(
        code, 200,
        "the alpha PDS now serves a descriptor; detection can prefer tier 1 for it"
    );
}

#[tokio::test]
#[ignore]
async fn unauthenticated_space_method_proves_the_route_exists() {
    // A spaces method that requires auth answers 401 when present. That is the
    // positive signal the fallback probe reads.
    let code = status("com.atproto.space.listSpaces").await;
    assert_eq!(
        code, 401,
        "expected AuthMissing from a present-but-protected spaces route"
    );
}

#[tokio::test]
#[ignore]
async fn control_probe_distinguishes_a_missing_route() {
    // A method that cannot exist must answer differently from a real one, or the
    // probe says nothing about whether the feature is present.
    let real = status("com.atproto.space.listSpaces").await;
    let control = status("com.atproto.space.thisMethodDoesNotExist").await;

    assert_ne!(
        real, control,
        "control probe matched the real probe; capability detection would be unreliable"
    );
    assert_eq!(control, 400, "missing routes answer InvalidRequest");
}

#[tokio::test]
#[ignore]
async fn probe_must_use_an_auth_gated_method() {
    // A missing route answers 400, and so does a present route that rejects
    // missing query params. `simplespace.getSpace` is therefore indistinguishable
    // from a route that does not exist and cannot serve as a probe. Only an
    // auth-gated method (401) gives an unambiguous signal.
    let get_space = status("com.atproto.simplespace.getSpace").await;
    let control = status("com.atproto.space.thisMethodDoesNotExist").await;

    assert_eq!(
        get_space, control,
        "if these ever differ, getSpace becomes usable as a probe and this test can go"
    );
}

// ---------------------------------------------------------------------------
// Commits
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn a_commit_written_by_the_reference_pds_verifies_and_agrees_with_our_set_hash() {
    let account = Account::new().await;
    let space = account.default_space().await;

    // Creates, an overwrite and a delete, so the hash covers every transition
    // rather than only additions.
    account.put_note(&space, "a", "a").await;
    account.put_note(&space, "b", "b").await;
    let b2 = account.put_note(&space, "b", "b2").await;
    account.delete_note(&space, "a").await;

    let mut expected = LtHashState::new();
    expected.add(&record_element(NOTE, "b", b2["cid"].as_str().unwrap()));

    let commit = parse_signed_commit(&account.latest_commit(&space).await).expect("parses");
    assert_eq!(commit.ver, 1);

    let key = author_key(&account.did).await;
    verify_commit(&commit, &space, &account.did, &key)
        .expect("a commit written by the reference PDS must verify with our own code");
    assert_eq!(
        commit.hash,
        expected.hash(),
        "our set hash disagrees with the reference PDS over the same records"
    );
}

#[tokio::test]
#[ignore]
async fn a_commit_from_another_repo_does_not_verify_as_ours() {
    // Verification binds the commit to its space and author. A commit lifted
    // from one account's repo must not pass as another's, or a host could
    // replay a stale or foreign state during migration.
    let alice = Account::new().await;
    let bob = Account::new().await;
    let space = alice.default_space().await;
    alice.put_note(&space, "a", "a").await;

    let commit = parse_signed_commit(&alice.latest_commit(&space).await).unwrap();
    let alice_key = author_key(&alice.did).await;
    let bob_key = author_key(&bob.did).await;

    assert!(verify_commit(&commit, &space, &bob.did, &alice_key).is_err());
    assert!(verify_commit(&commit, &space, &alice.did, &bob_key).is_err());
}

#[tokio::test]
#[ignore]
async fn the_migration_replay_lands_on_the_reference_pds_with_the_hash_it_expects() {
    let account = Account::new().await;
    let space = account.default_space().await;
    let (did, token) = (account.did.clone(), account.token.clone());

    let records =
        interop_support::records_awaiting_migration(&space, &did, interop_support::Sample::Full);
    let commit = interop_support::replay_as_migration(PDS, &token, &space, &did, &records).await;

    let disagreements =
        interop_support::cid_disagreements(PDS, &token, &space, &did, &records).await;
    assert!(disagreements.is_empty(), "{disagreements:#?}");

    let commit = parse_signed_commit(&commit).expect("parses");
    verify_commit(&commit, &space, &did, &author_key(&did).await).expect("authentic");
    assert_eq!(commit.hash, interop_support::migration_expects(&records));
}

// ---------------------------------------------------------------------------
// The oplog, which native sync reads
// ---------------------------------------------------------------------------

/// Fold the records that survive an op window, the way native sync applies it.
fn fold_latest(ops: &[happyview::spaces::native_client::RepoOp]) -> LtHashState {
    let mut fold = LtHashState::new();
    for op in latest_op_per_record(ops) {
        match op.action {
            OpAction::Delete => {}
            OpAction::Create | OpAction::Update => {
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
    }
    fold
}

#[tokio::test]
#[ignore]
async fn replaying_the_oplog_reproduces_the_hosts_commit() {
    // Native sync's correctness condition: our reading of the ops (actions
    // implied by null CIDs, superseded values withheld) must rebuild the state
    // the host signed.
    let account = Account::new().await;
    let space = account.default_space().await;

    account.put_note(&space, "a", "a").await;
    account.put_note(&space, "b", "b").await;
    account.put_note(&space, "b", "b2").await;
    account.delete_note(&space, "a").await;
    account.put_note(&space, "c", "c").await;

    let ops = account.oplog(&space, None, 100).await;
    let actions: Vec<OpAction> = ops.iter().map(|o| o.action).collect();
    assert_eq!(
        actions,
        [
            OpAction::Create,
            OpAction::Create,
            OpAction::Update,
            OpAction::Delete,
            OpAction::Create
        ],
        "the host sends no action; each must be inferred from its CIDs"
    );

    let commit = parse_signed_commit(&account.latest_commit(&space).await).unwrap();
    assert_eq!(fold_latest(&ops).hash(), commit.hash);
}

#[tokio::test]
#[ignore]
async fn a_repo_longer_than_one_page_is_collected_in_full() {
    // With a page size far below the op count, stopping at the first page would
    // leave the fold short and the sync permanently diverged.
    let account = Account::new().await;
    let space = account.default_space().await;
    for i in 0..7 {
        account.put_note(&space, &format!("r{i}"), "x").await;
    }

    let ops = account.oplog(&space, None, 2).await;
    assert_eq!(ops.len(), 7, "every page must be followed");

    let commit = parse_signed_commit(&account.latest_commit(&space).await).unwrap();
    assert_eq!(fold_latest(&ops).hash(), commit.hash);
}

#[tokio::test]
#[ignore]
async fn syncing_from_a_cursor_returns_only_later_ops() {
    // Native sync resumes from the last rev it applied. The host must treat
    // `since` as exclusive, or every sync re-applies its final op.
    let account = Account::new().await;
    let space = account.default_space().await;
    account.put_note(&space, "a", "a").await;
    let cursor = account
        .oplog(&space, None, 100)
        .await
        .last()
        .unwrap()
        .rev
        .clone();

    account.put_note(&space, "b", "b").await;
    let later = account.oplog(&space, Some(&cursor), 100).await;
    assert_eq!(later.len(), 1);
    assert_eq!(later[0].rkey, "b");
}

// ---------------------------------------------------------------------------
// simplespace: HappyView's types against the reference encoding
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn our_policy_and_app_access_unions_round_trip_through_the_reference_pds() {
    let account = Account::new().await;
    let read = Policy::Public;
    let write = Policy::ManagingApp {
        managing_app: "did:web:app.example#atproto_space".into(),
    };
    let app = AppAccess::AllowList {
        allowed: vec!["https://app.example/client-metadata.json".into()],
    };
    let space = account.create_space(&read, &write, &app).await;

    let (status, body) = get_json(
        "com.atproto.simplespace.getSpace",
        &account.token,
        &[("space", space.clone())],
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
    let account = Account::new().await;
    let space = account.default_space().await;
    let other = Account::new().await;

    let member = ResolvedMember {
        did: other.did.clone(),
        access: MemberAccess::READ,
    };
    let mut body = serde_json::to_value(&member).unwrap();
    body["space"] = json!(space);
    let (status, resp) = post_json(
        "com.atproto.simplespace.putMember",
        Some(&account.token),
        body,
    )
    .await;
    assert!(status < 300, "putMember failed ({status}): {resp}");

    // putMember is an upsert: granting write afterwards must replace, not
    // duplicate or reject.
    let mut body = serde_json::to_value(ResolvedMember {
        did: other.did.clone(),
        access: MemberAccess {
            read: true,
            write: true,
            read_self: false,
        },
    })
    .unwrap();
    body["space"] = json!(space);
    let (status, resp) = post_json(
        "com.atproto.simplespace.putMember",
        Some(&account.token),
        body,
    )
    .await;
    assert!(status < 300, "second putMember failed ({status}): {resp}");

    let (status, body) = get_json(
        "com.atproto.simplespace.listMembers",
        &account.token,
        &[("space", space)],
    )
    .await;
    assert_eq!(status, 200, "listMembers failed: {body}");

    let members: Vec<ResolvedMember> =
        serde_json::from_value(body["members"].clone()).expect("members parse as ours");
    let found: Vec<_> = members.iter().filter(|m| m.did == other.did).collect();
    assert_eq!(found.len(), 1, "putMember must upsert");
    assert!(found[0].access.read && found[0].access.write);
}
