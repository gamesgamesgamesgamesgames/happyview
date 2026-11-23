//! Space writes in the lexicons' shape: `applyWrites` writes tagged by the
//! `$type` union (beside HappyView's earlier `action` tag), `createRecord` at
//! a key the caller chose, and the lexicons' `RecordAlreadyExists` and
//! `RecordNotFound`.

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

const TYPE_NSID: &str = "com.example.writeshapes";
const COLLECTION: &str = "com.example.item";
const CREATE: &str = "com.atproto.space.applyWrites#create";
const UPDATE: &str = "com.atproto.space.applyWrites#update";
const DELETE: &str = "com.atproto.space.applyWrites#delete";

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
            .is_success()
    );
}

fn cookie_for(app: &TestApp, did: &str) -> (HeaderName, HeaderValue) {
    common::auth::admin_cookie_header(did, &app.state.cookie_key)
}

/// A memberlist space, and `author`, a writing member.
async fn setup(label: &str) -> (TestApp, String, String) {
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    let authority = format!("did:plc:{label}{}", Uuid::new_v4().simple());
    let author = format!("did:plc:{label}{}", Uuid::new_v4().simple());
    let now = now_rfc3339();
    let space = Space {
        id: Uuid::new_v4().to_string(),
        did: authority.clone(),
        authority_did: authority.clone(),
        creator_did: authority.clone(),
        type_nsid: TYPE_NSID.to_string(),
        skey: format!("{label}{}", Uuid::new_v4().simple()),
        display_name: None,
        description: None,
        read_policy: Policy::MemberList,
        write_policy: Policy::MemberList,
        app_access: AppAccess::Open,
        config: SpaceConfig::default(),
        revision: None,
        created_at: now.clone(),
        updated_at: now,
    };
    spaces_db::create_space(&app.state.db, app.state.db_backend, &space)
        .await
        .unwrap();
    spaces_db::add_member(
        &app.state.db,
        app.state.db_backend,
        &SpaceMember {
            id: Uuid::new_v4().to_string(),
            space_id: space.id.clone(),
            did: author.clone(),
            access: MemberAccess::WRITE,
            is_delegation: false,
            granted_by: None,
            created_at: now_rfc3339(),
        },
    )
    .await
    .unwrap();
    let uri = format!("at://{authority}/space/{TYPE_NSID}/{}", space.skey);
    (app, uri, author)
}

async fn post(app: &TestApp, did: &str, method: &str, body: Value) -> (StatusCode, Value) {
    let (name, value) = cookie_for(app, did);
    let req = Request::builder()
        .method("POST")
        .uri(format!("/xrpc/com.atproto.space.{method}"))
        .header("content-type", "application/json")
        .header(name, value)
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(json!(null)),
    )
}

async fn rkeys(app: &TestApp, did: &str, space: &str) -> Vec<String> {
    let (name, value) = cookie_for(app, did);
    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/xrpc/com.atproto.space.listRecords?space={}&repo={}",
            urlencoding::encode(space),
            urlencoding::encode(did)
        ))
        .header(name, value)
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    let mut keys: Vec<String> = body["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["rkey"].as_str().unwrap().to_string())
        .collect();
    keys.sort();
    keys
}

fn record(text: &str) -> Value {
    json!({ "$type": COLLECTION, "text": text })
}

#[tokio::test]
#[serial]
async fn apply_writes_takes_the_lexicons_type_union() {
    common::require_db!();
    let (app, space, author) = setup("union").await;
    let (status, body) = post(
        &app,
        &author,
        "applyWrites",
        json!({
            "space": space,
            "repo": author,
            "writes": [
                { "$type": CREATE, "collection": COLLECTION, "rkey": "a", "value": record("a") },
                { "$type": CREATE, "collection": COLLECTION, "rkey": "b", "value": record("b") },
            ],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let results = body["results"].as_array().unwrap();
    assert_eq!(
        results[0]["$type"],
        "com.atproto.space.applyWrites#createResult"
    );
    assert!(
        results[0]["uri"]
            .as_str()
            .unwrap()
            .ends_with("/com.example.item/a"),
        "{body}"
    );
    assert!(results[0]["cid"].is_string());

    let (status, body) = post(
        &app,
        &author,
        "applyWrites",
        json!({
            "space": space,
            "writes": [
                { "$type": UPDATE, "collection": COLLECTION, "rkey": "a", "value": record("a2") },
                { "$type": DELETE, "collection": COLLECTION, "rkey": "b" },
            ],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["results"][0]["$type"],
        "com.atproto.space.applyWrites#updateResult"
    );
    assert_eq!(
        body["results"][1],
        json!({ "$type": "com.atproto.space.applyWrites#deleteResult" })
    );
    assert_eq!(rkeys(&app, &author, &space).await, ["a"]);
}

#[tokio::test]
#[serial]
async fn apply_writes_keeps_the_action_tag() {
    common::require_db!();
    let (app, space, author) = setup("action").await;
    let (status, body) = post(
        &app,
        &author,
        "applyWrites",
        json!({
            "space": space,
            "writes": [{ "action": "create", "collection": COLLECTION, "rkey": "a", "value": record("a") }],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["results"][0]["uri"].is_string(), "{body}");
    assert!(
        body["results"][0].get("$type").is_none(),
        "the earlier shape's results are as they were: {body}"
    );

    // HappyView's earlier update creates a missing record, and deleting one is
    // a 404, as before.
    let (status, body) = post(
        &app,
        &author,
        "applyWrites",
        json!({
            "space": space,
            "writes": [{ "action": "update", "collection": COLLECTION, "rkey": "new", "value": record("new") }],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(rkeys(&app, &author, &space).await, ["a", "new"]);
    let (status, _) = post(
        &app,
        &author,
        "applyWrites",
        json!({
            "space": space,
            "writes": [{ "action": "delete", "collection": COLLECTION, "rkey": "missing" }],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
#[serial]
async fn the_lexicons_update_and_delete_need_the_record() {
    common::require_db!();
    let (app, space, author) = setup("missing").await;
    for write in [
        json!({ "$type": UPDATE, "collection": COLLECTION, "rkey": "missing", "value": record("x") }),
        json!({ "$type": DELETE, "collection": COLLECTION, "rkey": "missing" }),
    ] {
        let (status, body) = post(
            &app,
            &author,
            "applyWrites",
            json!({ "space": space, "writes": [write] }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"], "RecordNotFound", "{body}");
    }
    assert!(rkeys(&app, &author, &space).await.is_empty());
}

#[tokio::test]
#[serial]
async fn a_create_on_a_taken_key_is_record_already_exists() {
    common::require_db!();
    let (app, space, author) = setup("taken").await;
    let (status, body) = post(
        &app,
        &author,
        "createRecord",
        json!({ "space": space, "repo": author, "collection": COLLECTION, "rkey": "self", "record": record("one") }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(
        body["uri"]
            .as_str()
            .unwrap()
            .ends_with("/com.example.item/self"),
        "{body}"
    );

    let (status, body) = post(
        &app,
        &author,
        "createRecord",
        json!({ "space": space, "collection": COLLECTION, "rkey": "self", "record": record("two") }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "RecordAlreadyExists", "{body}");

    let (status, body) = post(
        &app,
        &author,
        "applyWrites",
        json!({ "space": space, "writes": [{ "$type": CREATE, "collection": COLLECTION, "rkey": "self", "value": record("three") }] }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "RecordAlreadyExists", "{body}");

    // The earlier shape keeps its answer: a 409.
    let (status, body) = post(
        &app,
        &author,
        "applyWrites",
        json!({ "space": space, "writes": [{ "action": "create", "collection": COLLECTION, "rkey": "self", "value": record("three") }] }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // Two creates of one key in one batch: the second is refused, and the
    // batch with it.
    let (status, body) = post(
        &app,
        &author,
        "applyWrites",
        json!({
            "space": space,
            "writes": [
                { "$type": CREATE, "collection": COLLECTION, "rkey": "twice", "value": record("1") },
                { "$type": CREATE, "collection": COLLECTION, "rkey": "twice", "value": record("2") },
            ],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "RecordAlreadyExists", "{body}");
    assert_eq!(
        rkeys(&app, &author, &space).await,
        ["self"],
        "nothing of the batch landed"
    );
}

#[tokio::test]
#[serial]
async fn create_record_without_a_key_still_assigns_one() {
    common::require_db!();
    let (app, space, author) = setup("tidkey").await;
    let (status, body) = post(
        &app,
        &author,
        "createRecord",
        json!({ "space": space, "collection": COLLECTION, "record": record("one") }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let rkey = body["uri"].as_str().unwrap().rsplit('/').next().unwrap();
    assert_eq!(rkey.len(), 13, "a TID: {rkey}");
}

#[tokio::test]
#[serial]
async fn a_malformed_write_is_invalid_request() {
    common::require_db!();
    let (app, space, author) = setup("malformed").await;
    let cases = [
        (
            "a bad key",
            "createRecord",
            json!({ "space": space, "collection": COLLECTION, "rkey": "no/slashes", "record": record("x") }),
        ),
        (
            "a bad key in a batch",
            "applyWrites",
            json!({ "space": space, "writes": [{ "$type": CREATE, "collection": COLLECTION, "rkey": "..", "value": record("x") }] }),
        ),
        (
            "an unknown type",
            "applyWrites",
            json!({ "space": space, "writes": [{ "$type": "com.atproto.space.applyWrites#upsert", "collection": COLLECTION, "rkey": "a", "value": record("x") }] }),
        ),
        (
            "two tags in one batch",
            "applyWrites",
            json!({ "space": space, "writes": [
                { "$type": CREATE, "collection": COLLECTION, "rkey": "a", "value": record("x") },
                { "action": "create", "collection": COLLECTION, "rkey": "b", "value": record("y") },
            ] }),
        ),
    ];
    for (label, method, body) in cases {
        let (status, answer) = post(&app, &author, method, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{label}: {answer}");
        assert_eq!(answer["error"], "InvalidRequest", "{label}: {answer}");
    }
    assert!(rkeys(&app, &author, &space).await.is_empty());
}

#[tokio::test]
#[serial]
async fn a_batch_is_capped_at_200_writes_in_either_shape() {
    common::require_db!();
    let (app, space, author) = setup("cap").await;
    let writes = |n: usize, tag: &dyn Fn() -> (&'static str, &'static str)| -> Vec<Value> {
        (0..n)
            .map(|i| {
                let (key, value) = tag();
                let mut w = json!({ "collection": COLLECTION, "rkey": format!("k{i}"), "value": record("x") });
                w[key] = value.into();
                w
            })
            .collect()
    };
    let lexicon = || ("$type", CREATE);
    let action = || ("action", "create");
    for (label, tag) in [("$type", &lexicon as &dyn Fn() -> _), ("action", &action)] {
        let (status, body) = post(
            &app,
            &author,
            "applyWrites",
            json!({ "space": space, "writes": writes(201, tag) }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{label}: {body}");
        assert_eq!(body["error"], "InvalidRequest", "{label}: {body}");
    }
    assert!(rkeys(&app, &author, &space).await.is_empty());
    let (status, body) = post(
        &app,
        &author,
        "applyWrites",
        json!({ "space": space, "writes": writes(200, &lexicon) }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
#[serial]
async fn the_repo_named_must_be_the_callers() {
    common::require_db!();
    let (app, space, author) = setup("repo").await;
    let other = format!("did:plc:other{}", Uuid::new_v4().simple());
    for (method, body) in [
        (
            "createRecord",
            json!({ "space": space, "repo": other, "collection": COLLECTION, "record": record("x") }),
        ),
        (
            "applyWrites",
            json!({ "space": space, "repo": other, "writes": [{ "$type": CREATE, "collection": COLLECTION, "value": record("x") }] }),
        ),
    ] {
        let (status, answer) = post(&app, &author, method, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method}: {answer}");
    }
    assert!(rkeys(&app, &author, &space).await.is_empty());
}

#[tokio::test]
#[serial]
async fn the_earlier_shape_keeps_its_answers() {
    common::require_db!();
    let (app, space, author) = setup("earlier").await;
    // A `$type` that isn't one of applyWrites' write types is ignored, as it
    // always was, and the write is read by its `action`.
    let (status, body) = post(
        &app,
        &author,
        "applyWrites",
        json!({ "space": space, "writes": [{ "$type": "com.atproto.repo.applyWrites#create", "action": "create", "collection": COLLECTION, "rkey": "a", "value": record("a") }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["results"][0].get("$type").is_none(), "{body}");

    // A write with an `action` is read by it, whatever its `$type` names.
    let (status, body) = post(
        &app,
        &author,
        "applyWrites",
        json!({ "space": space, "writes": [{ "$type": DELETE, "action": "create", "collection": COLLECTION, "rkey": "b", "value": record("b") }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["results"][0]["uri"]
            .as_str()
            .unwrap_or_default()
            .ends_with("/b"),
        "{body}"
    );

    // A malformed write is a 422, as before.
    let (status, body) = post(
        &app,
        &author,
        "applyWrites",
        json!({ "space": space, "writes": [{ "action": "create", "collection": COLLECTION }] }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body.is_null(),
        "the extractor's plain-text refusal, as before: {body}"
    );

    // Record keys are read as they were: no record-key check on this shape.
    let (status, body) = post(
        &app,
        &author,
        "applyWrites",
        json!({ "space": space, "writes": [{ "action": "delete", "collection": COLLECTION, "rkey": "not a key" }] }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
#[serial]
async fn the_lexicons_writes_take_no_swap_and_build_on_each_other() {
    common::require_db!();
    let (app, space, author) = setup("swapless").await;
    // The lexicons' writes take no swapRecord, so one sent is ignored, and a
    // missing record is RecordNotFound as without it.
    let swap = "bafyreihje4zjrs24gnxiciokziubf2bcsnwivwl5k3lhbvhyrhjuh7vcgi";
    for write in [
        json!({ "$type": DELETE, "collection": COLLECTION, "rkey": "a", "swapRecord": swap }),
        json!({ "$type": UPDATE, "collection": COLLECTION, "rkey": "a", "value": record("x"), "swapRecord": swap }),
    ] {
        let (status, body) = post(
            &app,
            &author,
            "applyWrites",
            json!({ "space": space, "writes": [write] }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"], "RecordNotFound", "{body}");
    }

    // A create, then an update of the same key, in one batch.
    let (status, body) = post(
        &app,
        &author,
        "applyWrites",
        json!({ "space": space, "writes": [
            { "$type": CREATE, "collection": COLLECTION, "rkey": "a", "value": record("1") },
            { "$type": UPDATE, "collection": COLLECTION, "rkey": "a", "value": record("2") },
        ] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["results"][1]["$type"],
        "com.atproto.space.applyWrites#updateResult"
    );
    assert_eq!(rkeys(&app, &author, &space).await, ["a"]);
}

#[tokio::test]
#[serial]
async fn an_oversized_batch_is_refused_whatever_it_holds() {
    common::require_db!();
    let (app, space, author) = setup("capfirst").await;
    let mut writes: Vec<Value> = (0..201)
        .map(|i| json!({ "$type": CREATE, "collection": COLLECTION, "rkey": format!("k{i}"), "value": record("x") }))
        .collect();
    writes[3] = json!({ "$type": "com.atproto.space.applyWrites#upsert" });
    let (status, body) = post(
        &app,
        &author,
        "applyWrites",
        json!({ "space": space, "writes": writes }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("too many writes"),
        "the cap is checked first: {body}"
    );
}

#[tokio::test]
#[serial]
async fn an_oversized_batch_is_refused_before_the_space_is_looked_up() {
    common::require_db!();
    let (app, _space, author) = setup("capearly").await;
    let writes: Vec<Value> = (0..201)
        .map(|i| json!({ "action": "create", "collection": COLLECTION, "rkey": format!("k{i}"), "value": record("x") }))
        .collect();
    let missing = format!("at://did:plc:nobody/space/{TYPE_NSID}/none");
    let (status, body) = post(
        &app,
        &author,
        "applyWrites",
        json!({ "space": missing, "writes": writes }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "InvalidRequest", "{body}");
}
