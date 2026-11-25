//! `clearRecords` under an app's OAuth grant: the grant is checked before a
//! repo on the author's PDS is forwarded, and a clear that finds the repo
//! changed under it since the check asks to be retried.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::db::now_rfc3339;
use happyview::oauth::pds_write::generate_dpop_proof;
use happyview::spaces::db as spaces_db;
use happyview::spaces::types::*;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

const TYPE_NSID: &str = "com.example.clearscope";
const COLLECTION: &str = "com.example.item";

async fn json_of(resp: axum::http::Response<Body>) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap_or(json!(null))
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
            .is_success()
    );
}

/// An app's DPoP session for `did`, granted `scopes`.
struct Session {
    client_key: String,
    dpop_key: Value,
    access_token: String,
}

async fn session_for(app: &TestApp, did: &str, scopes: &str) -> Session {
    let (client_key, client_secret, _) = app.create_api_client("confidential", None).await;
    let req = Request::builder()
        .method("POST")
        .uri("/oauth/dpop-keys")
        .header("content-type", "application/json")
        .header("x-client-key", &client_key)
        .header("x-client-secret", &client_secret)
        .body(Body::from("{}"))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let key = json_of(resp).await;

    let access_token = format!("token-{}", Uuid::new_v4().simple());
    app.mock_session_verification(did, did).await;
    let req = Request::builder()
        .method("POST")
        .uri("/oauth/sessions")
        .header("content-type", "application/json")
        .header("x-client-key", &client_key)
        .header("x-client-secret", &client_secret)
        .body(Body::from(
            json!({
                "provision_id": key["provision_id"],
                "did": did,
                "access_token": access_token,
                "scopes": "atproto",
                "pds_url": "https://pds.example.com",
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let session = json_of(resp).await;

    // The scopes the app registered and the authorization server granted.
    let sql = happyview::db::adapt_sql(
        "UPDATE happyview_dpop_sessions SET scopes = ?, granted_scope = ?, token_expires_at = ? WHERE id = ?",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(scopes)
        .bind(scopes)
        .bind((chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339())
        .bind(session["session_id"].as_str().unwrap())
        .execute(&app.state.db)
        .await
        .unwrap();

    Session {
        client_key,
        dpop_key: key["dpop_key"].clone(),
        access_token,
    }
}

fn clear_request(session: &Session, space: &str, repo: &str) -> Request<Body> {
    let url = "http://127.0.0.1:0/xrpc/com.atproto.space.clearRecords";
    let proof = generate_dpop_proof(&session.dpop_key, "POST", url, &session.access_token, None)
        .expect("a DPoP proof");
    Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.clearRecords")
        .header("host", "127.0.0.1:0")
        .header("content-type", "application/json")
        .header("x-client-key", &session.client_key)
        .header("authorization", format!("DPoP {}", session.access_token))
        .header("dpop", proof)
        .body(Body::from(
            json!({ "space": space, "repo": repo }).to_string(),
        ))
        .unwrap()
}

/// A memberlist space, `author` a writing member with one record in it.
async fn setup(label: &str) -> (TestApp, Space, String, String) {
    let app = TestApp::new_with_encryption().await;
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

    let (name, value) = common::auth::admin_cookie_header(&author, &app.state.cookie_key);
    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.createRecord")
        .header("content-type", "application/json")
        .header(name, value)
        .body(Body::from(
            json!({ "space": uri, "collection": COLLECTION, "record": { "$type": COLLECTION, "text": "one" } })
                .to_string(),
        ))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert!(
        resp.status().is_success(),
        "createRecord: {}",
        resp.status()
    );
    (app, space, uri, author)
}

const SET_NATIVE: &str = "UPDATE happyview_space_repo_state SET host_mode = 'native' WHERE space_id = ? AND author_did = ?";

/// Wait until a request on another connection waits on a lock, on Postgres;
/// on SQLite, give it time to reach its `BEGIN IMMEDIATE`.
async fn until_waiting_on_a_lock(app: &TestApp) {
    if app.state.db_backend != happyview::db::DatabaseBackend::Postgres {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        return;
    }
    for _ in 0..100 {
        let waiting: i64 = happyview::db::query_scalar(
            "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock'",
        )
        .fetch_one(&app.state.db)
        .await
        .unwrap();
        if waiting > 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    }
    panic!("the clear never waited on the space");
}

/// A write transaction holding the space, as another write would.
async fn hold_space(app: &TestApp, space_id: &str) -> sqlx::Transaction<'static, sqlx::Any> {
    let mut tx = happyview::db::begin_write(&app.state.db, app.state.db_backend)
        .await
        .unwrap();
    if app.state.db_backend == happyview::db::DatabaseBackend::Postgres {
        happyview::db::query("SELECT id FROM happyview_spaces WHERE id = $1 FOR UPDATE")
            .bind(space_id)
            .execute(&mut *tx)
            .await
            .unwrap();
    }
    tx
}

#[tokio::test]
#[serial]
async fn a_repo_on_its_pds_is_forwarded_only_under_a_grant_for_every_collection() {
    common::require_db!();
    let (app, space, uri, author) = setup("clearpds").await;
    let sql = happyview::db::adapt_sql(SET_NATIVE, app.state.db_backend);
    happyview::db::query(&sql)
        .bind(&space.id)
        .bind(&author)
        .execute(&app.state.db)
        .await
        .unwrap();

    // A grant naming the one collection HappyView has seen is refused before
    // anything is forwarded.
    let named = session_for(
        &app,
        &author,
        &format!("atproto space:{TYPE_NSID}?authority=*&collection={COLLECTION}&action=delete"),
    )
    .await;
    let resp = app
        .router
        .clone()
        .oneshot(clear_request(&named, &uri, &author))
        .await
        .unwrap();
    let status = resp.status();
    let body = json_of(resp).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        !body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("lives on your PDS"),
        "refused by the grant check, not at the forward: {body}"
    );

    // A grant for every collection reaches the forward. This run holds no
    // OAuth session to the author's PDS, which is where the forward stops.
    let every = session_for(
        &app,
        &author,
        &format!("atproto space:{TYPE_NSID}?authority=*&collection=*&action=delete"),
    )
    .await;
    let resp = app
        .router
        .clone()
        .oneshot(clear_request(&every, &uri, &author))
        .await
        .unwrap();
    let body = json_of(resp).await;
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("lives on your PDS"),
        "reached the forward: {body}"
    );
}

#[tokio::test]
#[serial]
async fn a_repo_held_here_needs_a_delete_grant_on_what_it_holds() {
    common::require_db!();
    let (app, _space, uri, author) = setup("clearhere").await;
    let other = session_for(
        &app,
        &author,
        &format!(
            "atproto space:{TYPE_NSID}?authority=*&collection=com.example.other&action=delete"
        ),
    )
    .await;
    let resp = app
        .router
        .clone()
        .oneshot(clear_request(&other, &uri, &author))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let held = session_for(
        &app,
        &author,
        &format!("atproto space:{TYPE_NSID}?authority=*&collection={COLLECTION}&action=delete"),
    )
    .await;
    let resp = app
        .router
        .clone()
        .oneshot(clear_request(&held, &uri, &author))
        .await
        .unwrap();
    let status = resp.status();
    assert_eq!(status, StatusCode::OK, "{}", json_of(resp).await);
}

#[tokio::test]
#[serial]
async fn a_collection_written_while_the_clear_waits_is_a_conflict() {
    common::require_db!();
    let (app, space, uri, author) = setup("clearrace").await;
    let session = session_for(
        &app,
        &author,
        &format!("atproto space:{TYPE_NSID}?authority=*&collection=*&action=delete"),
    )
    .await;

    // Another write holds the space and adds a collection the clear's grant
    // check didn't see.
    let mut other = hold_space(&app, &space.id).await;
    let value = json!({ "$type": "com.example.other" });
    spaces_db::insert_space_record(
        &mut *other,
        app.state.db_backend,
        &SpaceRecord {
            uri: format!("{uri}/{author}/com.example.other/self"),
            space_id: space.id.clone(),
            author_did: author.clone(),
            collection: "com.example.other".into(),
            rkey: "self".into(),
            cid: happyview::cid_verify::compute_record_cid(&value)
                .unwrap()
                .to_string(),
            record: value,
            indexed_at: now_rfc3339(),
        },
    )
    .await
    .unwrap();

    let router = app.router.clone();
    let req = clear_request(&session, &uri, &author);
    let clear = tokio::spawn(async move {
        let resp = router.oneshot(req).await.unwrap();
        (resp.status(), json_of(resp).await)
    });
    until_waiting_on_a_lock(&app).await;
    other.commit().await.unwrap();

    let (status, body) = clear.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[tokio::test]
#[serial]
async fn a_repo_that_moves_to_its_pds_while_the_clear_waits_is_a_conflict() {
    common::require_db!();
    let (app, space, uri, author) = setup("clearmove").await;
    let session = session_for(
        &app,
        &author,
        &format!("atproto space:{TYPE_NSID}?authority=*&collection={COLLECTION}&action=delete"),
    )
    .await;

    let mut other = hold_space(&app, &space.id).await;
    let sql = happyview::db::adapt_sql(SET_NATIVE, app.state.db_backend);
    happyview::db::query(&sql)
        .bind(&space.id)
        .bind(&author)
        .execute(&mut *other)
        .await
        .unwrap();

    let router = app.router.clone();
    let req = clear_request(&session, &uri, &author);
    let clear = tokio::spawn(async move {
        let resp = router.oneshot(req).await.unwrap();
        (resp.status(), json_of(resp).await)
    });
    until_waiting_on_a_lock(&app).await;
    other.commit().await.unwrap();

    let (status, body) = clear.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}
