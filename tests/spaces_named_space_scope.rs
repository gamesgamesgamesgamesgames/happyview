//! An app's `space:` grant naming the space type by parameter
//! (`space?spaceType=…`, the Nov 12 text) authorises writes as the positional
//! form does.

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

const TYPE_NSID: &str = "com.example.namedscope";
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

fn create_request(session: &Session, space: &str) -> Request<Body> {
    let url = "http://127.0.0.1:0/xrpc/com.atproto.space.createRecord";
    let proof = generate_dpop_proof(&session.dpop_key, "POST", url, &session.access_token, None)
        .expect("a DPoP proof");
    Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.createRecord")
        .header("host", "127.0.0.1:0")
        .header("content-type", "application/json")
        .header("x-client-key", &session.client_key)
        .header("authorization", format!("DPoP {}", session.access_token))
        .header("dpop", proof)
        .body(Body::from(
            json!({ "space": space, "collection": COLLECTION, "record": { "$type": COLLECTION, "text": "x" } })
                .to_string(),
        ))
        .unwrap()
}

/// A memberlist space, `author` a writing member.
async fn setup(label: &str) -> (TestApp, String, String) {
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
    (app, uri, author)
}

#[tokio::test]
#[serial]
async fn a_grant_naming_its_space_type_authorises_a_write() {
    common::require_db!();
    let (app, uri, author) = setup("named").await;
    for scope in [
        format!("atproto space?spaceType={TYPE_NSID}&authority=*&collection=*&action=create"),
        format!("atproto space?type={TYPE_NSID}&authority=*&collection=*&action=create"),
    ] {
        let session = session_for(&app, &author, &scope).await;
        let resp = app
            .router
            .clone()
            .oneshot(create_request(&session, &uri))
            .await
            .unwrap();
        let status = resp.status();
        assert_eq!(
            status,
            StatusCode::CREATED,
            "{scope}: {}",
            json_of(resp).await
        );
    }

    // A named grant for another type covers nothing here.
    let other = session_for(
        &app,
        &author,
        "atproto space?spaceType=com.example.other&authority=*&collection=*&action=create",
    )
    .await;
    let resp = app
        .router
        .clone()
        .oneshot(create_request(&other, &uri))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}
