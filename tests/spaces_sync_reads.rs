//! Sync reads in the lexicons' shape: the repo named `repo` (HappyView's
//! earlier routes took `did`, which keeps its old answers), `since` on
//! `listRepoOps`, `excludeValues` on `listRecords`, `RepoNotFound`,
//! `InvalidCursor`, a STAR `getRepo`, `repo` on `getBlob`, and `clearRecords`.

mod common;

use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Request, StatusCode};
use base64::Engine;
use happyview::db::now_rfc3339;
use happyview::spaces::db as spaces_db;
use happyview::spaces::types::*;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

const TYPE_NSID: &str = "com.example.syncreads";
const COLLECTION: &str = "com.example.item";
/// sha256 of the all-zero LtHash state: the `data` of an empty repo.
const EMPTY_DATA_HEX: &str = "e5a00aa9991ac8a5ee3109844d84a55583bd20572ad3ffcd42792f3c36b183ad";

fn rand_did(label: &str) -> String {
    format!("did:plc:{label}{}", Uuid::new_v4().simple())
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
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert!(resp.status().is_success(), "failed to enable spaces");
}

fn cookie_for(app: &TestApp, did: &str) -> (HeaderName, HeaderValue) {
    common::auth::admin_cookie_header(did, &app.state.cookie_key)
}

async fn body_of(resp: axum::http::Response<Body>) -> (StatusCode, Option<String>, Vec<u8>) {
    let status = resp.status();
    let content_type = resp
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, content_type, bytes)
}

fn json_from(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap_or(json!(null))
}

/// A memberlist space whose authority and `author` are writing members.
async fn setup(label: &str) -> (TestApp, String, String, String) {
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    let authority = rand_did(label);
    let author = rand_did(label);
    let skey = format!("{label}{}", Uuid::new_v4().simple());
    let now = now_rfc3339();
    let id = Uuid::new_v4().to_string();
    let space = Space {
        id: id.clone(),
        did: authority.clone(),
        authority_did: authority.clone(),
        creator_did: authority.clone(),
        type_nsid: TYPE_NSID.to_string(),
        skey: skey.clone(),
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
    for did in [&authority, &author] {
        spaces_db::add_member(
            &app.state.db,
            app.state.db_backend,
            &SpaceMember {
                id: Uuid::new_v4().to_string(),
                space_id: id.clone(),
                did: did.clone(),
                access: MemberAccess::WRITE,
                is_delegation: false,
                granted_by: None,
                created_at: now_rfc3339(),
            },
        )
        .await
        .unwrap();
    }
    let uri = format!("at://{authority}/space/{TYPE_NSID}/{skey}");
    (app, uri, authority, author)
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
    let (status, _, bytes) = body_of(app.router.clone().oneshot(req).await.unwrap()).await;
    (status, json_from(&bytes))
}

async fn write(app: &TestApp, space: &str, did: &str, collection: &str, text: &str) -> Value {
    let (status, body) = post(
        app,
        did,
        "createRecord",
        json!({ "space": space, "collection": collection, "record": { "$type": collection, "text": text } }),
    )
    .await;
    assert!(status.is_success(), "createRecord: {status} {body}");
    body
}

/// GET `method` as `reader` with `query` (already encoded), raw.
async fn get_raw(
    app: &TestApp,
    reader: &str,
    method: &str,
    query: &str,
) -> (StatusCode, Option<String>, Vec<u8>) {
    let (name, value) = cookie_for(app, reader);
    let req = Request::builder()
        .method("GET")
        .uri(format!("/xrpc/com.atproto.space.{method}?{query}"))
        .header(name, value)
        .body(Body::empty())
        .unwrap();
    body_of(app.router.clone().oneshot(req).await.unwrap()).await
}

async fn get(app: &TestApp, reader: &str, method: &str, query: &str) -> (StatusCode, Value) {
    let (status, _, bytes) = get_raw(app, reader, method, query).await;
    (status, json_from(&bytes))
}

fn q(space: &str, name: &str, repo: &str) -> String {
    format!(
        "space={}&{name}={}",
        urlencoding::encode(space),
        urlencoding::encode(repo)
    )
}

fn data_hex(commit: &Value) -> String {
    let b64 = commit["data"]["$bytes"].as_str().expect("data as $bytes");
    hex::encode(
        base64::engine::general_purpose::STANDARD_NO_PAD
            .decode(b64.trim_end_matches('='))
            .unwrap(),
    )
}

#[tokio::test]
#[serial]
async fn a_repo_request_gets_the_lexicon_commit_and_a_did_request_keeps_its_own() {
    common::require_db!();
    let (app, space, authority, author) = setup("lexcommit").await;
    write(&app, &space, &author, COLLECTION, "one").await;

    let (status, by_repo) = get(
        &app,
        &authority,
        "getLatestCommit",
        &q(&space, "repo", &author),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{by_repo}");
    let commit = &by_repo["commit"];
    assert_eq!(commit["ver"], 1, "{commit}");
    assert_eq!(commit["repo"], author.as_str(), "{commit}");
    assert_eq!(commit["space"], space.as_str(), "{commit}");
    assert!(commit["rev"].is_string(), "{commit}");
    assert_eq!(by_repo["rev"], commit["rev"], "{by_repo}");

    // The same state, read the earlier way.
    let (status, by_did) = get(
        &app,
        &authority,
        "getLatestCommit",
        &q(&space, "did", &author),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{by_did}");
    assert_eq!(by_did["rev"], commit["rev"]);
    let earlier = &by_did["commit"];
    let earlier_hash = if earlier["data"].is_object() {
        &earlier["data"]
    } else {
        &earlier["hash"]
    };
    assert_eq!(
        data_hex(&json!({ "data": earlier_hash })),
        data_hex(commit),
        "both shapes carry the same set hash"
    );

    // getRepoState is an alias, and answers the same.
    let (status, alias) = get(
        &app,
        &authority,
        "getRepoState",
        &q(&space, "repo", &author),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{alias}");
    assert_eq!(alias["commit"], *commit);
}

#[tokio::test]
#[serial]
async fn the_repo_must_be_named_once() {
    common::require_db!();
    let (app, space, authority, author) = setup("named").await;
    write(&app, &space, &author, COLLECTION, "one").await;

    for method in ["getLatestCommit", "getRepo", "listRepoOps"] {
        let (status, body) = get(
            &app,
            &authority,
            method,
            &format!("space={}", urlencoding::encode(&space)),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method}: {body}");
        assert_eq!(body["error"], "InvalidRequest", "{method}: {body}");

        let both = format!(
            "{}&did={}",
            q(&space, "repo", &author),
            urlencoding::encode(&authority)
        );
        let (status, body) = get(&app, &authority, method, &both).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method}: {body}");
        assert_eq!(body["error"], "InvalidRequest", "{method}: {body}");

        // The same repo under both names is the lexicons' request.
        let same = format!(
            "{}&did={}",
            q(&space, "repo", &author),
            urlencoding::encode(&author)
        );
        let (status, _, _) = get_raw(&app, &authority, method, &same).await;
        assert_eq!(status, StatusCode::OK, "{method}");
    }
}

#[tokio::test]
#[serial]
async fn a_repo_never_written_is_not_found_by_repo_and_empty_by_did() {
    common::require_db!();
    let (app, space, authority, author) = setup("notfound").await;

    for method in ["getLatestCommit", "getRepo", "listRepoOps"] {
        let (status, body) = get(&app, &authority, method, &q(&space, "repo", &author)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method}: {body}");
        assert_eq!(body["error"], "RepoNotFound", "{method}: {body}");
    }

    let (status, body) = get(
        &app,
        &authority,
        "getLatestCommit",
        &q(&space, "did", &author),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["commit"].is_null(), "{body}");
    let (status, body) = get(&app, &authority, "listRepoOps", &q(&space, "did", &author)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["commit"].is_null(), "{body}");
}

#[tokio::test]
#[serial]
async fn get_repo_by_repo_is_a_star_archive_that_reproduces_the_commit() {
    common::require_db!();
    let (app, space, authority, author) = setup("star").await;
    // `.reply` sorts before the shorter name as a path, after it as a column.
    write(&app, &space, &author, "com.example.item", "one").await;
    write(&app, &space, &author, "com.example.item.reply", "two").await;
    write(&app, &space, &author, "com.example.item", "three").await;

    let (_, commit) = get(
        &app,
        &authority,
        "getLatestCommit",
        &q(&space, "repo", &author),
    )
    .await;
    let commit = commit["commit"].clone();

    let (status, content_type, bytes) =
        get_raw(&app, &authority, "getRepo", &q(&space, "repo", &author)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("application/vnd.dasl.star"));

    let mut reader =
        happyview_star::StarReader::open(bytes.as_slice(), happyview_star::Limits::default())
            .unwrap();
    assert_eq!(reader.profile(), "atproto-space-repo");
    let meta = reader.meta().clone();
    use ipld_core::ipld::Ipld;
    assert_eq!(meta.get("repo"), Some(&Ipld::String(author.clone())));
    assert_eq!(meta.get("space"), Some(&Ipld::String(space.clone())));
    assert_eq!(
        meta.get("rev"),
        Some(&Ipld::String(commit["rev"].as_str().unwrap().into()))
    );
    let Some(Ipld::Bytes(data)) = meta.get("data") else {
        panic!("no data: {meta:?}")
    };
    assert_eq!(hex::encode(data), data_hex(&commit));

    let mut keys = Vec::new();
    let mut state = happyview::spaces::lthash::LtHashState::new();
    while let Some(pair) = reader.next_pair().unwrap() {
        let path = String::from_utf8(pair.key).unwrap();
        let (collection, rkey) = path.rsplit_once('/').unwrap();
        let cid = happyview::cid_verify::dag_cbor_cid(&pair.value)
            .unwrap()
            .to_string();
        state.add(&happyview::spaces::lthash::record_element(
            collection, rkey, &cid,
        ));
        keys.push(path);
    }
    assert_eq!(keys.len(), 3);
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted, "pairs are in byte order of their paths");
    assert!(keys[0].starts_with("com.example.item.reply/"), "{keys:?}");
    assert_eq!(hex::encode(state.hash()), data_hex(&commit));

    // The earlier request still gets its CAR.
    let (status, content_type, _) =
        get_raw(&app, &authority, "getRepo", &q(&space, "did", &author)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("application/vnd.ipld.car"));
}

/// Three writes, one rev each, and the revs in order.
async fn three_revs(app: &TestApp, space: &str, authority: &str, author: &str) -> Vec<String> {
    for text in ["one", "two", "three"] {
        write(app, space, author, COLLECTION, text).await;
    }
    let (_, body) = get(app, authority, "listRepoOps", &q(space, "repo", author)).await;
    body["ops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|op| op["rev"].as_str().unwrap().to_string())
        .collect()
}

fn revs_of(body: &Value) -> Vec<String> {
    body["ops"]
        .as_array()
        .unwrap_or_else(|| panic!("{body}"))
        .iter()
        .map(|op| op["rev"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
#[serial]
async fn list_repo_ops_since_is_exclusive_and_works_with_a_cursor() {
    common::require_db!();
    let (app, space, authority, author) = setup("since").await;
    let revs = three_revs(&app, &space, &authority, &author).await;
    assert_eq!(revs.len(), 3);

    let base = q(&space, "repo", &author);
    let (status, body) = get(
        &app,
        &authority,
        "listRepoOps",
        &format!("{base}&since={}", revs[0]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        revs_of(&body),
        revs[1..].to_vec(),
        "since excludes its own rev"
    );
    assert!(!body["commit"].is_null(), "{body}");

    // Paging from a cursor that is past since keeps paging.
    let (_, page1) = get(
        &app,
        &authority,
        "listRepoOps",
        &format!("{base}&limit=1&since={}", revs[0]),
    )
    .await;
    assert_eq!(revs_of(&page1), vec![revs[1].clone()], "{page1}");
    let cursor = page1["cursor"]
        .as_str()
        .expect("a full page has a cursor")
        .to_string();
    let (_, page2) = get(
        &app,
        &authority,
        "listRepoOps",
        &format!(
            "{base}&since={}&cursor={}",
            revs[0],
            urlencoding::encode(&cursor)
        ),
    )
    .await;
    assert_eq!(revs_of(&page2), vec![revs[2].clone()], "{page2}");

    // A since past the cursor wins.
    let early_cursor = format!("{}:0", revs[0]);
    let (_, body) = get(
        &app,
        &authority,
        "listRepoOps",
        &format!(
            "{base}&since={}&cursor={}",
            revs[1],
            urlencoding::encode(&early_cursor)
        ),
    )
    .await;
    assert_eq!(revs_of(&body), vec![revs[2].clone()], "{body}");

    // since reads the same on the earlier request.
    let (_, body) = get(
        &app,
        &authority,
        "listRepoOps",
        &format!("{}&since={}", q(&space, "did", &author), revs[1]),
    )
    .await;
    assert_eq!(revs_of(&body), vec![revs[2].clone()], "{body}");
}

#[tokio::test]
#[serial]
async fn a_malformed_cursor_or_since_is_refused() {
    common::require_db!();
    let (app, space, authority, author) = setup("badcursor").await;
    three_revs(&app, &space, &authority, &author).await;

    let (status, body) = get(
        &app,
        &authority,
        "listRepoOps",
        &format!("{}&cursor=not-a-cursor", q(&space, "repo", &author)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "InvalidCursor", "{body}");

    // The earlier request reads any cursor, as it always has.
    let (status, body) = get(
        &app,
        &authority,
        "listRepoOps",
        &format!("{}&cursor=not-a-cursor", q(&space, "did", &author)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    for name in ["repo", "did"] {
        let (status, body) = get(
            &app,
            &authority,
            "listRepoOps",
            &format!("{}&since=yesterday", q(&space, name, &author)),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}: {body}");
        assert_eq!(body["error"], "InvalidRequest", "{name}: {body}");
    }

    let (status, body) = get(
        &app,
        &authority,
        "listRecords",
        &format!("{}&cursor=not-a-cursor", q(&space, "repo", &author)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "InvalidCursor", "{body}");
}

#[tokio::test]
#[serial]
async fn list_records_inlines_values_unless_asked_not_to() {
    common::require_db!();
    let (app, space, authority, author) = setup("values").await;
    write(&app, &space, &author, COLLECTION, "one").await;
    let base = q(&space, "repo", &author);

    let (status, body) = get(&app, &authority, "listRecords", &base).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["records"][0]["value"]["text"], "one", "{body}");

    for opt_out in [
        "excludeValues=true",
        "includeValues=false",
        "includeValues=true&excludeValues=true",
    ] {
        let (status, body) = get(
            &app,
            &authority,
            "listRecords",
            &format!("{base}&{opt_out}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{opt_out}: {body}");
        assert!(
            body["records"][0].get("value").is_none(),
            "{opt_out}: {body}"
        );
        assert!(body["records"][0]["cid"].is_string(), "{opt_out}: {body}");
    }
    let (_, body) = get(
        &app,
        &authority,
        "listRecords",
        &format!("{base}&excludeValues=false"),
    )
    .await;
    assert_eq!(body["records"][0]["value"]["text"], "one", "{body}");
}

#[tokio::test]
#[serial]
async fn get_blob_naming_a_repo_needs_that_repos_record() {
    common::require_db!();
    let (app, space, authority, author) = setup("blobrepo").await;
    let cid = "bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku";
    let (status, body) = post(
        &app,
        &author,
        "createRecord",
        json!({
            "space": space,
            "collection": COLLECTION,
            "record": {
                "$type": COLLECTION,
                "image": { "$type": "blob", "ref": { "$link": cid }, "mimeType": "image/png", "size": 1 },
            },
        }),
    )
    .await;
    assert!(status.is_success(), "{status} {body}");

    let (status, body) = get(
        &app,
        &authority,
        "getBlob",
        &format!("{}&cid={cid}", q(&space, "repo", &authority)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "BlobNotFound", "{body}");
}

#[tokio::test]
#[serial]
async fn clear_records_empties_the_callers_repo_in_one_commit() {
    common::require_db!();
    let (app, space, authority, author) = setup("clear").await;
    write(&app, &space, &author, COLLECTION, "one").await;
    write(&app, &space, &author, "com.example.other", "two").await;
    write(&app, &space, &authority, COLLECTION, "theirs").await;
    let (_, before) = get(
        &app,
        &authority,
        "getLatestCommit",
        &q(&space, "repo", &author),
    )
    .await;

    // Only the caller's own repo.
    let (status, body) = post(
        &app,
        &author,
        "clearRecords",
        json!({ "space": space, "repo": authority }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, body) = post(
        &app,
        &author,
        "clearRecords",
        json!({ "space": space, "repo": author }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({}));

    let (_, listed) = get(&app, &authority, "listRecords", &q(&space, "repo", &author)).await;
    assert_eq!(listed["records"], json!([]), "{listed}");
    let (_, after) = get(
        &app,
        &authority,
        "getLatestCommit",
        &q(&space, "repo", &author),
    )
    .await;
    assert_eq!(data_hex(&after["commit"]), EMPTY_DATA_HEX, "{after}");
    assert_ne!(
        after["commit"]["rev"], before["commit"]["rev"],
        "a clear advances the rev"
    );

    // Syncers following the op log see both deletes, in one rev.
    let (_, ops) = get(
        &app,
        &authority,
        "listRepoOps",
        &format!(
            "{}&since={}",
            q(&space, "repo", &author),
            before["commit"]["rev"].as_str().unwrap()
        ),
    )
    .await;
    let ops = ops["ops"].as_array().unwrap();
    assert_eq!(ops.len(), 2, "{ops:?}");
    assert!(
        ops.iter()
            .all(|op| op["cid"].is_null() && op["rev"] == after["commit"]["rev"]),
        "{ops:?}"
    );

    // Another account's records are untouched.
    let (_, theirs) = get(
        &app,
        &authority,
        "listRecords",
        &q(&space, "repo", &authority),
    )
    .await;
    assert_eq!(theirs["records"].as_array().unwrap().len(), 1, "{theirs}");

    // Clearing an empty repo changes nothing.
    let (status, _) = post(
        &app,
        &author,
        "clearRecords",
        json!({ "space": space, "repo": author }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, again) = get(
        &app,
        &authority,
        "getLatestCommit",
        &q(&space, "repo", &author),
    )
    .await;
    assert_eq!(again["commit"]["rev"], after["commit"]["rev"]);
}

#[tokio::test]
#[serial]
async fn clear_records_keeps_the_configuration_a_space_publishes_in_its_own_repo() {
    common::require_db!();
    // The space is on the authority's DID here, so the authority's repo is the
    // space's own.
    let (app, space, authority, _author) = setup("clearkeep").await;
    write(&app, &space, &authority, COLLECTION, "mine").await;
    let skey = space.rsplit('/').next().unwrap();
    let row = spaces_db::get_space_by_address(
        &app.state.db,
        app.state.db_backend,
        &authority,
        TYPE_NSID,
        skey,
    )
    .await
    .unwrap()
    .expect("the space");
    let policy_uri = format!("{space}/{authority}/com.atproto.space.policy/self");
    let policy = json!({ "$type": "com.atproto.space.policy", "policy": { "$type": "com.atproto.memberspace.policy" } });
    spaces_db::insert_space_record(
        &app.state.db,
        app.state.db_backend,
        &SpaceRecord {
            uri: policy_uri.clone(),
            space_id: row.id.clone(),
            author_did: authority.clone(),
            collection: "com.atproto.space.policy".into(),
            rkey: "self".into(),
            cid: happyview::cid_verify::compute_record_cid(&policy)
                .unwrap()
                .to_string(),
            record: policy,
            indexed_at: now_rfc3339(),
        },
    )
    .await
    .unwrap();

    let (status, body) = post(
        &app,
        &authority,
        "clearRecords",
        json!({ "space": space, "repo": authority }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (_, listed) = get(
        &app,
        &authority,
        "listRecords",
        &q(&space, "repo", &authority),
    )
    .await;
    let collections: Vec<&str> = listed["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["collection"].as_str().unwrap())
        .collect();
    assert_eq!(collections, ["com.atproto.space.policy"], "{listed}");
}
