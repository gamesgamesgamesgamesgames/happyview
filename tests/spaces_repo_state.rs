mod common;

use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Request, StatusCode};
use happyview::db::now_rfc3339;
use happyview::spaces::db as spaces_db;
use happyview::spaces::types::*;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

const TYPE_NSID: &str = "com.example.repostate";
const COLLECTION: &str = "com.example.item";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn rand_did(label: &str) -> String {
    format!("did:plc:{label}{}", Uuid::new_v4().simple())
}

fn rand_skey(label: &str) -> String {
    format!("{label}{}", Uuid::new_v4().simple())
}

async fn enable_spaces(app: &TestApp) {
    let (name, value) = app.admin_cookie();
    let req = Request::builder()
        .method("PUT")
        .uri("/admin/settings/feature.spaces_enabled")
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(json!({ "value": "true" }).to_string()))
        .unwrap();
    assert!(
        app.router
            .clone()
            .oneshot(req)
            .await
            .unwrap()
            .status()
            .is_success(),
        "failed to enable spaces"
    );
}

fn cookie_for(app: &TestApp, did: &str) -> (HeaderName, HeaderValue) {
    common::auth::admin_cookie_header(did, &app.state.cookie_key)
}

async fn json_of(resp: axum::http::Response<Body>) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap_or(json!(null))
}

async fn create_space(app: &TestApp, authority: &str, skey: &str) -> (String, String) {
    let now = now_rfc3339();
    let id = Uuid::new_v4().to_string();
    let space = Space {
        id: id.clone(),
        did: authority.to_string(),
        authority_did: authority.to_string(),
        creator_did: authority.to_string(),
        type_nsid: TYPE_NSID.to_string(),
        skey: skey.to_string(),
        display_name: None,
        description: None,
        mint_policy: MintPolicy::MemberList,
        app_access: AppAccess::Open,
        managing_app_did: None,
        config: SpaceConfig::default(),
        revision: None,
        created_at: now.clone(),
        updated_at: now,
    };
    spaces_db::create_space(&app.state.db, app.state.db_backend, &space)
        .await
        .expect("create_space failed");
    let uri = format!("at://{authority}/space/{TYPE_NSID}/{skey}");
    (id, uri)
}

async fn add_member(app: &TestApp, space_id: &str, did: &str, access: SpaceAccess) {
    spaces_db::add_member(
        &app.state.db,
        app.state.db_backend,
        &SpaceMember {
            id: Uuid::new_v4().to_string(),
            space_id: space_id.to_string(),
            did: did.to_string(),
            access,
            is_delegation: false,
            granted_by: None,
            created_at: now_rfc3339(),
        },
    )
    .await
    .expect("add_member failed");
}

async fn setup(label: &str) -> (TestApp, String, String, String) {
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    let authority = rand_did(label);
    let (space_id, space_uri) = create_space(&app, &authority, &rand_skey(label)).await;
    add_member(&app, &space_id, &authority, SpaceAccess::Write).await;
    (app, space_id, space_uri, authority)
}

async fn create_record(app: &TestApp, space_uri: &str, did: &str, body: Value) -> Value {
    let (name, value) = cookie_for(app, did);
    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.createRecord")
        .header("content-type", "application/json")
        .header(name, value)
        .body(Body::from(
            json!({ "space": space_uri, "collection": COLLECTION, "record": body }).to_string(),
        ))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert!(
        resp.status().is_success(),
        "createRecord failed: {}",
        resp.status()
    );
    json_of(resp).await
}

async fn get_repo_state(app: &TestApp, space_uri: &str, did: &str) -> Value {
    let (name, value) = cookie_for(app, did);
    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/xrpc/com.atproto.space.getRepoState?space={}&did={}",
            urlencoding::encode(space_uri),
            urlencoding::encode(did)
        ))
        .header(name, value)
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "getRepoState failed");
    json_of(resp).await
}

async fn list_repo_ops(app: &TestApp, space_uri: &str, did: &str, extra: &str) -> Value {
    let (name, value) = cookie_for(app, did);
    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/xrpc/com.atproto.space.listRepoOps?space={}&did={}{extra}",
            urlencoding::encode(space_uri),
            urlencoding::encode(did)
        ))
        .header(name, value)
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "listRepoOps failed");
    json_of(resp).await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn write_populates_commit() {
    common::require_db!();
    let (app, _space_id, space_uri, did) = setup("commit").await;

    // No writes yet: no commit exists.
    let before = get_repo_state(&app, &space_uri, &did).await;
    assert!(
        before["commit"].is_null(),
        "a repo with no writes must have no commit, got {before}"
    );

    create_record(&app, &space_uri, &did, json!({ "text": "hello" })).await;

    let after = get_repo_state(&app, &space_uri, &did).await;
    let commit = &after["commit"];
    assert!(!commit.is_null(), "commit must exist after a write");
    assert_eq!(commit["ver"], 1);
    assert!(commit["hash"].as_str().is_some(), "hash must be set");
    assert!(commit["ikm"].as_str().is_some(), "ikm must be set");
    assert!(commit["mac"].as_str().is_some(), "mac must be set");
    assert!(commit["rev"].as_str().is_some(), "rev must be set");
    assert!(after["rev"].as_str().is_some());
}

#[tokio::test]
#[serial]
async fn commit_has_no_asymmetric_signature() {
    common::require_db!();
    let (app, _space_id, space_uri, did) = setup("nosig").await;
    create_record(&app, &space_uri, &did, json!({ "text": "hi" })).await;

    let state = get_repo_state(&app, &space_uri, &did).await;
    let commit = state["commit"].as_object().expect("commit must exist");
    assert!(
        !commit.contains_key("sig"),
        "commits must not expose an asymmetric signature, got {commit:?}"
    );
}

#[tokio::test]
#[serial]
async fn lthash_remove_undoes_add() {
    common::require_db!();
    let (app, _space_id, space_uri, did) = setup("lthash").await;

    async fn delete(app: &TestApp, space_uri: &str, did: &str, rkey: &str) {
        let (name, value) = cookie_for(app, did);
        let req = Request::builder()
            .method("POST")
            .uri("/xrpc/com.atproto.space.deleteRecord")
            .header("content-type", "application/json")
            .header(name, value)
            .body(Body::from(
                json!({ "space": space_uri, "collection": COLLECTION, "rkey": rkey }).to_string(),
            ))
            .unwrap();
        let resp = app.router.clone().oneshot(req).await.unwrap();
        assert!(
            resp.status().is_success(),
            "deleteRecord failed: {}",
            resp.status()
        );
    }

    fn rkey_of(created: &Value) -> String {
        created["uri"]
            .as_str()
            .expect("uri")
            .rsplit('/')
            .next()
            .expect("rkey")
            .to_string()
    }

    // Record A stays put; record B is the one we add then remove.
    create_record(&app, &space_uri, &did, json!({ "text": "keeper" })).await;
    let hash_a = get_repo_state(&app, &space_uri, &did).await["commit"]["hash"]
        .as_str()
        .expect("hash")
        .to_string();

    let b = create_record(&app, &space_uri, &did, json!({ "text": "transient" })).await;
    let hash_ab = get_repo_state(&app, &space_uri, &did).await["commit"]["hash"]
        .as_str()
        .expect("hash")
        .to_string();
    assert_ne!(
        hash_a, hash_ab,
        "adding a record must change the multiset hash"
    );

    delete(&app, &space_uri, &did, &rkey_of(&b)).await;
    let hash_after_delete = get_repo_state(&app, &space_uri, &did).await["commit"]["hash"]
        .as_str()
        .expect("hash")
        .to_string();

    assert_eq!(
        hash_a, hash_after_delete,
        "removing B must return the hash to its exact pre-B value"
    );
}

#[tokio::test]
#[serial]
async fn list_repo_ops_returns_ops_and_bundled_commit() {
    common::require_db!();
    let (app, _space_id, space_uri, did) = setup("ops").await;
    create_record(&app, &space_uri, &did, json!({ "text": "one" })).await;

    let body = list_repo_ops(&app, &space_uri, &did, "").await;
    let ops = body["ops"].as_array().expect("ops array");
    assert_eq!(ops.len(), 1, "expected exactly one op, got {body}");
    assert_eq!(ops[0]["action"], "create");
    assert_eq!(ops[0]["collection"], COLLECTION);
    assert_eq!(ops[0]["idx"], 0);
    assert!(ops[0]["cid"].as_str().is_some());
    assert!(
        ops[0]["prev"].is_null(),
        "a create has nothing to link back to"
    );

    assert!(
        body["cursor"].is_null(),
        "page is not full, so there is no next page"
    );
    assert!(
        !body["commit"].is_null(),
        "listRepoOps must bundle the current MAC'd hash"
    );
    assert_eq!(body["commit"]["hash"], {
        let s = get_repo_state(&app, &space_uri, &did).await;
        s["commit"]["hash"].clone()
    });
}

#[tokio::test]
#[serial]
async fn get_repo_returns_car_after_write() {
    common::require_db!();
    let (app, _space_id, space_uri, did) = setup("car").await;
    create_record(&app, &space_uri, &did, json!({ "text": "in a car" })).await;

    let (name, value) = cookie_for(&app, &did);
    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/xrpc/com.atproto.space.getRepo?space={}&did={}",
            urlencoding::encode(&space_uri),
            urlencoding::encode(&did)
        ))
        .header(name, value)
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "getRepo must succeed");
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/vnd.ipld.car"
    );
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(bytes.len() > 10, "CAR body looks empty");
}

#[tokio::test]
#[serial]
async fn apply_writes_shares_one_rev_with_ordered_idx() {
    common::require_db!();
    let (app, _space_id, space_uri, did) = setup("batch").await;

    let (name, value) = cookie_for(&app, &did);
    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.applyWrites")
        .header("content-type", "application/json")
        .header(name, value)
        .body(Body::from(
            json!({
                "space": space_uri,
                "writes": [
                    { "action": "create", "collection": COLLECTION, "value": { "n": 1 } },
                    { "action": "create", "collection": COLLECTION, "value": { "n": 2 } },
                    { "action": "create", "collection": COLLECTION, "value": { "n": 3 } },
                ]
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert!(
        resp.status().is_success(),
        "applyWrites failed: {}",
        resp.status()
    );

    let body = list_repo_ops(&app, &space_uri, &did, "").await;
    let ops = body["ops"].as_array().expect("ops");
    assert_eq!(ops.len(), 3);

    let rev = ops[0]["rev"].as_str().expect("rev");
    for (i, op) in ops.iter().enumerate() {
        assert_eq!(op["rev"].as_str().unwrap(), rev, "batch shares one rev");
        assert_eq!(op["idx"], i as i64, "ops are ordered by idx within the rev");
    }

    // One commit for the whole batch, stamped with the batch rev.
    let state = get_repo_state(&app, &space_uri, &did).await;
    assert_eq!(state["commit"]["rev"].as_str().unwrap(), rev);
}

#[tokio::test]
#[serial]
async fn list_repo_ops_paginates_within_a_batch_rev() {
    common::require_db!();
    let (app, _space_id, space_uri, did) = setup("page").await;

    let writes: Vec<Value> = (0..5)
        .map(|n| json!({ "action": "create", "collection": COLLECTION, "value": { "n": n } }))
        .collect();
    let (name, value) = cookie_for(&app, &did);
    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.applyWrites")
        .header("content-type", "application/json")
        .header(name, value)
        .body(Body::from(
            json!({ "space": space_uri, "writes": writes }).to_string(),
        ))
        .unwrap();
    assert!(
        app.router
            .clone()
            .oneshot(req)
            .await
            .unwrap()
            .status()
            .is_success()
    );

    let mut seen = Vec::new();
    let mut extra = "&limit=2".to_string();
    loop {
        let body = list_repo_ops(&app, &space_uri, &did, &extra).await;
        let ops = body["ops"].as_array().unwrap().clone();
        assert!(ops.len() <= 2, "page must respect the limit");
        for op in &ops {
            seen.push(op["idx"].as_i64().unwrap());
        }
        match body["cursor"].as_str() {
            Some(c) => extra = format!("&limit=2&cursor={}", urlencoding::encode(c)),
            None => break,
        }
    }

    assert_eq!(
        seen,
        vec![0, 1, 2, 3, 4],
        "every op in the batch must be visited exactly once, in order"
    );
}
