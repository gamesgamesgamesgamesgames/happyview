//! HTTP-level e2e coverage for the space record-write endpoints:
//! `com.atproto.space.createRecord` / `putRecord` / `deleteRecord`.
//!
//! Mirrors the harness established by `spaces_list_repos_auth.rs` /
//! `spaces_notify_auth.rs` / `spaces_credential_mint.rs`: a `TestApp` router,
//! `feature.spaces_enabled` flipped on via the admin API, membership seeded
//! directly through `happyview::spaces::db`, and callers authenticated with a
//! signed session cookie for an arbitrary DID (`common::auth::admin_cookie_header`
//! — despite the name it just signs a `happyview_session` cookie for whatever
//! DID you pass, satisfying `require_auth`/`require_auth_or_credential`; the
//! DID need not exist in `happyview_users`).

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

/// Create a space authored by `authority` with the given `skey`. Returns the
/// space's DB id and its `at://` URI.
async fn create_space(app: &TestApp, authority: &str, skey: &str) -> (String, String) {
    let now = now_rfc3339();
    let id = Uuid::new_v4().to_string();
    let type_nsid = "com.example.records";
    let space = Space {
        id: id.clone(),
        did: authority.to_string(),
        authority_did: authority.to_string(),
        creator_did: authority.to_string(),
        type_nsid: type_nsid.to_string(),
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
    let uri = format!("at://{authority}/space/{type_nsid}/{skey}");
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

fn create_record_req(
    space_uri: &str,
    collection: &str,
    record: &Value,
    cookie: Option<(HeaderName, HeaderValue)>,
) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.createRecord")
        .header("content-type", "application/json");
    if let Some((name, value)) = cookie {
        b = b.header(name, value);
    }
    b.body(Body::from(
        json!({ "space": space_uri, "collection": collection, "record": record }).to_string(),
    ))
    .unwrap()
}

#[allow(clippy::too_many_arguments)]
fn put_record_req(
    space_uri: &str,
    collection: &str,
    rkey: &str,
    record: &Value,
    swap_record: Option<&str>,
    cookie: Option<(HeaderName, HeaderValue)>,
) -> Request<Body> {
    let mut body = json!({
        "space": space_uri,
        "collection": collection,
        "rkey": rkey,
        "record": record,
    });
    if let Some(swap) = swap_record {
        body["swapRecord"] = json!(swap);
    }
    let mut b = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.putRecord")
        .header("content-type", "application/json");
    if let Some((name, value)) = cookie {
        b = b.header(name, value);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

fn delete_record_req(
    space_uri: &str,
    collection: &str,
    rkey: &str,
    swap_record: Option<&str>,
    cookie: Option<(HeaderName, HeaderValue)>,
) -> Request<Body> {
    let mut body = json!({
        "space": space_uri,
        "collection": collection,
        "rkey": rkey,
    });
    if let Some(swap) = swap_record {
        body["swapRecord"] = json!(swap);
    }
    let mut b = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.deleteRecord")
        .header("content-type", "application/json");
    if let Some((name, value)) = cookie {
        b = b.header(name, value);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

fn get_record_req(
    space_uri: &str,
    collection: &str,
    rkey: &str,
    cookie: Option<(HeaderName, HeaderValue)>,
) -> Request<Body> {
    let mut b = Request::builder().method("GET").uri(format!(
        "/xrpc/com.atproto.space.getRecord?space={}&collection={}&rkey={}",
        urlencoding::encode(space_uri),
        urlencoding::encode(collection),
        urlencoding::encode(rkey),
    ));
    if let Some((name, value)) = cookie {
        b = b.header(name, value);
    }
    b.body(Body::empty()).unwrap()
}

/// Pull the trailing rkey segment off a space record `at://` URI.
fn rkey_from_uri(uri: &str) -> &str {
    uri.rsplit('/')
        .next()
        .expect("uri must have a final segment")
}

// ---------------------------------------------------------------------------
// createRecord
// ---------------------------------------------------------------------------

/// A write-member's createRecord succeeds (201, {uri, cid}) and the record is
/// then retrievable via getRecord with matching cid/value.
#[tokio::test]
#[serial]
async fn create_record_succeeds_for_write_member_and_is_retrievable() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("authority");
    let writer = rand_did("writer");
    let skey = rand_skey("space");
    let (space_id, space_uri) = create_space(&app, &authority, &skey).await;
    add_member(&app, &space_id, &writer, SpaceAccess::Write).await;

    let collection = "com.example.item";
    let record = json!({ "$type": collection, "text": "hello from writer" });

    let resp = app
        .router
        .clone()
        .oneshot(create_record_req(
            &space_uri,
            collection,
            &record,
            Some(cookie_for(&app, &writer)),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = json_of(resp).await;
    let uri = body["uri"].as_str().expect("uri present").to_string();
    let cid = body["cid"].as_str().expect("cid present").to_string();
    assert!(uri.starts_with("at://"));
    assert!(!cid.is_empty());
    // author segment of the returned uri must be the writer's own DID
    assert!(
        uri.contains(&format!("/{writer}/{collection}/")),
        "uri {uri} should embed the writer's DID as author"
    );

    let rkey = rkey_from_uri(&uri).to_string();
    let get_resp = app
        .router
        .clone()
        .oneshot(get_record_req(
            &space_uri,
            collection,
            &rkey,
            Some(cookie_for(&app, &writer)),
        ))
        .await
        .unwrap();
    assert_eq!(get_resp.status(), StatusCode::OK);
    let get_body = json_of(get_resp).await;
    assert_eq!(get_body["uri"], json!(uri));
    assert_eq!(get_body["cid"], json!(cid));
    assert_eq!(get_body["value"], record);
}

/// A caller who is not a member of the space at all cannot createRecord.
#[tokio::test]
#[serial]
async fn create_record_rejects_non_member() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("authority");
    let outsider = rand_did("outsider");
    let skey = rand_skey("space");
    let (_space_id, space_uri) = create_space(&app, &authority, &skey).await;
    // outsider is intentionally never added as a member

    let resp = app
        .router
        .clone()
        .oneshot(create_record_req(
            &space_uri,
            "com.example.item",
            &json!({ "text": "should not land" }),
            Some(cookie_for(&app, &outsider)),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

/// An unauthenticated createRecord request is rejected before it ever reaches
/// membership checks.
#[tokio::test]
#[serial]
async fn create_record_rejects_unauthenticated() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("authority");
    let skey = rand_skey("space");
    let (_space_id, space_uri) = create_space(&app, &authority, &skey).await;

    let resp = app
        .router
        .clone()
        .oneshot(create_record_req(
            &space_uri,
            "com.example.item",
            &json!({ "text": "no auth" }),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// The legacy `dev.happyview.space.createRecord` alias must route to the same
/// handler as the `com.atproto` form.
#[tokio::test]
#[serial]
async fn create_record_legacy_alias_works() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("authority");
    let writer = rand_did("writer");
    let skey = rand_skey("space");
    let (space_id, space_uri) = create_space(&app, &authority, &skey).await;
    add_member(&app, &space_id, &writer, SpaceAccess::Write).await;

    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/dev.happyview.space.createRecord")
        .header("content-type", "application/json")
        .header(cookie_for(&app, &writer).0, cookie_for(&app, &writer).1)
        .body(Body::from(
            json!({
                "space": space_uri,
                "collection": "com.example.item",
                "record": { "text": "via legacy alias" },
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
}

// ---------------------------------------------------------------------------
// putRecord
// ---------------------------------------------------------------------------

/// putRecord creates on first call, updates the same URI on a second call
/// with a different rkey-addressed record, and a stale swapRecord is
/// rejected with 409.
#[tokio::test]
#[serial]
async fn put_record_creates_updates_and_rejects_stale_swap() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("authority");
    let writer = rand_did("writer");
    let skey = rand_skey("space");
    let (space_id, space_uri) = create_space(&app, &authority, &skey).await;
    add_member(&app, &space_id, &writer, SpaceAccess::Write).await;

    let collection = "com.example.doc";
    let rkey = "fixed-rkey-1";
    let record_v1 = json!({ "$type": collection, "text": "v1" });

    // First put: creates.
    let resp1 = app
        .router
        .clone()
        .oneshot(put_record_req(
            &space_uri,
            collection,
            rkey,
            &record_v1,
            None,
            Some(cookie_for(&app, &writer)),
        ))
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::CREATED);
    let body1 = json_of(resp1).await;
    let uri = body1["uri"].as_str().unwrap().to_string();
    let cid1 = body1["cid"].as_str().unwrap().to_string();

    // Second put, same rkey, different content: updates in place.
    let record_v2 = json!({ "$type": collection, "text": "v2" });
    let resp2 = app
        .router
        .clone()
        .oneshot(put_record_req(
            &space_uri,
            collection,
            rkey,
            &record_v2,
            None,
            Some(cookie_for(&app, &writer)),
        ))
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::CREATED);
    let body2 = json_of(resp2).await;
    assert_eq!(body2["uri"], json!(uri), "update must keep the same uri");
    let cid2 = body2["cid"].as_str().unwrap().to_string();
    assert_ne!(cid1, cid2, "content changed, cid must change too");

    // Confirm the update actually landed.
    let get_resp = app
        .router
        .clone()
        .oneshot(get_record_req(
            &space_uri,
            collection,
            rkey,
            Some(cookie_for(&app, &writer)),
        ))
        .await
        .unwrap();
    assert_eq!(get_resp.status(), StatusCode::OK);
    let get_body = json_of(get_resp).await;
    assert_eq!(get_body["value"], record_v2);
    assert_eq!(get_body["cid"], json!(cid2));

    // Third put with a stale/incorrect swapRecord: rejected.
    let record_v3 = json!({ "$type": collection, "text": "v3-should-not-land" });
    let resp3 = app
        .router
        .clone()
        .oneshot(put_record_req(
            &space_uri,
            collection,
            rkey,
            &record_v3,
            Some("bafyreiwrongwrongwrongwrongwrongwrongwrong00"),
            Some(cookie_for(&app, &writer)),
        ))
        .await
        .unwrap();
    assert_eq!(resp3.status(), StatusCode::CONFLICT);

    // The record must be unchanged after the rejected swap.
    let get_resp2 = app
        .router
        .clone()
        .oneshot(get_record_req(
            &space_uri,
            collection,
            rkey,
            Some(cookie_for(&app, &writer)),
        ))
        .await
        .unwrap();
    let get_body2 = json_of(get_resp2).await;
    assert_eq!(
        get_body2["value"], record_v2,
        "conflicting swap must not apply"
    );
    assert_eq!(get_body2["cid"], json!(cid2));

    // A *correct* swapRecord (matching the current cid) does succeed.
    let record_v4 = json!({ "$type": collection, "text": "v4" });
    let resp4 = app
        .router
        .clone()
        .oneshot(put_record_req(
            &space_uri,
            collection,
            rkey,
            &record_v4,
            Some(&cid2),
            Some(cookie_for(&app, &writer)),
        ))
        .await
        .unwrap();
    assert_eq!(resp4.status(), StatusCode::CREATED);
}

/// A non-member cannot putRecord into a space.
#[tokio::test]
#[serial]
async fn put_record_rejects_non_member() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("authority");
    let outsider = rand_did("outsider");
    let skey = rand_skey("space");
    let (_space_id, space_uri) = create_space(&app, &authority, &skey).await;

    let resp = app
        .router
        .clone()
        .oneshot(put_record_req(
            &space_uri,
            "com.example.doc",
            "rk1",
            &json!({ "text": "nope" }),
            None,
            Some(cookie_for(&app, &outsider)),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

/// An unauthenticated putRecord request is rejected.
#[tokio::test]
#[serial]
async fn put_record_rejects_unauthenticated() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("authority");
    let skey = rand_skey("space");
    let (_space_id, space_uri) = create_space(&app, &authority, &skey).await;

    let resp = app
        .router
        .clone()
        .oneshot(put_record_req(
            &space_uri,
            "com.example.doc",
            "rk1",
            &json!({ "text": "nope" }),
            None,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// deleteRecord
// ---------------------------------------------------------------------------

/// A record's own author can delete it via deleteRecord, and it is gone
/// afterwards.
#[tokio::test]
#[serial]
async fn delete_record_author_can_delete_own_record() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("authority");
    let writer = rand_did("writer");
    let skey = rand_skey("space");
    let (space_id, space_uri) = create_space(&app, &authority, &skey).await;
    add_member(&app, &space_id, &writer, SpaceAccess::Write).await;

    let collection = "com.example.item";
    let create_resp = app
        .router
        .clone()
        .oneshot(create_record_req(
            &space_uri,
            collection,
            &json!({ "text": "delete me" }),
            Some(cookie_for(&app, &writer)),
        ))
        .await
        .unwrap();
    assert_eq!(create_resp.status(), StatusCode::CREATED);
    let uri = json_of(create_resp).await["uri"]
        .as_str()
        .unwrap()
        .to_string();
    let rkey = rkey_from_uri(&uri).to_string();

    let del_resp = app
        .router
        .clone()
        .oneshot(delete_record_req(
            &space_uri,
            collection,
            &rkey,
            None,
            Some(cookie_for(&app, &writer)),
        ))
        .await
        .unwrap();
    assert_eq!(del_resp.status(), StatusCode::OK);
    let del_body = json_of(del_resp).await;
    assert_eq!(del_body["success"], json!(true));

    let get_resp = app
        .router
        .clone()
        .oneshot(get_record_req(
            &space_uri,
            collection,
            &rkey,
            Some(cookie_for(&app, &writer)),
        ))
        .await
        .unwrap();
    assert_eq!(get_resp.status(), StatusCode::NOT_FOUND);
}

/// Deleting a record that was never created returns 404.
#[tokio::test]
#[serial]
async fn delete_record_missing_returns_not_found() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("authority");
    let writer = rand_did("writer");
    let skey = rand_skey("space");
    let (space_id, space_uri) = create_space(&app, &authority, &skey).await;
    add_member(&app, &space_id, &writer, SpaceAccess::Write).await;

    let resp = app
        .router
        .clone()
        .oneshot(delete_record_req(
            &space_uri,
            "com.example.item",
            "never-existed",
            None,
            Some(cookie_for(&app, &writer)),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// deleteRecord's author-ownership guard (`service::delete_record`: "You can
/// only delete your own records") returns 403 when it is reached.
///
/// NOTE on how this is reached: `delete_record` builds the record URI to
/// delete from the *caller's own* DID
/// (`.../space/{type}/{skey}/{did}/{collection}/{rkey}`), so a second
/// write-member calling deleteRecord with someone else's collection/rkey
/// addresses their *own* (nonexistent) URI, not the other author's record —
/// they get 404, never 403, through the normal create/put/delete API surface.
/// This mirrors an existing unit test with the same finding
/// (`src/spaces/service.rs::delete_record_forbidden_for_non_author`). To
/// exercise the ownership-check branch at the HTTP layer at all, we
/// reproduce that unit test's setup here: seed a record directly via
/// `spaces_db::insert_space_record` whose URI's embedded DID segment is
/// member B's (so `delete_record` will resolve to this exact URI when B
/// calls it) but whose stored `author_did` is member A's — a state that
/// cannot arise through the public create/put/delete API, only via direct
/// DB seeding (or, presumably, a future data-migration/import path).
#[tokio::test]
#[serial]
async fn delete_record_forbidden_when_uri_and_stored_author_diverge() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("authority");
    let member_a = rand_did("membera");
    let member_b = rand_did("memberb");
    let skey = rand_skey("space");
    let (space_id, space_uri) = create_space(&app, &authority, &skey).await;
    add_member(&app, &space_id, &member_a, SpaceAccess::Write).await;
    add_member(&app, &space_id, &member_b, SpaceAccess::Write).await;

    let collection = "com.example.item";
    let rkey = "fixed-rkey-ownership";
    // The exact URI `delete_record` will construct when member_b calls it
    // with this space/collection/rkey.
    let record_uri = format!("{space_uri}/{member_b}/{collection}/{rkey}");
    let content = json!({ "text": "authored by A" });
    let record = SpaceRecord {
        uri: record_uri.clone(),
        space_id: space_id.clone(),
        author_did: member_a.clone(), // stored author is A, not B
        collection: collection.to_string(),
        rkey: rkey.to_string(),
        record: content.clone(),
        cid: "bafyreiseedforownershiptest0000000000000000".to_string(),
        indexed_at: now_rfc3339(),
    };
    spaces_db::insert_space_record(&app.state.db, app.state.db_backend, &record)
        .await
        .expect("failed to seed mismatched-author record");

    let resp = app
        .router
        .clone()
        .oneshot(delete_record_req(
            &space_uri,
            collection,
            rkey,
            None,
            Some(cookie_for(&app, &member_b)),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // The record must still be present since the delete was rejected.
    let still_there = spaces_db::get_space_record(&app.state.db, app.state.db_backend, &record_uri)
        .await
        .unwrap();
    assert!(still_there.is_some());
}

/// An unauthenticated deleteRecord request is rejected.
#[tokio::test]
#[serial]
async fn delete_record_rejects_unauthenticated() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("authority");
    let skey = rand_skey("space");
    let (_space_id, space_uri) = create_space(&app, &authority, &skey).await;

    let resp = app
        .router
        .clone()
        .oneshot(delete_record_req(
            &space_uri,
            "com.example.item",
            "rk1",
            None,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// applyWrites
// ---------------------------------------------------------------------------

fn write_op_create(collection: &str, rkey: Option<&str>, value: &Value) -> Value {
    let mut op = json!({ "action": "create", "collection": collection, "value": value });
    if let Some(rkey) = rkey {
        op["rkey"] = json!(rkey);
    }
    op
}

fn write_op_update(
    collection: &str,
    rkey: &str,
    value: &Value,
    swap_record: Option<&str>,
) -> Value {
    let mut op = json!({
        "action": "update",
        "collection": collection,
        "rkey": rkey,
        "value": value,
    });
    if let Some(swap) = swap_record {
        op["swapRecord"] = json!(swap);
    }
    op
}

fn write_op_delete(collection: &str, rkey: &str, swap_record: Option<&str>) -> Value {
    let mut op = json!({ "action": "delete", "collection": collection, "rkey": rkey });
    if let Some(swap) = swap_record {
        op["swapRecord"] = json!(swap);
    }
    op
}

fn apply_writes_req(
    space_uri: &str,
    writes: Value,
    swap_commit: Option<&str>,
    cookie: Option<(HeaderName, HeaderValue)>,
) -> Request<Body> {
    let mut body = json!({ "space": space_uri, "writes": writes });
    if let Some(swap) = swap_commit {
        body["swapCommit"] = json!(swap);
    }
    let mut b = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.applyWrites")
        .header("content-type", "application/json");
    if let Some((name, value)) = cookie {
        b = b.header(name, value);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

/// A write-member's applyWrites batch (create + update + delete in one call)
/// succeeds, and every op takes effect: the created record is retrievable
/// with matching content/cid, the updated record's content/cid changed, and
/// the deleted record is gone afterwards. Also confirms the response shape
/// (`{ "results": [...] }`, one entry per write op, in order) and that the
/// created record's uri embeds the caller's own DID as author.
#[tokio::test]
#[serial]
async fn apply_writes_batch_create_update_delete_succeeds_for_write_member() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("authority");
    let writer = rand_did("writer");
    let skey = rand_skey("space");
    let (space_id, space_uri) = create_space(&app, &authority, &skey).await;
    add_member(&app, &space_id, &writer, SpaceAccess::Write).await;

    let collection = "com.example.item";

    // Pre-seed the records that the update/delete ops in the batch will
    // target, via the existing single-write endpoints.
    let update_rkey = "batch-update-target";
    let update_v1 = json!({ "$type": collection, "text": "update v1" });
    let seed_update = app
        .router
        .clone()
        .oneshot(put_record_req(
            &space_uri,
            collection,
            update_rkey,
            &update_v1,
            None,
            Some(cookie_for(&app, &writer)),
        ))
        .await
        .unwrap();
    assert_eq!(seed_update.status(), StatusCode::CREATED);
    let update_uri = json_of(seed_update).await["uri"]
        .as_str()
        .unwrap()
        .to_string();

    let delete_rkey = "batch-delete-target";
    let delete_v1 = json!({ "$type": collection, "text": "will be deleted" });
    let seed_delete = app
        .router
        .clone()
        .oneshot(put_record_req(
            &space_uri,
            collection,
            delete_rkey,
            &delete_v1,
            None,
            Some(cookie_for(&app, &writer)),
        ))
        .await
        .unwrap();
    assert_eq!(seed_delete.status(), StatusCode::CREATED);

    // The batch: create a brand-new record, update the pre-seeded one, and
    // delete the other pre-seeded one.
    let create_value = json!({ "$type": collection, "text": "created in batch" });
    let update_v2 = json!({ "$type": collection, "text": "update v2" });
    let writes = json!([
        write_op_create(collection, None, &create_value),
        write_op_update(collection, update_rkey, &update_v2, None),
        write_op_delete(collection, delete_rkey, None),
    ]);

    let resp = app
        .router
        .clone()
        .oneshot(apply_writes_req(
            &space_uri,
            writes,
            None,
            Some(cookie_for(&app, &writer)),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_of(resp).await;
    let results = body["results"].as_array().expect("results array");
    assert_eq!(results.len(), 3, "one result per write op, in order");

    // create result
    let create_uri = results[0]["uri"]
        .as_str()
        .expect("create result has uri")
        .to_string();
    let create_cid = results[0]["cid"]
        .as_str()
        .expect("create result has cid")
        .to_string();
    assert!(create_uri.starts_with("at://"));
    assert!(!create_cid.is_empty());
    assert!(
        create_uri.contains(&format!("/{writer}/{collection}/")),
        "created record's uri must embed the caller's own DID as author"
    );

    // update result: same uri as the pre-seeded record, new cid
    assert_eq!(results[1]["uri"], json!(update_uri));
    let update_cid2 = results[1]["cid"]
        .as_str()
        .expect("update result has cid")
        .to_string();
    assert!(!update_cid2.is_empty());

    // delete result: empty object
    assert_eq!(results[2], json!({}));

    // Verify via getRecord: created record exists with expected content.
    let create_rkey = rkey_from_uri(&create_uri).to_string();
    let get_created = app
        .router
        .clone()
        .oneshot(get_record_req(
            &space_uri,
            collection,
            &create_rkey,
            Some(cookie_for(&app, &writer)),
        ))
        .await
        .unwrap();
    assert_eq!(get_created.status(), StatusCode::OK);
    let created_body = json_of(get_created).await;
    assert_eq!(created_body["value"], create_value);
    assert_eq!(created_body["cid"], json!(create_cid));

    // Verify the update landed.
    let get_updated = app
        .router
        .clone()
        .oneshot(get_record_req(
            &space_uri,
            collection,
            update_rkey,
            Some(cookie_for(&app, &writer)),
        ))
        .await
        .unwrap();
    assert_eq!(get_updated.status(), StatusCode::OK);
    let updated_body = json_of(get_updated).await;
    assert_eq!(updated_body["value"], update_v2);
    assert_eq!(updated_body["cid"], json!(update_cid2));

    // Verify the delete landed.
    let get_deleted = app
        .router
        .clone()
        .oneshot(get_record_req(
            &space_uri,
            collection,
            delete_rkey,
            Some(cookie_for(&app, &writer)),
        ))
        .await
        .unwrap();
    assert_eq!(get_deleted.status(), StatusCode::NOT_FOUND);
}

/// A non-member's applyWrites batch is rejected by the up-front membership
/// check (403), and nothing in the batch is applied — not even the first op.
#[tokio::test]
#[serial]
async fn apply_writes_rejects_non_member_and_writes_nothing() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("authority");
    let outsider = rand_did("outsider");
    let skey = rand_skey("space");
    let (space_id, space_uri) = create_space(&app, &authority, &skey).await;
    // outsider is intentionally never added as a member

    let collection = "com.example.item";
    let writes = json!([write_op_create(
        collection,
        Some("should-not-exist"),
        &json!({ "text": "nope" })
    )]);

    let resp = app
        .router
        .clone()
        .oneshot(apply_writes_req(
            &space_uri,
            writes,
            None,
            Some(cookie_for(&app, &outsider)),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let (records, _) = spaces_db::list_space_records(
        &app.state.db,
        app.state.db_backend,
        &space_id,
        None,
        None,
        100,
        None,
        false,
    )
    .await
    .unwrap();
    assert!(
        records.is_empty(),
        "membership gate must reject before any write in the batch lands"
    );
}

/// An unauthenticated applyWrites request is rejected before it ever reaches
/// membership checks.
#[tokio::test]
#[serial]
async fn apply_writes_rejects_unauthenticated() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("authority");
    let skey = rand_skey("space");
    let (_space_id, space_uri) = create_space(&app, &authority, &skey).await;

    let writes = json!([write_op_create(
        "com.example.item",
        None,
        &json!({ "text": "no auth" })
    )]);

    let resp = app
        .router
        .clone()
        .oneshot(apply_writes_req(&space_uri, writes, None, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// applyWrites calls `require_membership(..., true, ...)` up front, so a
/// read-access member (not write) is rejected the same way a non-member is,
/// and nothing is written.
#[tokio::test]
#[serial]
async fn apply_writes_rejects_read_only_member_and_writes_nothing() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("authority");
    let reader = rand_did("reader");
    let skey = rand_skey("space");
    let (space_id, space_uri) = create_space(&app, &authority, &skey).await;
    add_member(&app, &space_id, &reader, SpaceAccess::Read).await;

    let collection = "com.example.item";
    let writes = json!([write_op_create(
        collection,
        Some("should-not-exist"),
        &json!({ "text": "nope" })
    )]);

    let resp = app
        .router
        .clone()
        .oneshot(apply_writes_req(
            &space_uri,
            writes,
            None,
            Some(cookie_for(&app, &reader)),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let (records, _) = spaces_db::list_space_records(
        &app.state.db,
        app.state.db_backend,
        &space_id,
        None,
        None,
        100,
        None,
        false,
    )
    .await
    .unwrap();
    assert!(records.is_empty());
}

/// applyWrites' delete op enforces the same author-ownership guard as the
/// standalone `deleteRecord` endpoint (`service::delete_record`): only the
/// record's own author may delete it via the non-swap path. Reproduces the
/// same "divergent author" seeding pattern as
/// `delete_record_forbidden_when_uri_and_stored_author_diverge` above (a
/// state that cannot arise through the public create/put/delete/applyWrites
/// API, only via direct DB seeding) to exercise the ownership-check branch:
/// a record whose URI's embedded DID segment is member B's (so applyWrites
/// resolves to this exact URI when B calls it) but whose stored
/// `author_did` is member A's.
///
/// Before the fix in `apply_writes`'s `WriteOp::Delete` non-swap branch,
/// this op had NO author check at all and the delete succeeded (200),
/// silently destroying another member's record. This test pins the fixed
/// behavior: 403 Forbidden, and the record survives.
#[tokio::test]
#[serial]
async fn apply_writes_delete_rejects_author_mismatch() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("authority");
    let member_a = rand_did("membera");
    let member_b = rand_did("memberb");
    let skey = rand_skey("space");
    let (space_id, space_uri) = create_space(&app, &authority, &skey).await;
    add_member(&app, &space_id, &member_a, SpaceAccess::Write).await;
    add_member(&app, &space_id, &member_b, SpaceAccess::Write).await;

    let collection = "com.example.item";
    let rkey = "fixed-rkey-ownership-applywrites";
    // The exact URI applyWrites' delete op will construct when member_b
    // calls it with this space/collection/rkey.
    let record_uri = format!("{space_uri}/{member_b}/{collection}/{rkey}");
    let record = SpaceRecord {
        uri: record_uri.clone(),
        space_id: space_id.clone(),
        author_did: member_a.clone(), // stored author is A, not B
        collection: collection.to_string(),
        rkey: rkey.to_string(),
        record: json!({ "text": "authored by A" }),
        cid: "bafyreiseedforapplywritesownership0000000000".to_string(),
        indexed_at: now_rfc3339(),
    };
    spaces_db::insert_space_record(&app.state.db, app.state.db_backend, &record)
        .await
        .expect("failed to seed mismatched-author record");

    let writes = json!([write_op_delete(collection, rkey, None)]);
    let resp = app
        .router
        .clone()
        .oneshot(apply_writes_req(
            &space_uri,
            writes,
            None,
            Some(cookie_for(&app, &member_b)),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let still_there = spaces_db::get_space_record(&app.state.db, app.state.db_backend, &record_uri)
        .await
        .unwrap();
    assert!(
        still_there.is_some(),
        "a record authored by someone else must survive an applyWrites delete op"
    );
}

/// applyWrites' delete op returns 404 (mirroring deleteRecord) when the
/// non-swap path targets a record that was never created, rather than
/// silently reporting success for a no-op delete.
#[tokio::test]
#[serial]
async fn apply_writes_delete_of_nonexistent_record_returns_not_found() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("authority");
    let writer = rand_did("writer");
    let skey = rand_skey("space");
    let (space_id, space_uri) = create_space(&app, &authority, &skey).await;
    add_member(&app, &space_id, &writer, SpaceAccess::Write).await;

    let writes = json!([write_op_delete("com.example.item", "never-existed", None)]);
    let resp = app
        .router
        .clone()
        .oneshot(apply_writes_req(
            &space_uri,
            writes,
            None,
            Some(cookie_for(&app, &writer)),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
