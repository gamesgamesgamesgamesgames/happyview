mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::db::{adapt_sql, now_rfc3339};
use happyview::spaces::db as spaces_db;
use happyview::spaces::types::*;
use serde_json::json;
use serial_test::serial;
use sha2::{Digest, Sha256};
use tokio::sync::Barrier;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

const AUTHORITY: &str = "did:plc:inviteuses-authority";

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

async fn create_space(app: &TestApp) -> String {
    let now = now_rfc3339();
    let id = Uuid::new_v4().to_string();
    let space = Space {
        id: id.clone(),
        did: AUTHORITY.to_string(),
        authority_did: AUTHORITY.to_string(),
        creator_did: AUTHORITY.to_string(),
        type_nsid: "com.example.inviteuses".to_string(),
        skey: format!("s{}", Uuid::new_v4().simple()),
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
        .expect("create_space failed");
    id
}

/// Stores an invite and returns its token.
async fn create_invite(app: &TestApp, space_id: &str, max_uses: i64) -> String {
    let token = Uuid::new_v4().simple().to_string();
    let invite = SpaceInvite {
        id: Uuid::new_v4().to_string(),
        space_id: space_id.to_string(),
        token_hash: hex::encode(Sha256::digest(token.as_bytes())),
        created_by: AUTHORITY.to_string(),
        access: MemberAccess::READ,
        max_uses: Some(max_uses),
        uses: 0,
        expires_at: None,
        revoked: false,
        created_at: now_rfc3339(),
    };
    spaces_db::create_invite(&app.state.db, app.state.db_backend, &invite)
        .await
        .expect("create_invite failed");
    token
}

/// Sends one accept per DID, all released together, and returns their statuses.
async fn accept_all(app: &TestApp, token: &str, dids: Vec<String>) -> Vec<StatusCode> {
    let barrier = Arc::new(Barrier::new(dids.len()));
    let tasks: Vec<_> = dids
        .into_iter()
        .map(|did| {
            let router = app.router.clone();
            let (name, value) = common::auth::admin_cookie_header(&did, &app.state.cookie_key);
            let body = json!({ "token": token }).to_string();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                let req = Request::builder()
                    .method("POST")
                    .uri("/xrpc/dev.happyview.space.acceptInvite")
                    .header(name, value)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap();
                barrier.wait().await;
                router.oneshot(req).await.unwrap().status()
            })
        })
        .collect();
    let mut statuses = Vec::new();
    for task in tasks {
        statuses.push(task.await.unwrap());
    }
    statuses
}

async fn uses_of(app: &TestApp, token: &str) -> i64 {
    let hash = hex::encode(Sha256::digest(token.as_bytes()));
    spaces_db::get_invite_by_token_hash(&app.state.db, app.state.db_backend, &hash)
        .await
        .unwrap()
        .expect("invite vanished")
        .uses
}

async fn member_count(app: &TestApp, space_id: &str) -> i64 {
    let sql = adapt_sql(
        "SELECT COUNT(*) FROM happyview_space_members WHERE space_id = ?",
        app.state.db_backend,
    );
    let (count,): (i64,) = happyview::db::query_as(&sql)
        .bind(space_id)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    count
}

fn count(statuses: &[StatusCode], status: StatusCode) -> usize {
    statuses.iter().filter(|s| **s == status).count()
}

/// Concurrent accepts never take more uses than an invite has, and a member
/// accepting twice at once joins once and takes one use.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[serial]
async fn concurrent_accepts_stay_within_max_uses() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    const MAX: i64 = 2;
    let space = create_space(&app).await;
    let token = create_invite(&app, &space, MAX).await;
    let dids = (0..16)
        .map(|i| format!("did:plc:inviteuses{i}{}", Uuid::new_v4().simple()))
        .collect();
    let statuses = accept_all(&app, &token, dids).await;
    assert_eq!(
        count(&statuses, StatusCode::CREATED),
        MAX as usize,
        "{statuses:?}"
    );
    assert_eq!(
        count(&statuses, StatusCode::BAD_REQUEST),
        16 - MAX as usize,
        "{statuses:?}"
    );
    assert_eq!(uses_of(&app, &token).await, MAX);
    assert_eq!(member_count(&app, &space).await, MAX);

    let space = create_space(&app).await;
    let token = create_invite(&app, &space, 5).await;
    let did = format!("did:plc:inviteusestwice{}", Uuid::new_v4().simple());
    let statuses = accept_all(&app, &token, vec![did; 8]).await;
    assert_eq!(count(&statuses, StatusCode::CREATED), 1, "{statuses:?}");
    assert_eq!(count(&statuses, StatusCode::CONFLICT), 7, "{statuses:?}");
    assert_eq!(uses_of(&app, &token).await, 1);
    assert_eq!(member_count(&app, &space).await, 1);
}
