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
    notify_write_with(signer, repo, "repoRev", "3lzq2b3k4c22a")
        .await
        .0
}

/// `notify_write_as`, naming the revision field and value, returning the
/// status and body.
async fn notify_write_with(
    signer: &str,
    repo: &str,
    rev_field: &str,
    rev: &str,
) -> (StatusCode, serde_json::Value) {
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
                rev_field: rev,
                "hash": { "$bytes": "q83vEjRWeJq83vEjRWeJq83vEjRWeJq83vEjRWeJq80" },
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null),
    )
}

/// Repo hosts on the alpha lexicon name the repo revision `rev`. Accepted
/// until v3.
#[tokio::test]
#[serial]
async fn notify_write_accepts_the_alpha_rev_field() {
    common::require_db!();
    let (status, _) =
        notify_write_with("did:plc:writer", "did:plc:writer", "rev", "3lzq2b3k4c22a").await;
    assert_eq!(status, StatusCode::OK);
}

/// A repo revision further ahead than clock skew allows is refused by name.
#[tokio::test]
#[serial]
async fn notify_write_refuses_a_future_repo_rev() {
    common::require_db!();
    // A TID in the year 2100.
    let (status, body) = notify_write_with(
        "did:plc:writer",
        "did:plc:writer",
        "repoRev",
        "7zzzzzzzzzzzz",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], json!("FutureRev"));
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
    repo_host_notification_naming(auth, space, "hash")
}

/// `repo_host_notification`, naming the field that carries the repo's digest.
fn repo_host_notification_naming(auth: &str, space: &str, digest_field: &str) -> Request<Body> {
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
                "repoRev": "3lzq2b3k4c22a",
                digest_field: { "$bytes": "q83vEjRWeJq83vEjRWeJq83vEjRWeJq83vEjRWeJq80" },
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
    assert_eq!(body["repoRev"], json!("3lzq2b3k4c22a"));
    // Syncers on the alpha lexicon read `rev`. Sent until v3.
    assert_eq!(body["rev"], json!("3lzq2b3k4c22a"));
    assert!(body["data"]["$bytes"].is_string());
    // Syncers that predate proposal 0016 read `hash`. Sent until every
    // syncer reads `data`.
    assert_eq!(body["hash"], body["data"]);
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

/// A repo host on proposal 0016 reports the digest as `data`, and the writer
/// set advances on it as it does on `hash`.
#[tokio::test]
#[serial]
async fn notify_write_records_a_digest_reported_as_data() {
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

    // Only an update that advanced the writer set is forwarded, so a syncer
    // hearing of it is the proof that `data` was recorded.
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
        .oneshot(repo_host_notification_naming(&auth, &space, "data"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let received = common::syncer::received(&syncer, 1).await;
    assert_eq!(
        received.len(),
        1,
        "a digest sent as data should be recorded"
    );
    let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(
        body["data"]["$bytes"],
        json!("q83vEjRWeJq83vEjRWeJq83vEjRWeJq83vEjRWeJq80")
    );
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
                "repoRev": "3lzq2b3k4c22a",
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
        .uri("/xrpc/com.atproto.space.deleteSpace")
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

/// A repo host told the space does not exist stops retrying, so the error is
/// named.
#[tokio::test]
#[serial]
async fn notify_write_names_an_unknown_space() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let plc_store = common::plc::setup_mock_plc(&app.mock_server).await;
    let instance_did = app.setup_did_web().await;
    enable_spaces(&app).await;

    let auth = app
        .service_auth_jwt_for(
            &plc_store,
            WRITER,
            &instance_did,
            "#atproto_space_host",
            Some("com.atproto.space.notifyWrite"),
        )
        .await;
    let missing = format!("at://{instance_did}/space/{SPACE_TYPE}/no-such-space");
    let resp = app
        .router
        .clone()
        .oneshot(repo_host_notification(&auth, &missing))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"], json!("SpaceNotFound"));
}

/// Service auth for a space host may name the authority's bare DID instead of
/// its `#atproto_space_host` service.
#[tokio::test]
#[serial]
async fn notify_write_accepts_the_bare_authority_did_as_audience() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let plc_store = common::plc::setup_mock_plc(&app.mock_server).await;
    let instance_did = app.setup_did_web().await;
    enable_spaces(&app).await;
    let space = create_instance_space(&app, &instance_did).await;

    let auth = app
        .service_auth_jwt_for(
            &plc_store,
            WRITER,
            &instance_did,
            "",
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
}

/// Create a space through `createSpace` as `creator`, returning its uri. It is
/// created on a DID of its own.
async fn create_space_on_its_own_did(app: &TestApp, creator: &str) -> String {
    create_space_on_its_own_did_with(app, creator, json!({})).await
}

/// `create_space_on_its_own_did`, with `extra` merged into the request.
async fn create_space_on_its_own_did_with(
    app: &TestApp,
    creator: &str,
    extra: serde_json::Value,
) -> String {
    let (name, value) = common::auth::admin_cookie_header(creator, &app.state.cookie_key);
    let mut body = json!({ "spaceType": SPACE_TYPE, "skey": SPACE_SKEY, "policy": { "$type": "com.atproto.memberspace.policy" } });
    for (k, v) in extra.as_object().unwrap() {
        body[k] = v.clone();
    }
    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.createSpace")
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice::<serde_json::Value>(&body).unwrap()["uri"]
        .as_str()
        .unwrap()
        .to_string()
}

/// `notifyWrite` to a space on its own DID, with service auth addressed to
/// `aud_fragment` of that DID, on an instance that exposes no identity of its
/// own.
async fn notify_space_did(aud_fragment: &str) -> StatusCode {
    let app = TestApp::new().await;
    let plc_store = common::plc::setup_mock_plc(&app.mock_server).await;
    enable_spaces(&app).await;
    let writer = format!("did:plc:writer{}", Uuid::new_v4().simple());
    let space = create_space_on_its_own_did(&app, &writer).await;
    let space_did = space.trim_start_matches("at://").split('/').next().unwrap();

    let auth = app
        .service_auth_jwt_for(
            &plc_store,
            &writer,
            space_did,
            aud_fragment,
            Some("com.atproto.space.notifyWrite"),
        )
        .await;
    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.notifyWrite")
        .header("content-type", "application/json")
        .header("authorization", auth)
        .body(Body::from(
            json!({
                "space": space,
                "repo": writer,
                "repoRev": "3lzq2b3k4c22a",
                "data": { "$bytes": "q83vEjRWeJq83vEjRWeJq83vEjRWeJq83vEjRWeJq80" },
            })
            .to_string(),
        ))
        .unwrap();
    app.router.clone().oneshot(req).await.unwrap().status()
}

/// Proposal 0016 addresses a space host as its authority's `#atproto_pds`. A
/// space DID this instance holds is an audience in its own right, whatever
/// identity the instance itself exposes.
#[tokio::test]
#[serial]
async fn notify_write_accepts_service_auth_addressed_to_the_space_did() {
    common::require_db!();
    assert_eq!(notify_space_did("#atproto_pds").await, StatusCode::OK);
    assert_eq!(notify_space_did("").await, StatusCode::OK);
}

#[tokio::test]
#[serial]
async fn notify_write_refuses_another_service_of_the_space_did() {
    common::require_db!();
    assert_eq!(
        notify_space_did("#atproto_labeler").await,
        StatusCode::UNAUTHORIZED
    );
}

/// A space on its own DID forwards write notifications signed as that DID,
/// with the key its genesis operation published as `#atproto`.
#[tokio::test]
#[serial]
async fn a_space_on_its_own_did_forwards_notifications_signed_as_it() {
    use base64::Engine;
    use p256::ecdsa::signature::Verifier;

    common::require_db!();
    let app = TestApp::new().await;
    let plc_store = common::plc::setup_mock_plc(&app.mock_server).await;
    enable_spaces(&app).await;
    let writer = format!("did:plc:writer{}", Uuid::new_v4().simple());
    let space = create_space_on_its_own_did(&app, &writer).await;
    let space_did = space
        .trim_start_matches("at://")
        .split('/')
        .next()
        .unwrap()
        .to_string();
    let space_row = spaces_db::get_space_by_address(
        &app.state.db,
        app.state.db_backend,
        &space_did,
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
            &writer,
            &space_did,
            "#atproto_pds",
            Some("com.atproto.space.notifyWrite"),
        )
        .await;
    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.notifyWrite")
        .header("content-type", "application/json")
        .header("authorization", auth)
        .body(Body::from(
            json!({
                "space": space,
                "repo": writer,
                "repoRev": "3lzq2b3k4c22a",
                "data": { "$bytes": "q83vEjRWeJq83vEjRWeJq83vEjRWeJq83vEjRWeJq80" },
            })
            .to_string(),
        ))
        .unwrap();
    assert_eq!(
        app.router.clone().oneshot(req).await.unwrap().status(),
        StatusCode::OK
    );

    let received = common::syncer::received(&syncer, 1).await;
    assert_eq!(received.len(), 1, "the syncer should be notified once");
    let token = received[0]
        .headers
        .get("authorization")
        .unwrap()
        .to_str()
        .unwrap()
        .trim_start_matches("Bearer ")
        .to_string();
    let parts: Vec<&str> = token.split('.').collect();
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let payload: serde_json::Value =
        serde_json::from_slice(&b64.decode(parts[1]).unwrap()).unwrap();
    assert_eq!(payload["iss"], json!(space_did));

    let genesis = plc_store
        .read()
        .await
        .get(&space_did)
        .cloned()
        .expect("the space's DID was published");
    let published = genesis["verificationMethods"]["atproto"].as_str().unwrap();
    let (_, multikey) = multibase::decode(published.trim_start_matches("did:key:")).unwrap();
    let verifying = p256::ecdsa::VerifyingKey::from_sec1_bytes(&multikey[2..]).unwrap();
    let signature = p256::ecdsa::Signature::from_slice(&b64.decode(parts[2]).unwrap()).unwrap();
    verifying
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .expect("signed with the space DID's #atproto key");
}

fn space_did_of(space: &str) -> String {
    space
        .trim_start_matches("at://")
        .split('/')
        .next()
        .unwrap()
        .to_string()
}

/// A space on its own DID asks its managing app with service auth signed as
/// that DID.
#[tokio::test]
#[serial]
async fn a_space_on_its_own_did_asks_its_managing_app_as_it() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    common::require_db!();
    let app = TestApp::new().await;
    let plc_store = common::plc::setup_mock_plc(&app.mock_server).await;
    enable_spaces(&app).await;

    let managing_app = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/xrpc/com.atproto.simplespace.checkUserAccess"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "authorized": true })))
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
    let creator = format!("did:plc:creator{}", Uuid::new_v4().simple());
    let space = create_space_on_its_own_did_with(
        &app,
        &creator,
        json!({ "policy": {
            "$type": "com.atproto.managedspace.policy",
            "managingApp": "did:plc:forumapp#forum",
        }}),
    )
    .await;
    let space_did = space_did_of(&space);

    let newcomer = format!("did:plc:newcomer{}", Uuid::new_v4().simple());
    let auth = app
        .service_auth_jwt_for(
            &plc_store,
            &newcomer,
            &space_did,
            "#atproto_pds",
            Some("com.atproto.space.notifyWrite"),
        )
        .await;
    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.notifyWrite")
        .header("content-type", "application/json")
        .header("authorization", auth)
        .body(Body::from(
            json!({
                "space": space,
                "repo": newcomer,
                "repoRev": "3lzq2b3k4c22a",
                "data": { "$bytes": "q83vEjRWeJq83vEjRWeJq83vEjRWeJq83vEjRWeJq80" },
            })
            .to_string(),
        ))
        .unwrap();
    assert_eq!(
        app.router.clone().oneshot(req).await.unwrap().status(),
        StatusCode::OK
    );

    let asked = managing_app.received_requests().await.unwrap();
    assert_eq!(asked.len(), 1, "the managing app is asked once");
    let authorization = asked[0]
        .headers
        .get("authorization")
        .unwrap()
        .to_str()
        .unwrap();
    app.assert_signed_as_space(authorization, &space_did).await;
}

/// A space on its own DID tells its registered syncers it was deleted with
/// service auth signed as that DID.
#[tokio::test]
#[serial]
async fn a_space_on_its_own_did_announces_its_deletion_as_it() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    let creator = format!("did:plc:creator{}", Uuid::new_v4().simple());
    let space = create_space_on_its_own_did(&app, &creator).await;
    let space_did = space_did_of(&space);
    let space_row = spaces_db::get_space_by_address(
        &app.state.db,
        app.state.db_backend,
        &space_did,
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

    let (name, value) = common::auth::admin_cookie_header(&creator, &app.state.cookie_key);
    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.deleteSpace")
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(json!({ "space": space }).to_string()))
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

    let mut received = Vec::new();
    for _ in 0..50 {
        received = syncer.received_requests().await.unwrap_or_default();
        if !received.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(received.len(), 1, "the syncer is told once");
    let authorization = received[0]
        .headers
        .get("authorization")
        .unwrap()
        .to_str()
        .unwrap();
    app.assert_signed_as_space(authorization, &space_did).await;
}

/// Service auth addressed to one space's DID is refused on another space's
/// routes, though this instance holds both DIDs.
#[tokio::test]
#[serial]
async fn service_auth_for_one_space_is_refused_on_another() {
    common::require_db!();
    let app = TestApp::new().await;
    let plc_store = common::plc::setup_mock_plc(&app.mock_server).await;
    enable_spaces(&app).await;
    let writer = format!("did:plc:writer{}", Uuid::new_v4().simple());
    let space = create_space_on_its_own_did(&app, &writer).await;
    let other = create_space_on_its_own_did_with(&app, &writer, json!({ "skey": "other" })).await;
    let other_did = space_did_of(&other);

    let auth = app
        .service_auth_jwt_for(
            &plc_store,
            &writer,
            &other_did,
            "#atproto_pds",
            Some("com.atproto.space.notifyWrite"),
        )
        .await;
    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.notifyWrite")
        .header("content-type", "application/json")
        .header("authorization", auth)
        .body(Body::from(
            json!({
                "space": space,
                "repo": writer,
                "repoRev": "3lzq2b3k4c22a",
                "data": { "$bytes": "q83vEjRWeJq83vEjRWeJq83vEjRWeJq83vEjRWeJq80" },
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
