mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;

async fn json_body(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

// ---------------------------------------------------------------------------
// Service entry CRUD via admin endpoints
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn create_list_update_delete_service_entry() {
    common::require_db!();
    let app = TestApp::new().await;
    let cookie = app.admin_cookie();

    // CREATE
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/service-entries")
                .header(cookie.0.clone(), cookie.1.clone())
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "fragment_id": "#chess",
                        "service_type": "ChessAppView"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);
    let created = json_body(resp).await;
    let entry_id = created["id"].as_i64().unwrap();
    assert_eq!(created["fragment_id"], "#chess");
    assert_eq!(created["service_type"], "ChessAppView");
    assert_eq!(created["access_mode"], "all");

    // LIST
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/service-entries")
                .header(cookie.0.clone(), cookie.1.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let list = json_body(resp).await;
    let entries = list.as_array().unwrap();
    assert!(entries.iter().any(|e| e["id"].as_i64() == Some(entry_id)));

    // UPDATE
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/admin/service-entries/{}", entry_id))
                .header(cookie.0.clone(), cookie.1.clone())
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "access_mode": "specific"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // DELETE
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/admin/service-entries/{}", entry_id))
                .header(cookie.0.clone(), cookie.1.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Verify deletion
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/service-entries")
                .header(cookie.0.clone(), cookie.1.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let list = json_body(resp).await;
    let entries = list.as_array().unwrap();
    assert!(!entries.iter().any(|e| e["id"].as_i64() == Some(entry_id)));
}

#[tokio::test]
#[serial]
async fn delete_nonexistent_entry_returns_404() {
    common::require_db!();
    let app = TestApp::new().await;
    let cookie = app.admin_cookie();

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/admin/service-entries/99999")
                .header(cookie.0, cookie.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[serial]
async fn update_entry_with_invalid_access_mode_returns_400() {
    common::require_db!();
    let app = TestApp::new().await;
    let cookie = app.admin_cookie();

    let entry_id = app
        .create_service_entry("#chess", "ChessAppView", "all")
        .await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/admin/service-entries/{}", entry_id))
                .header(cookie.0, cookie.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "access_mode": "invalid_mode"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "invalid access_mode should return 400"
    );
}

// ---------------------------------------------------------------------------
// XRPC junction table via admin endpoints
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn add_list_remove_entry_xrpcs() {
    common::require_db!();
    let app = TestApp::new().await;
    let cookie = app.admin_cookie();

    let entry_id = app
        .create_service_entry("#chess", "ChessAppView", "specific")
        .await;

    // ADD xrpcs
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/admin/service-entries/{}/xrpcs", entry_id))
                .header(cookie.0.clone(), cookie.1.clone())
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "lexicon_ids": ["games.example.listGames", "games.example.getGame"]
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // LIST xrpcs
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/admin/service-entries/{}/xrpcs", entry_id))
                .header(cookie.0.clone(), cookie.1.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let xrpcs = json_body(resp).await;
    let list = xrpcs.as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert!(list.contains(&json!("games.example.getGame")));
    assert!(list.contains(&json!("games.example.listGames")));

    // REMOVE one xrpc
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/admin/service-entries/{}/xrpcs", entry_id))
                .header(cookie.0.clone(), cookie.1.clone())
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "lexicon_ids": ["games.example.getGame"]
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Verify removal
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/admin/service-entries/{}/xrpcs", entry_id))
                .header(cookie.0.clone(), cookie.1.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let xrpcs = json_body(resp).await;
    let list = xrpcs.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0], "games.example.listGames");
}

// ---------------------------------------------------------------------------
// Reverse lookup: services_for_lexicon
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn lexicon_services_reverse_lookup() {
    common::require_db!();
    let app = TestApp::new().await;
    let cookie = app.admin_cookie();

    let id_all = app
        .create_service_entry("#chess", "ChessAppView", "all")
        .await;
    let id_specific = app
        .create_service_entry("#checkers", "CheckersAppView", "specific")
        .await;
    app.add_entry_xrpcs(id_specific, &["games.example.listGames"])
        .await;

    // Both should appear for games.example.listGames (one via access_mode=all, one via junction)
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/lexicons/games.example.listGames/services")
                .header(cookie.0.clone(), cookie.1.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let services = json_body(resp).await;
    let list = services.as_array().unwrap();
    assert_eq!(list.len(), 2);
    let ids: Vec<i64> = list.iter().filter_map(|e| e["id"].as_i64()).collect();
    assert!(ids.contains(&id_all));
    assert!(ids.contains(&id_specific));

    // Only #chess (access_mode=all) should appear for a random XRPC
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/lexicons/games.example.unrelated/services")
                .header(cookie.0.clone(), cookie.1.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let services = json_body(resp).await;
    let list = services.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"].as_i64().unwrap(), id_all);
}
