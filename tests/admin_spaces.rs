mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::db::{adapt_sql, now_rfc3339};
use happyview::spaces::db as spaces_db;
use happyview::spaces::types::*;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

const CREATOR: &str = "did:plc:adminspaces-creator";
const MEMBER: &str = "did:plc:adminspaces-member";
const COLLECTION: &str = "com.example.adminspaces.post";

async fn json_body(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap_or(json!(null))
}

async fn get(app: &TestApp, uri: &str, key: Option<&str>) -> axum::response::Response {
    let mut req = Request::builder().uri(uri);
    req = match key {
        Some(key) => req.header("authorization", format!("Bearer {key}")),
        None => {
            let (name, value) = app.admin_cookie();
            req.header(name, value)
        }
    };
    app.router
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

/// A space the test admin neither created nor belongs to, holding one record.
async fn seed_space(app: &TestApp) -> String {
    let now = now_rfc3339();
    let id = Uuid::new_v4().to_string();
    let space = Space {
        id: id.clone(),
        did: CREATOR.to_string(),
        authority_did: CREATOR.to_string(),
        creator_did: CREATOR.to_string(),
        type_nsid: "com.example.adminspaces".to_string(),
        skey: "main".to_string(),
        display_name: Some("Private space".to_string()),
        description: None,
        read_policy: Policy::MemberList,
        write_policy: Policy::MemberList,
        app_access: AppAccess::Open,
        config: SpaceConfig::default(),
        revision: None,
        created_at: now.clone(),
        updated_at: now.clone(),
    };
    spaces_db::create_space(&app.state.db, app.state.db_backend, &space)
        .await
        .unwrap();
    spaces_db::add_member(
        &app.state.db,
        app.state.db_backend,
        &SpaceMember {
            id: Uuid::new_v4().to_string(),
            space_id: id.clone(),
            did: MEMBER.to_string(),
            access: MemberAccess::WRITE,
            is_delegation: false,
            granted_by: Some(CREATOR.to_string()),
            created_at: now.clone(),
        },
    )
    .await
    .unwrap();
    spaces_db::upsert_space_record(
        &app.state.db,
        app.state.db_backend,
        &SpaceRecord {
            uri: format!(
                "at://{CREATOR}/space/com.example.adminspaces/main/{MEMBER}/{COLLECTION}/1"
            ),
            space_id: id.clone(),
            author_did: MEMBER.to_string(),
            collection: COLLECTION.to_string(),
            rkey: "1".to_string(),
            record: json!({ "$type": COLLECTION, "text": "hello" }),
            cid: "bafyreiadminspaces".to_string(),
            indexed_at: now,
        },
    )
    .await
    .unwrap();
    id
}

async fn moderator_reads(app: &TestApp) -> Vec<(Option<String>, Option<String>, String)> {
    let sql = adapt_sql(
        "SELECT actor_did, subject, detail FROM happyview_event_logs WHERE event_type = ?",
        app.state.db_backend,
    );
    happyview::db::query_as(&sql)
        .bind("space.moderator_read")
        .fetch_all(&app.state.db)
        .await
        .unwrap()
}

#[tokio::test]
#[serial]
async fn super_admin_lists_and_reads_spaces() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = seed_space(&app).await;

    let resp = get(&app, "/admin/spaces", None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["spaces"][0]["id"], id);
    assert_eq!(
        body["spaces"][0]["uri"],
        "at://did:plc:adminspaces-creator/space/com.example.adminspaces/main"
    );

    let resp = get(&app, &format!("/admin/spaces/{id}"), None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["members"][0]["did"], MEMBER);
    assert_eq!(body["collections"][0]["collection"], COLLECTION);
    assert_eq!(body["collections"][0]["count"], 1);

    let resp = get(&app, &format!("/admin/spaces/{id}/records"), None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["records"][0]["record"]["text"], "hello");
    assert_eq!(body["records"][0]["did"], MEMBER);
}

#[tokio::test]
#[serial]
async fn spaces_read_does_not_grant_record_access() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = seed_space(&app).await;
    let key = common::api_key(&app, &["spaces:read"]).await;

    let resp = get(&app, &format!("/admin/spaces/{id}"), Some(&key)).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = get(&app, &format!("/admin/spaces/{id}/records"), Some(&key)).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let resp = get(
        &app,
        &format!("/admin/spaces/{id}/blob?cid=bafy"),
        Some(&key),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(moderator_reads(&app).await.is_empty());
}

#[tokio::test]
#[serial]
async fn manage_records_reads_records_and_logs_the_read() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = seed_space(&app).await;
    let key = common::api_key(&app, &["spaces:manage-records"]).await;

    let resp = get(
        &app,
        &format!("/admin/spaces/{id}/records?collection={COLLECTION}"),
        Some(&key),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        json_body(resp).await["records"].as_array().unwrap().len(),
        1
    );

    let reads = moderator_reads(&app).await;
    assert_eq!(reads.len(), 1);
    let (actor, subject, detail) = &reads[0];
    assert_eq!(actor.as_deref(), Some(app.admin_did.as_str()));
    assert_eq!(
        subject.as_deref(),
        Some("at://did:plc:adminspaces-creator/space/com.example.adminspaces/main")
    );
    let detail: Value = serde_json::from_str(detail).unwrap();
    assert_eq!(detail["action"], "list_records");
    assert_eq!(detail["collection"], COLLECTION);
    assert_eq!(detail["space_id"], id);
}

#[tokio::test]
#[serial]
async fn metadata_reads_are_not_logged() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = seed_space(&app).await;

    assert_eq!(
        get(&app, "/admin/spaces", None).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        get(&app, &format!("/admin/spaces/{id}"), None)
            .await
            .status(),
        StatusCode::OK
    );
    assert!(moderator_reads(&app).await.is_empty());
}

#[tokio::test]
#[serial]
async fn unknown_space_is_not_found() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = get(&app, "/admin/spaces/does-not-exist", None).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let resp = get(&app, "/admin/spaces/does-not-exist/records", None).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[serial]
async fn blob_outside_the_space_is_not_found() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = seed_space(&app).await;

    let resp = get(
        &app,
        &format!("/admin/spaces/{id}/blob?cid=bafyunknown"),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert!(moderator_reads(&app).await.is_empty());
}
