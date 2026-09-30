mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::db::now_rfc3339;
use happyview::spaces::db as spaces_db;
use happyview::spaces::types::*;
use serde_json::json;
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

const SPACE_DID: &str = "did:plc:spacehost";
const SPACE_TYPE: &str = "com.example.notify";
const SPACE_SKEY: &str = "main";

fn space_uri() -> String {
    format!("at://{SPACE_DID}/space/{SPACE_TYPE}/{SPACE_SKEY}")
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
    assert!(resp.status().is_success(), "failed to enable spaces flag");
}

async fn create_space(app: &TestApp) {
    let now = now_rfc3339();
    let space = Space {
        id: Uuid::new_v4().to_string(),
        did: SPACE_DID.to_string(),
        authority_did: SPACE_DID.to_string(),
        creator_did: SPACE_DID.to_string(),
        type_nsid: SPACE_TYPE.to_string(),
        skey: SPACE_SKEY.to_string(),
        display_name: Some("Notify Space".to_string()),
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
        .expect("create_space failed");
}

fn notify_write_req(
    auth_cookie: Option<(axum::http::HeaderName, axum::http::HeaderValue)>,
) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.notifyWrite")
        .header("content-type", "application/json");
    if let Some((name, value)) = auth_cookie {
        b = b.header(name, value);
    }
    b.body(Body::from(
        json!({
            "space": space_uri(),
            "did": "did:plc:someauthor",
            "collection": "com.example.post",
            "rkey": "rk1",
        })
        .to_string(),
    ))
    .unwrap()
}

/// An unauthenticated caller must NOT be able to fire write notifications.
/// Before the fix this returned success (the caller was ignored entirely).
#[tokio::test]
#[serial]
async fn notify_write_rejects_unauthenticated() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    create_space(&app).await;

    let resp = app
        .router
        .clone()
        .oneshot(notify_write_req(None))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "unauthenticated notifyWrite must be rejected"
    );
}

/// A super admin is allowed (require_space_admin accepts authority or super).
#[tokio::test]
#[serial]
async fn notify_write_allows_super_admin() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    create_space(&app).await;

    let resp = app
        .router
        .clone()
        .oneshot(notify_write_req(Some(app.admin_cookie())))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// notifySpaceDeleted is likewise gated.
#[tokio::test]
#[serial]
async fn notify_space_deleted_rejects_unauthenticated() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    create_space(&app).await;

    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.notifySpaceDeleted")
        .header("content-type", "application/json")
        .body(Body::from(json!({ "space": space_uri() }).to_string()))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// The member allowed to write in [`create_instance_space`] spaces.
const WRITER: &str = "did:plc:writer";

/// A space whose authority is this instance, with [`WRITER`] as its one
/// writer.
async fn create_instance_space(app: &TestApp, instance_did: &str) -> String {
    create_instance_space_with(app, instance_did, Policy::MemberList).await
}

async fn create_instance_space_with(
    app: &TestApp,
    instance_did: &str,
    write_policy: Policy,
) -> String {
    let now = now_rfc3339();
    let space = Space {
        id: Uuid::new_v4().to_string(),
        did: instance_did.to_string(),
        authority_did: instance_did.to_string(),
        creator_did: "did:plc:creator".to_string(),
        type_nsid: SPACE_TYPE.to_string(),
        skey: SPACE_SKEY.to_string(),
        display_name: None,
        description: None,
        read_policy: Policy::MemberList,
        write_policy,
        app_access: AppAccess::Open,
        config: SpaceConfig::default(),
        revision: None,
        created_at: now.clone(),
        updated_at: now,
    };
    spaces_db::create_space(&app.state.db, app.state.db_backend, &space)
        .await
        .expect("create_space failed");
    spaces_db::add_member(
        &app.state.db,
        app.state.db_backend,
        &SpaceMember {
            id: Uuid::new_v4().to_string(),
            space_id: space.id.clone(),
            did: WRITER.to_string(),
            access: MemberAccess::WRITE,
            is_delegation: false,
            granted_by: None,
            created_at: now_rfc3339(),
        },
    )
    .await
    .expect("add_member failed");
    format!("at://{instance_did}/space/{SPACE_TYPE}/{SPACE_SKEY}")
}

/// Send `notifyWrite` in the lexicon's shape, as a repo host does, with service
/// auth signed by `signer` and addressed to this instance's space host.
async fn notify_write_as(signer: &str, repo: &str) -> StatusCode {
    let mut app = TestApp::new().await;
    let plc_store = common::plc::setup_mock_plc(&app.mock_server).await;
    let instance_did = app.setup_did_web().await;
    enable_spaces(&app).await;
    let space = create_instance_space(&app, &instance_did).await;

    let auth = app
        .service_auth_jwt_for(
            &plc_store,
            signer,
            &instance_did,
            "#atproto_space_host",
            Some("com.atproto.space.notifyWrite"),
        )
        .await;

    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.notifyWrite")
        .header("content-type", "application/json")
        .header("authorization", auth)
        .header("host", "127.0.0.1:0")
        .body(Body::from(
            json!({
                "space": space,
                "repo": repo,
                "rev": "3lzq2b3k4c22a",
                "hash": { "$bytes": "q83vEjRWeJq83vEjRWeJq83vEjRWeJq83vEjRWeJq80" },
            })
            .to_string(),
        ))
        .unwrap();
    app.router.clone().oneshot(req).await.unwrap().status()
}

/// A repo host notifies with service auth signed as the account that wrote.
#[tokio::test]
#[serial]
async fn notify_write_accepts_the_writers_repo_host() {
    common::require_db!();
    let status = notify_write_as("did:plc:writer", "did:plc:writer").await;
    assert_eq!(status, StatusCode::OK);
}

/// The space tracks only writers its write policy admits.
#[tokio::test]
#[serial]
async fn notify_write_rejects_a_writer_the_space_does_not_admit() {
    common::require_db!();
    let status = notify_write_as("did:plc:outsider", "did:plc:outsider").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// One account cannot report writes to another account's repo.
#[tokio::test]
#[serial]
async fn notify_write_rejects_a_notification_for_someone_elses_repo() {
    common::require_db!();
    let status = notify_write_as("did:plc:intruder", "did:plc:writer").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// `did:plc:writer`'s repo host reporting a fixed repo state.
fn repo_host_notification(auth: &str, space: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.notifyWrite")
        .header("content-type", "application/json")
        .header("authorization", auth)
        .header("host", "127.0.0.1:0")
        .body(Body::from(
            json!({
                "space": space,
                "repo": "did:plc:writer",
                "rev": "3lzq2b3k4c22a",
                "hash": { "$bytes": "q83vEjRWeJq83vEjRWeJq83vEjRWeJq83vEjRWeJq80" },
            })
            .to_string(),
        ))
        .unwrap()
}

/// A notification accepted from a repo host is passed on to registered syncers
/// in the lexicon's shape, signed by this instance.
#[tokio::test]
#[serial]
async fn notify_write_forwards_to_registered_syncers() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let plc_store = common::plc::setup_mock_plc(&app.mock_server).await;
    let instance_did = app.setup_did_web().await;
    enable_spaces(&app).await;
    let space = create_instance_space(&app, &instance_did).await;
    let space_row = spaces_db::get_space_by_address(
        &app.state.db,
        app.state.db_backend,
        &instance_did,
        SPACE_TYPE,
        SPACE_SKEY,
    )
    .await
    .unwrap()
    .unwrap();

    let syncer = common::syncer::start().await;
    happyview::spaces::notifications::register(
        &app.state.db,
        app.state.db_backend,
        &space_row.id,
        "did:web:syncer.example#atproto_space_syncer",
        &syncer.uri(),
        "did:web:syncer.example",
        NotifyDelivery::Xrpc,
    )
    .await
    .unwrap();

    let auth = app
        .service_auth_jwt_for(
            &plc_store,
            "did:plc:writer",
            &instance_did,
            "#atproto_space_host",
            Some("com.atproto.space.notifyWrite"),
        )
        .await;
    let resp = app
        .router
        .clone()
        .oneshot(repo_host_notification(&auth, &space))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let received = common::syncer::received(&syncer, 1).await;
    assert_eq!(received.len(), 1, "the syncer should be notified once");
    let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(body["space"], json!(space));
    assert_eq!(body["repo"], json!("did:plc:writer"));
    assert_eq!(body["rev"], json!("3lzq2b3k4c22a"));
    assert!(body["hash"]["$bytes"].is_string());
    assert!(
        body["spaceRev"].is_string(),
        "forwarded notifications carry the space revision"
    );

    // The same state reported again is not news to anyone.
    let resp = app
        .router
        .clone()
        .oneshot(repo_host_notification(&auth, &space))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(common::syncer::received(&syncer, 2).await.len(), 1);
    let authorization = received[0]
        .headers
        .get("authorization")
        .expect("forwarded notifications carry service auth")
        .to_str()
        .unwrap();
    assert!(authorization.starts_with("Bearer "));
}

/// A managing app is named by a service identifier, and is asked at the
/// endpoint that service publishes.
#[tokio::test]
#[serial]
async fn a_managing_app_is_reached_through_its_service_entry() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    common::require_db!();
    let mut app = TestApp::new().await;
    let plc_store = common::plc::setup_mock_plc(&app.mock_server).await;
    let instance_did = app.setup_did_web().await;
    enable_spaces(&app).await;

    let managing_app = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/xrpc/com.atproto.simplespace.checkUserAccess"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "authorized": true })))
        .expect(1)
        .mount(&managing_app)
        .await;
    plc_store.write().await.insert(
        "did:plc:forumapp".to_string(),
        json!({
            "id": "did:plc:forumapp",
            "verificationMethod": [],
            "service": [{ "id": "#forum", "type": "ForumApp", "serviceEndpoint": managing_app.uri() }],
        }),
    );
    let space = create_instance_space_with(
        &app,
        &instance_did,
        Policy::ManagingApp {
            managing_app: "did:plc:forumapp#forum".into(),
        },
    )
    .await;

    let auth = app
        .service_auth_jwt_for(
            &plc_store,
            "did:plc:newcomer",
            &instance_did,
            "#atproto_space_host",
            Some("com.atproto.space.notifyWrite"),
        )
        .await;
    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.notifyWrite")
        .header("content-type", "application/json")
        .header("authorization", auth)
        .header("host", "127.0.0.1:0")
        .body(Body::from(
            json!({
                "space": space,
                "repo": "did:plc:newcomer",
                "rev": "3lzq2b3k4c22a",
                "hash": { "$bytes": "q83vEjRWeJq83vEjRWeJq83vEjRWeJq83vEjRWeJq80" },
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// Services registered by identifier hear about a deleted space through the
/// lexicon method, signed by this instance, at their service endpoint.
#[tokio::test]
#[serial]
async fn deleting_a_space_notifies_registered_syncers() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    common::require_db!();
    let mut app = TestApp::new().await;
    let instance_did = app.setup_did_web().await;
    enable_spaces(&app).await;
    let space = create_instance_space(&app, &instance_did).await;
    let space_row = spaces_db::get_space_by_address(
        &app.state.db,
        app.state.db_backend,
        &instance_did,
        SPACE_TYPE,
        SPACE_SKEY,
    )
    .await
    .unwrap()
    .unwrap();

    let syncer = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.space.notifySpaceDeleted"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&syncer)
        .await;
    happyview::spaces::notifications::register(
        &app.state.db,
        app.state.db_backend,
        &space_row.id,
        "did:web:syncer.example#atproto_space_syncer",
        &syncer.uri(),
        "did:web:syncer.example",
        NotifyDelivery::Xrpc,
    )
    .await
    .unwrap();

    let (name, value) = common::auth::admin_cookie_header("did:plc:creator", &app.state.cookie_key);
    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.simplespace.deleteSpace")
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(json!({ "space": space }).to_string()))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert!(
        resp.status().is_success(),
        "deleteSpace failed: {}",
        resp.status()
    );

    let mut received = Vec::new();
    for _ in 0..50 {
        received = syncer.received_requests().await.unwrap_or_default();
        if !received.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(received.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(body["space"], json!(space));
    assert!(received[0].headers.get("authorization").is_some());
}
