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
    seed_space_with_skey(app, "main").await
}

/// Like `seed_space`, but with a distinct id and record URI so more than one
/// space can be seeded in the same test.
async fn seed_space_with_skey(app: &TestApp, skey: &str) -> String {
    let now = now_rfc3339();
    let id = Uuid::new_v4().to_string();
    let space = Space {
        id: id.clone(),
        did: CREATOR.to_string(),
        authority_did: CREATOR.to_string(),
        creator_did: CREATOR.to_string(),
        type_nsid: "com.example.adminspaces".to_string(),
        skey: skey.to_string(),
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
                "at://{CREATOR}/space/com.example.adminspaces/{skey}/{MEMBER}/{COLLECTION}/1"
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

async fn enable_inspector(app: &TestApp) {
    let (name, value) = app.admin_cookie();
    let req = Request::builder()
        .method("PUT")
        .uri("/admin/settings/feature.space_inspector_enabled")
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(json!({ "value": "true" }).to_string()))
        .unwrap();
    assert_eq!(
        app.router.clone().oneshot(req).await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
}

async fn grant(app: &TestApp, scope: &str, target: &str) -> String {
    let (name, value) = app.admin_cookie();
    let req = Request::builder()
        .method("POST")
        .uri("/admin/spaces/access-grants")
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "scope": scope, "target": target, "reason": "report" }).to_string(),
        ))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    json_body(resp).await["id"].as_str().unwrap().to_string()
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

    enable_inspector(&app).await;
    grant(&app, "space", &id).await;
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
async fn inspect_reads_records_and_logs_the_read() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = seed_space(&app).await;
    enable_inspector(&app).await;
    let grant_id = grant(&app, "space", &id).await;
    let key = common::api_key(&app, &["spaces:inspect"]).await;

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
    assert_eq!(detail["grant_id"], grant_id);
    assert_eq!(detail["scope"], "space");
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

    enable_inspector(&app).await;
    grant(&app, "space", &id).await;
    let resp = get(
        &app,
        &format!("/admin/spaces/{id}/blob?cid=bafyunknown"),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert!(moderator_reads(&app).await.is_empty());
}

#[tokio::test]
#[serial]
async fn manage_records_alone_no_longer_reads_contents() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = seed_space(&app).await;
    enable_inspector(&app).await;
    let key = common::api_key(&app, &["spaces:manage-records"]).await;
    let resp = get(&app, &format!("/admin/spaces/{id}/records"), Some(&key)).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(json_body(resp).await["error"], "InsufficientPermissions");
}

#[tokio::test]
#[serial]
async fn reads_need_the_switch_and_a_grant() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = seed_space(&app).await;

    let resp = get(&app, &format!("/admin/spaces/{id}/records"), None).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(json_body(resp).await["error"], "SpaceInspectorDisabled");

    enable_inspector(&app).await;
    let resp = get(&app, &format!("/admin/spaces/{id}/records"), None).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(json_body(resp).await["error"], "SpaceAccessGrantRequired");
    assert!(moderator_reads(&app).await.is_empty());
}

#[tokio::test]
#[serial]
async fn disabling_the_switch_stops_existing_grants() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = seed_space(&app).await;
    enable_inspector(&app).await;
    grant(&app, "space", &id).await;

    let (name, value) = app.admin_cookie();
    let req = Request::builder()
        .method("DELETE")
        .uri("/admin/settings/feature.space_inspector_enabled")
        .header(name, value)
        .body(Body::empty())
        .unwrap();
    app.router.clone().oneshot(req).await.unwrap();

    let resp = get(&app, &format!("/admin/spaces/{id}/records"), None).await;
    assert_eq!(json_body(resp).await["error"], "SpaceInspectorDisabled");
}

#[tokio::test]
#[serial]
async fn expired_revoked_and_foreign_grants_do_not_cover() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = seed_space(&app).await;
    enable_inspector(&app).await;
    let now = chrono::Utc::now();
    let insert = adapt_sql(
        "INSERT INTO happyview_space_access_grants (id, user_id, user_did, scope, target, reason, created_at, expires_at, revoked_at) VALUES (?, ?, ?, 'space', ?, 'r', ?, ?, ?)",
        app.state.db_backend,
    );
    let admin_user_id: (String,) = happyview::db::query_as(&adapt_sql(
        "SELECT id FROM happyview_users WHERE did = ?",
        app.state.db_backend,
    ))
    .bind(&app.admin_did)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    let rows = [
        // expired
        (
            admin_user_id.0.clone(),
            (now - chrono::Duration::minutes(1)).to_rfc3339(),
            None,
        ),
        // revoked
        (
            admin_user_id.0.clone(),
            (now + chrono::Duration::minutes(60)).to_rfc3339(),
            Some(now.to_rfc3339()),
        ),
        // someone else's
        (
            "someone-else".to_string(),
            (now + chrono::Duration::minutes(60)).to_rfc3339(),
            None,
        ),
    ];
    for (user_id, expires_at, revoked_at) in rows {
        happyview::db::query(&insert)
            .bind(Uuid::new_v4().to_string())
            .bind(user_id)
            .bind("did:plc:x")
            .bind(&id)
            .bind(now.to_rfc3339())
            .bind(expires_at)
            .bind(revoked_at)
            .execute(&app.state.db)
            .await
            .unwrap();
    }
    let resp = get(&app, &format!("/admin/spaces/{id}/records"), None).await;
    assert_eq!(json_body(resp).await["error"], "SpaceAccessGrantRequired");
}

#[tokio::test]
#[serial]
async fn space_grant_covers_only_its_space() {
    common::require_db!();
    let app = TestApp::new().await;
    let first = seed_space(&app).await;
    let second = seed_space_with_skey(&app, "second").await;
    enable_inspector(&app).await;
    grant(&app, "space", &first).await;
    assert_eq!(
        get(&app, &format!("/admin/spaces/{first}/records"), None)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        get(&app, &format!("/admin/spaces/{second}/records"), None)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
#[serial]
async fn account_grant_reads_only_that_author_in_a_space() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = seed_space(&app).await;
    enable_inspector(&app).await;
    grant(&app, "account", MEMBER).await;
    assert_eq!(
        get(
            &app,
            &format!("/admin/spaces/{id}/records?repo={MEMBER}"),
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        get(&app, &format!("/admin/spaces/{id}/records"), None)
            .await
            .status(),
        StatusCode::FORBIDDEN,
        "an account grant does not open every author in the space"
    );
    assert_eq!(
        get(
            &app,
            &format!("/admin/spaces/{id}/records?repo={CREATOR}"),
            None
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
#[serial]
async fn grant_reads_lists_the_reads_made_under_a_grant() {
    common::require_db!();
    let app = TestApp::new().await;
    let id = seed_space(&app).await;
    enable_inspector(&app).await;
    let grant_id = grant(&app, "space", &id).await;
    get(&app, &format!("/admin/spaces/{id}/records"), None).await;
    let resp = get(
        &app,
        &format!("/admin/spaces/access-grants/{grant_id}/reads"),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["events"].as_array().unwrap().len(), 1);
    assert_eq!(body["events"][0]["detail"]["action"], "list_records");
}
