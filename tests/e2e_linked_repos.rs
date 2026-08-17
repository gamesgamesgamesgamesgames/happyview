mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::app::TestApp;
use happyview::linked_repos::{db, types};
use http_body_util::BodyExt;
use serial_test::serial;
use tower::ServiceExt;

#[tokio::test]
#[serial]
async fn create_and_list_grants() {
    common::require_db!();
    let app = TestApp::new().await;

    let grant = db::create(
        &app.state,
        None,
        None,
        Some("mirror target"),
        "atproto repo:com.example.note?action=create",
        "did:plc:admin",
    )
    .await
    .unwrap();

    assert_eq!(grant.status, types::STATUS_PENDING);
    assert!(grant.did.is_none());
    assert_eq!(grant.reason.as_deref(), Some("mirror target"));

    let all = db::list(&app.state).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, grant.id);
}

#[tokio::test]
#[serial]
async fn bind_did_activates_grant() {
    common::require_db!();
    let app = TestApp::new().await;

    let grant = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();

    db::bind_did(&app.state, &grant.id, "did:plc:target", Some("target.test"))
        .await
        .unwrap();

    let reloaded = db::get(&app.state, &grant.id).await.unwrap().unwrap();
    assert_eq!(reloaded.status, types::STATUS_ACTIVE);
    assert_eq!(reloaded.did.as_deref(), Some("did:plc:target"));
    assert_eq!(reloaded.handle.as_deref(), Some("target.test"));
    assert!(reloaded.authorized_at.is_some());

    let by_did = db::get_by_did(&app.state, "did:plc:target").await.unwrap();
    assert_eq!(by_did.unwrap().id, grant.id);
}

#[tokio::test]
#[serial]
async fn mark_needs_reauth_records_error() {
    common::require_db!();
    let app = TestApp::new().await;

    let grant = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();
    db::bind_did(&app.state, &grant.id, "did:plc:target", None)
        .await
        .unwrap();

    db::mark_needs_reauth(&app.state, &grant.id, "refresh failed: invalid_grant")
        .await
        .unwrap();

    let reloaded = db::get(&app.state, &grant.id).await.unwrap().unwrap();
    assert_eq!(reloaded.status, types::STATUS_NEEDS_REAUTH);
    assert_eq!(
        reloaded.last_error.as_deref(),
        Some("refresh failed: invalid_grant")
    );
}

#[tokio::test]
#[serial]
async fn delete_removes_grant_and_session() {
    common::require_db!();
    let app = TestApp::new().await;

    let grant = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();
    db::bind_did(&app.state, &grant.id, "did:plc:target", None)
        .await
        .unwrap();

    // Seed a session row for the bound DID.
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_linked_repo_sessions (did, session_data, updated_at) VALUES (?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind("did:plc:target")
        .bind("{}")
        .bind(happyview::db::now_rfc3339())
        .execute(&app.state.db)
        .await
        .unwrap();

    assert!(db::delete(&app.state, &grant.id).await.unwrap());

    assert!(db::get(&app.state, &grant.id).await.unwrap().is_none());

    let count_sql = happyview::db::adapt_sql(
        "SELECT COUNT(*) FROM happyview_linked_repo_sessions WHERE did = ?",
        app.state.db_backend,
    );
    let (count,): (i64,) = happyview::db::query_as(&count_sql)
        .bind("did:plc:target")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(count, 0, "deleting a grant must delete its session");
}

#[tokio::test]
#[serial]
async fn all_scopes_returns_every_grants_scopes() {
    common::require_db!();
    let app = TestApp::new().await;

    db::create(
        &app.state,
        None,
        None,
        None,
        "repo:com.example.a",
        "did:plc:admin",
    )
    .await
    .unwrap();
    db::create(
        &app.state,
        None,
        None,
        None,
        "repo:com.example.b",
        "did:plc:admin",
    )
    .await
    .unwrap();

    let mut scopes = db::all_scopes(&app.state).await.unwrap();
    scopes.sort();
    assert_eq!(scopes, vec!["repo:com.example.a", "repo:com.example.b"]);
}

#[tokio::test]
#[serial]
async fn linked_sessions_are_isolated_from_dashboard_sessions() {
    common::require_db!();
    let app = TestApp::new().await;

    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_oauth_sessions (did, session_data, updated_at) VALUES (?, ?, datetime('now'))",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind("did:plc:shared")
        .bind(r#"{"marker":"dashboard"}"#)
        .execute(&app.state.db)
        .await
        .unwrap();

    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_linked_repo_sessions (did, session_data, updated_at) VALUES (?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind("did:plc:shared")
        .bind(r#"{"marker":"linked"}"#)
        .bind(happyview::db::now_rfc3339())
        .execute(&app.state.db)
        .await
        .unwrap();

    // Neither table clobbers the other.
    let read = |table: &str| {
        let sql = happyview::db::adapt_sql(
            &format!("SELECT session_data FROM {table} WHERE did = ?"),
            app.state.db_backend,
        );
        let pool = app.state.db.clone();
        async move {
            let (data,): (String,) = happyview::db::query_as(&sql)
                .bind("did:plc:shared")
                .fetch_one(&pool)
                .await
                .unwrap();
            data
        }
    };

    assert!(read("happyview_oauth_sessions").await.contains("dashboard"));
    assert!(
        read("happyview_linked_repo_sessions")
            .await
            .contains("linked")
    );
}

#[tokio::test]
#[serial]
async fn app_state_exposes_a_linked_repos_client() {
    common::require_db!();
    let app = TestApp::new().await;
    // Shares the primary client's identity; only the session store differs.
    assert_eq!(
        app.state.linked_repos_client.client_metadata.client_id,
        app.state.oauth.primary_client().client_metadata.client_id,
    );
}

async fn fetch_client_metadata(app: &TestApp) -> serde_json::Value {
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/oauth-client-metadata.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

fn primary_client_scopes(app: &TestApp) -> Vec<String> {
    let client = app.state.oauth.get_for_domain("http://127.0.0.1:0");
    let scopes = happyview::linked_repos::scope::client_base_scopes(
        client.client_metadata.scope.as_deref(),
        &client.client_metadata.client_id,
    );
    // A loopback client_id must still yield the scopes it was built with —
    // anchor on one so a silently-empty base can't make this vacuous.
    assert!(
        scopes.split_whitespace().any(|s| s == "atproto"),
        "the primary client must hold atproto, got {scopes:?}"
    );
    scopes.split_whitespace().map(String::from).collect()
}

fn scope_tokens(scope: &str) -> Vec<String> {
    let mut tokens: Vec<String> = scope.split_whitespace().map(String::from).collect();
    tokens.sort();
    tokens
}

#[tokio::test]
#[serial]
async fn client_metadata_advertises_base_scopes_with_no_grants() {
    common::require_db!();
    let app = TestApp::new().await;
    let metadata = fetch_client_metadata(&app).await;
    let scope = metadata["scope"].as_str().unwrap();

    let mut expected = primary_client_scopes(&app);
    expected.sort();
    assert!(!expected.is_empty(), "the client must advertise something");
    // With no grants the union is exactly the client's own scopes — no more
    // (nothing invented) and no fewer (nothing dropped).
    assert_eq!(scope_tokens(scope), expected);
}

#[tokio::test]
#[serial]
async fn client_metadata_includes_grant_scopes() {
    common::require_db!();
    let app = TestApp::new().await;

    db::create(
        &app.state,
        None,
        None,
        None,
        "repo:com.example.note?action=create blob:image/*",
        "did:plc:admin",
    )
    .await
    .unwrap();

    let metadata = fetch_client_metadata(&app).await;
    let scope = metadata["scope"].as_str().unwrap();
    let advertised = scope_tokens(scope);
    assert!(
        advertised
            .iter()
            .any(|s| s == "repo:com.example.note?action=create")
    );
    assert!(advertised.iter().any(|s| s == "blob:image/*"));

    for base in primary_client_scopes(&app) {
        assert!(
            advertised.contains(&base),
            "primary client scope {base} must survive the union, got {scope}"
        );
    }
}

#[tokio::test]
#[serial]
async fn deleting_a_grant_drops_its_scopes_from_the_union() {
    common::require_db!();
    let app = TestApp::new().await;

    let a = db::create(
        &app.state,
        None,
        None,
        None,
        "repo:com.example.a",
        "did:plc:admin",
    )
    .await
    .unwrap();
    db::create(
        &app.state,
        None,
        None,
        None,
        "repo:com.example.shared",
        "did:plc:admin",
    )
    .await
    .unwrap();

    let metadata = fetch_client_metadata(&app).await;
    assert!(
        metadata["scope"]
            .as_str()
            .unwrap()
            .contains("repo:com.example.a")
    );

    db::delete(&app.state, &a.id).await.unwrap();

    let metadata = fetch_client_metadata(&app).await;
    assert!(
        !metadata["scope"]
            .as_str()
            .unwrap()
            .contains("repo:com.example.a")
    );
}

#[tokio::test]
#[serial]
async fn shared_scopes_survive_deleting_one_grant() {
    common::require_db!();
    let app = TestApp::new().await;

    let a = db::create(
        &app.state,
        None,
        None,
        None,
        "repo:com.example.shared",
        "did:plc:admin",
    )
    .await
    .unwrap();
    db::create(
        &app.state,
        None,
        None,
        None,
        "repo:com.example.shared",
        "did:plc:admin",
    )
    .await
    .unwrap();

    db::delete(&app.state, &a.id).await.unwrap();

    let metadata = fetch_client_metadata(&app).await;
    assert!(
        metadata["scope"]
            .as_str()
            .unwrap()
            .contains("repo:com.example.shared"),
        "a scope another grant still needs must stay advertised"
    );
}

fn admin_post(
    uri: &str,
    cookie: (axum::http::HeaderName, axum::http::HeaderValue),
    body: &serde_json::Value,
) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(cookie.0, cookie.1)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

async fn json_body(resp: axum::response::Response) -> serde_json::Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
#[serial]
async fn admin_creates_and_lists_a_grant() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/linked-repos",
            app.admin_cookie(),
            &serde_json::json!({
                "reason": "mirror target",
                "scopes": "atproto repo:com.example.note?action=create",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let created = json_body(resp).await;
    assert_eq!(created["status"], "pending");
    assert!(created["did"].is_null());

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/linked-repos")
                .header(app.admin_cookie().0, app.admin_cookie().1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let listed = json_body(resp).await;
    assert_eq!(listed["linked_repos"].as_array().unwrap().len(), 1);
}

#[tokio::test]
#[serial]
async fn admin_create_adds_atproto_to_a_scope_set_without_it() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/linked-repos",
            app.admin_cookie(),
            &serde_json::json!({ "scopes": "repo:com.example.note?action=create" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let created = json_body(resp).await;
    let scopes = created["scopes"].as_str().unwrap();
    assert!(
        scopes.split_whitespace().any(|s| s == "atproto"),
        "stored scopes must include atproto, got {scopes}"
    );

    let stored = db::get(&app.state, created["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(stored.scopes.split_whitespace().any(|s| s == "atproto"));
    assert!(
        stored
            .scopes
            .split_whitespace()
            .any(|s| s == "repo:com.example.note?action=create"),
        "the requested scopes must survive normalization, got {}",
        stored.scopes
    );

    let metadata = fetch_client_metadata(&app).await;
    let advertised = scope_tokens(metadata["scope"].as_str().unwrap());
    for s in stored.scopes.split_whitespace() {
        assert!(advertised.contains(&s.to_string()), "{s} not advertised");
    }
}

#[tokio::test]
#[serial]
async fn admin_create_does_not_duplicate_atproto() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/linked-repos",
            app.admin_cookie(),
            &serde_json::json!({ "scopes": "atproto repo:com.example.note" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let created = json_body(resp).await;
    let scopes = created["scopes"].as_str().unwrap();
    assert_eq!(
        scopes
            .split_whitespace()
            .filter(|s| *s == "atproto")
            .count(),
        1,
        "atproto must appear exactly once, got {scopes}"
    );
    assert_eq!(scopes, "atproto repo:com.example.note");
}

#[tokio::test]
#[serial]
async fn admin_rejects_invalid_scopes() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/linked-repos",
            app.admin_cookie(),
            &serde_json::json!({ "scopes": "repo:com.example.note?action=frobnicate" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[serial]
async fn admin_rejects_empty_scopes() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/linked-repos",
            app.admin_cookie(),
            &serde_json::json!({ "scopes": "   " }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[serial]
async fn admin_deletes_a_grant() {
    common::require_db!();
    let app = TestApp::new().await;

    let grant = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/admin/linked-repos/{}", grant.id))
                .header(app.admin_cookie().0, app.admin_cookie().1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(db::get(&app.state, &grant.id).await.unwrap().is_none());
}

#[tokio::test]
#[serial]
async fn admin_list_requires_auth() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/linked-repos")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[serial]
async fn complete_binds_an_open_grant() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::flow;

    let grant = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();

    flow::complete(&app.state, &grant.id, "did:plc:whoever")
        .await
        .unwrap();

    let reloaded = db::get(&app.state, &grant.id).await.unwrap().unwrap();
    assert_eq!(reloaded.did.as_deref(), Some("did:plc:whoever"));
    assert_eq!(reloaded.status, types::STATUS_ACTIVE);
}

#[tokio::test]
#[serial]
async fn complete_rejects_a_did_mismatch_on_a_pinned_grant() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::flow;

    let grant = db::create(
        &app.state,
        Some("did:plc:expected"),
        Some("expected.test"),
        None,
        "atproto",
        "did:plc:admin",
    )
    .await
    .unwrap();

    let err = flow::complete(&app.state, &grant.id, "did:plc:attacker")
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        !msg.contains("did:plc:expected") && !msg.contains("did:plc:attacker"),
        "refusal must not disclose either DID, got: {msg}"
    );
    assert!(msg.contains("different account"), "got: {msg}");

    let reloaded = db::get(&app.state, &grant.id).await.unwrap().unwrap();
    assert_eq!(reloaded.did.as_deref(), Some("did:plc:expected"));
    assert_eq!(
        reloaded.status,
        types::STATUS_PENDING,
        "a rejected completion must not activate the grant"
    );
}

#[tokio::test]
#[serial]
async fn complete_rejects_an_already_linked_did() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::flow;

    let first = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();
    flow::complete(&app.state, &first.id, "did:plc:target")
        .await
        .unwrap();

    let second = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();
    let err = flow::complete(&app.state, &second.id, "did:plc:target")
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("already linked"), "got: {msg}");
    assert!(
        !msg.contains(&first.id),
        "refusal must not disclose another grant's id, got: {msg}"
    );

    let reloaded = db::get(&app.state, &second.id).await.unwrap().unwrap();
    assert_eq!(reloaded.status, types::STATUS_PENDING);
}

#[tokio::test]
#[serial]
async fn a_refused_completion_flags_the_grant_whose_session_it_clobbered() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::flow;

    let first = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();
    flow::complete(&app.state, &first.id, "did:plc:target")
        .await
        .unwrap();
    assert_eq!(
        db::get(&app.state, &first.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        types::STATUS_ACTIVE
    );

    let second = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();
    flow::complete(&app.state, &second.id, "did:plc:target")
        .await
        .unwrap_err();

    let reloaded = db::get(&app.state, &first.id).await.unwrap().unwrap();
    assert_eq!(
        reloaded.status,
        types::STATUS_NEEDS_REAUTH,
        "the grant that lost its session must not still read as active"
    );
    assert!(
        reloaded.last_error.is_some(),
        "the dashboard needs a reason to show"
    );
}

#[tokio::test]
#[serial]
async fn a_refused_pinned_completion_flags_the_interlopers_own_grant() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::flow;

    let theirs = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();
    flow::complete(&app.state, &theirs.id, "did:plc:interloper")
        .await
        .unwrap();

    let pinned = db::create(
        &app.state,
        Some("did:plc:expected"),
        None,
        None,
        "atproto",
        "did:plc:admin",
    )
    .await
    .unwrap();
    flow::complete(&app.state, &pinned.id, "did:plc:interloper")
        .await
        .unwrap_err();

    assert_eq!(
        db::get(&app.state, &theirs.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        types::STATUS_NEEDS_REAUTH
    );
}

#[tokio::test]
#[serial]
async fn invite_token_survives_lookup_and_dies_on_linking() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::flow;

    let grant = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();
    let token = flow::mint_invite_with_expiry(&app.state, &grant.id, None)
        .await
        .unwrap()
        .token;

    // Reading it, repeatedly, leaves it usable
    for _ in 0..3 {
        assert_eq!(
            flow::invite_grant_id(&app.state, &token).await.unwrap(),
            Some(grant.id.clone())
        );
        assert!(flow::invite_exists(&app.state, &token).await.unwrap());
    }

    // Linking the grant retires it
    flow::invalidate_invites_for_grant(&app.state, &grant.id)
        .await
        .unwrap();
    assert!(!flow::invite_exists(&app.state, &token).await.unwrap());
    assert_eq!(
        flow::invite_grant_id(&app.state, &token).await.unwrap(),
        None
    );
}

#[tokio::test]
#[serial]
async fn invite_endpoint_returns_a_url_once() {
    common::require_db!();
    let app = TestApp::new().await;

    let grant = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/linked-repos/{}/invite", grant.id),
            app.admin_cookie(),
            &serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    let url = body["invite_url"].as_str().unwrap();
    assert!(url.contains("/auth/linked-repo/start?token="));

    // The admin handing this link out needs to know when it stops working.
    let expires_at = body["expires_at"]
        .as_str()
        .expect("invite response must report an expiry");
    assert!(
        chrono::DateTime::parse_from_rfc3339(expires_at).is_ok(),
        "expires_at must be RFC3339, got: {expires_at}"
    );

    // The raw token must never appear in the list response.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/linked-repos")
                .header(app.admin_cookie().0, app.admin_cookie().1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let listed = serde_json::to_string(&json_body(resp).await).unwrap();
    let token = url.split("token=").nth(1).unwrap();
    assert!(
        !listed.contains(token),
        "list response leaked the invite token"
    );
}

#[tokio::test]
#[serial]
async fn a_bare_did_leaves_the_handle_unset() {
    common::require_db!();
    let app = TestApp::new().await;

    let resolved = happyview::linked_repos::flow::resolve_identifier(&app.state, "did:plc:pinned")
        .await
        .unwrap();
    assert_eq!(resolved.did, "did:plc:pinned");
    assert!(resolved.handle.is_none());

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/linked-repos",
            app.admin_cookie(),
            &serde_json::json!({ "handle": "did:plc:pinned", "scopes": "atproto" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let created = json_body(resp).await;
    assert_eq!(created["did"], "did:plc:pinned");
    assert!(
        created["handle"].is_null(),
        "a DID-pinned grant must not carry its own DID as a handle, got: {}",
        created["handle"]
    );
}

#[tokio::test]
#[serial]
async fn authorize_endpoint_refuses_an_open_grant() {
    common::require_db!();
    let app = TestApp::new().await;

    // No handle and no DID: there is nobody to authorize against, so the
    // endpoint must say so rather than start a flow for nobody.
    let grant = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/linked-repos/{}/authorize", grant.id),
            app.admin_cookie(),
            &serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[serial]
async fn invite_endpoint_rejects_an_unknown_grant() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/linked-repos/does-not-exist/invite",
            app.admin_cookie(),
            &serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[serial]
async fn start_prompts_for_a_handle_without_burning_the_invite() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::flow;

    let grant = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();
    let token = flow::mint_invite_with_expiry(&app.state, &grant.id, None)
        .await
        .unwrap()
        .token;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/auth/linked-repo/start?token={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        location.contains("/link/start?token="),
        "expected a redirect to the landing page, got: {location}"
    );
    assert!(
        location.contains(&urlencoding::encode(&token).to_string()),
        "the landing page must receive the token"
    );

    // Loading the prompt must not consume the invite.
    assert_eq!(
        flow::invite_grant_id(&app.state, &token).await.unwrap(),
        Some(grant.id.clone())
    );
}

#[tokio::test]
#[serial]
async fn start_rejects_an_unknown_token() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/auth/linked-repo/start?token=nope&handle=someone.test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        text.contains("this link is invalid, expired, or has already been used"),
        "expected the invite check to fail first, got {text}"
    );
    assert!(
        !text.contains("resolve handle"),
        "handle resolution ran before the invite check: {text}"
    );
}

#[tokio::test]
#[serial]
async fn discard_session_removes_a_linked_session() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::flow;

    happyview::db::query(&happyview::db::adapt_sql(
        "INSERT INTO happyview_linked_repo_sessions (did, session_data, updated_at) VALUES (?, ?, ?)",
        app.state.db_backend,
    ))
    .bind("did:plc:refused")
    .bind("{}")
    .bind("2026-01-01T00:00:00Z")
    .execute(&app.state.db)
    .await
    .unwrap();

    flow::discard_session(&app.state, "did:plc:refused")
        .await
        .unwrap();

    let remaining: Option<(String,)> = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT did FROM happyview_linked_repo_sessions WHERE did = ?",
        app.state.db_backend,
    ))
    .bind("did:plc:refused")
    .fetch_optional(&app.state.db)
    .await
    .unwrap();
    assert!(remaining.is_none(), "refused session must not survive");
}

async fn insert_auth_state(
    app: &TestApp,
    state_key: &str,
    grant_id: &str,
    token_hash: Option<&str>,
    expires_at: &str,
) {
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_linked_repo_auth_state (state, grant_id, token_hash, expires_at) \
         VALUES (?, ?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(state_key)
        .bind(grant_id)
        .bind(token_hash)
        .bind(expires_at)
        .execute(&app.state.db)
        .await
        .unwrap();
}

fn future_rfc3339() -> String {
    (chrono::Utc::now() + chrono::Duration::minutes(10)).to_rfc3339()
}

fn past_rfc3339() -> String {
    (chrono::Utc::now() - chrono::Duration::minutes(10)).to_rfc3339()
}

#[tokio::test]
#[serial]
async fn take_pending_grant_returns_the_grant_and_is_single_use() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::flow::{self, PendingGrant};

    let grant = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();
    insert_auth_state(&app, "state-live", &grant.id, None, &future_rfc3339()).await;

    assert_eq!(
        flow::take_pending_grant(&app.state, "state-live")
            .await
            .unwrap(),
        PendingGrant::Grant {
            grant_id: grant.id.clone(),
            origin: flow::AuthOrigin::Admin,
        }
    );
    // The DELETE is the claim, so a replay finds nothing at all.
    assert_eq!(
        flow::take_pending_grant(&app.state, "state-live")
            .await
            .unwrap(),
        PendingGrant::NotLinked
    );
}

#[tokio::test]
#[serial]
async fn take_pending_grant_reports_not_linked_for_a_dashboard_login() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::flow::{self, PendingGrant};

    let grant = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();

    insert_auth_state(
        &app,
        "invite:deadbeef",
        &grant.id,
        Some("deadbeef"),
        &future_rfc3339(),
    )
    .await;

    assert_eq!(
        flow::take_pending_grant(&app.state, "invite:deadbeef")
            .await
            .unwrap(),
        PendingGrant::NotLinked
    );
    assert_eq!(
        flow::take_pending_grant(&app.state, "never-seen-this")
            .await
            .unwrap(),
        PendingGrant::NotLinked
    );

    let (count,): (i64,) = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT COUNT(*) FROM happyview_linked_repo_auth_state WHERE state = ?",
        app.state.db_backend,
    ))
    .bind("invite:deadbeef")
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(count, 1, "an invite must not be consumed by a callback");
}

#[tokio::test]
#[serial]
async fn take_pending_grant_reports_expired_rather_than_not_linked() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::flow::{self, PendingGrant};

    let grant = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();
    insert_auth_state(&app, "state-stale", &grant.id, None, &past_rfc3339()).await;

    assert_eq!(
        flow::take_pending_grant(&app.state, "state-stale")
            .await
            .unwrap(),
        PendingGrant::Expired
    );
}

#[tokio::test]
#[serial]
async fn invite_lookup_rejects_an_expired_token() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::flow;

    let grant = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();
    let token = flow::mint_invite_with_expiry(&app.state, &grant.id, None)
        .await
        .unwrap()
        .token;

    happyview::db::query(&happyview::db::adapt_sql(
        "UPDATE happyview_linked_repo_auth_state SET expires_at = ? WHERE grant_id = ?",
        app.state.db_backend,
    ))
    .bind(past_rfc3339())
    .bind(&grant.id)
    .execute(&app.state.db)
    .await
    .unwrap();

    assert!(
        !flow::invite_exists(&app.state, &token).await.unwrap(),
        "an expired invite must not be usable"
    );
}

fn callback_request(code: &str, state: &str) -> Request<Body> {
    Request::builder()
        .uri(format!("/auth/callback?code={code}&state={state}"))
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
#[serial]
async fn an_expired_linked_state_never_reaches_the_dashboard_login_path() {
    common::require_db!();
    let app = TestApp::new().await;

    let grant = db::create(
        &app.state,
        None,
        None,
        None,
        "atproto repo:com.example.note?action=create",
        "did:plc:admin",
    )
    .await
    .unwrap();
    insert_auth_state(&app, "state-expired", &grant.id, None, &past_rfc3339()).await;

    let resp = app
        .router
        .clone()
        .oneshot(callback_request("bogus-code", "state-expired"))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(
        location.contains("/link/result?status=expired"),
        "got: {location}"
    );
    assert!(
        resp.headers().get("set-cookie").is_none(),
        "a linked-repo callback must never issue a dashboard session cookie"
    );
}

#[tokio::test]
#[serial]
async fn a_grant_deleted_mid_flight_never_reaches_the_dashboard_login_path() {
    common::require_db!();
    let app = TestApp::new().await;

    let grant = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();
    insert_auth_state(&app, "state-orphan", &grant.id, None, &future_rfc3339()).await;
    assert!(db::delete(&app.state, &grant.id).await.unwrap());

    let resp = app
        .router
        .clone()
        .oneshot(callback_request("bogus-code", "state-orphan"))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(
        location.contains("/link/result?status=gone"),
        "got: {location}"
    );
    assert!(
        resp.headers().get("set-cookie").is_none(),
        "a linked-repo callback must never issue a dashboard session cookie"
    );
}

#[tokio::test]
#[serial]
async fn the_linked_repo_callback_branch_issues_no_cookie() {
    common::require_db!();
    let app = TestApp::new().await;

    let grant = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();
    insert_auth_state(&app, "state-live", &grant.id, None, &future_rfc3339()).await;

    // A bogus code fails the token exchange, which is as far as a test can get
    // without a PDS — but far enough to prove nothing sets a session cookie.
    let resp = app
        .router
        .clone()
        .oneshot(callback_request("bogus-code", "state-live"))
        .await
        .unwrap();

    let location = resp
        .headers()
        .get("location")
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    assert!(
        location.contains("/link/result?status=failed"),
        "expected the failure result page, got status {} location {location:?}",
        resp.status()
    );
    assert!(
        resp.headers().get("set-cookie").is_none(),
        "the linked-repo branch must not touch the dashboard cookie jar"
    );
}

#[tokio::test]
#[serial]
async fn client_for_grant_is_deterministic_and_scope_specific() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::flow;

    let grant = db::create(
        &app.state,
        None,
        None,
        None,
        "atproto repo:com.example.note?action=create",
        "did:plc:admin",
    )
    .await
    .unwrap();

    let at_authorize = flow::client_for_grant(&app.state, &grant).unwrap();
    let reloaded = db::get(&app.state, &grant.id).await.unwrap().unwrap();
    let at_callback = flow::client_for_grant(&app.state, &reloaded).unwrap();
    assert_eq!(
        at_authorize.client_metadata.client_id, at_callback.client_metadata.client_id,
        "a grant must derive the same client on both sides of the flow"
    );

    let other = db::create(
        &app.state,
        None,
        None,
        None,
        "atproto repo:com.example.other?action=create",
        "did:plc:admin",
    )
    .await
    .unwrap();
    let other_client = flow::client_for_grant(&app.state, &other).unwrap();
    assert_ne!(
        at_authorize.client_metadata.client_id, other_client.client_metadata.client_id,
        "different scopes must derive different loopback client ids"
    );
}

#[tokio::test]
#[serial]
async fn start_rejects_a_url_shaped_handle_without_burning_the_invite() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::flow;

    // atrium treats any input starting with https:// as a PDS/entryway URL and
    // fetches it, so an unvalidated handle here is an SSRF primitive handed to
    // whoever holds the invite.
    let grant = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();
    let token = flow::mint_invite_with_expiry(&app.state, &grant.id, None)
        .await
        .unwrap()
        .token;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/auth/linked-repo/start?token={token}&handle=https%3A%2F%2Fexample.com%2F"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Rejected before anything was spent.
    assert_eq!(
        flow::invite_grant_id(&app.state, &token).await.unwrap(),
        Some(grant.id.clone())
    );
}

#[tokio::test]
#[serial]
async fn a_failed_authorization_leaves_the_invite_usable() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::flow;

    let grant = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();
    let token = flow::mint_invite_with_expiry(&app.state, &grant.id, None)
        .await
        .unwrap()
        .token;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/auth/linked-repo/start?token={token}&handle=did:plc:zzzzzzzzzzzzzzzzzzzzzzzz"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status().is_client_error() || resp.status().is_server_error(),
        "expected the authorization to fail, got {}",
        resp.status()
    );

    assert_eq!(
        flow::invite_grant_id(&app.state, &token).await.unwrap(),
        Some(grant.id.clone()),
        "a failed authorization must not burn the invite"
    );
}

#[tokio::test]
#[serial]
async fn admin_rejects_a_malformed_did() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/linked-repos",
            app.admin_cookie(),
            &serde_json::json!({ "handle": "did:nope", "scopes": "atproto" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[serial]
async fn refresh_marks_needs_reauth_when_no_session_exists() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::worker;

    let grant = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();
    db::bind_did(&app.state, &grant.id, "did:plc:ghost", None)
        .await
        .unwrap();
    let grant = db::get(&app.state, &grant.id).await.unwrap().unwrap();

    // No session row exists for did:plc:ghost, so restore must fail.
    assert!(worker::refresh_grant(&app.state, &grant).await.is_err());

    let reloaded = db::get(&app.state, &grant.id).await.unwrap().unwrap();
    assert_eq!(reloaded.status, types::STATUS_NEEDS_REAUTH);
    assert!(reloaded.last_error.is_some());
}

#[tokio::test]
#[serial]
async fn refresh_skips_grants_without_a_did() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::worker;

    let grant = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();

    // A pending, unbound grant is not an error — there is nothing to refresh.
    assert!(worker::refresh_grant(&app.state, &grant).await.is_ok());

    let reloaded = db::get(&app.state, &grant.id).await.unwrap().unwrap();
    assert_eq!(reloaded.status, types::STATUS_PENDING);
}

#[tokio::test]
#[serial]
async fn create_record_refuses_a_collection_outside_scope() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::pds;

    let grant = db::create(
        &app.state,
        None,
        None,
        None,
        "repo:com.example.allowed?action=create",
        "did:plc:admin",
    )
    .await
    .unwrap();
    db::bind_did(&app.state, &grant.id, "did:plc:target", None)
        .await
        .unwrap();
    let grant = db::get(&app.state, &grant.id).await.unwrap().unwrap();

    let err = pds::create_record(
        &app.state,
        &grant,
        "com.example.forbidden",
        None,
        serde_json::json!({ "text": "nope" }),
    )
    .await
    .unwrap_err();

    let msg = format!("{err}");
    assert!(msg.contains("com.example.forbidden"), "got: {msg}");
    assert!(
        msg.contains("repo:com.example.forbidden?action=create"),
        "got: {msg}"
    );
}

#[tokio::test]
#[serial]
async fn delete_record_refuses_when_only_create_is_granted() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::pds;

    let grant = db::create(
        &app.state,
        None,
        None,
        None,
        "repo:com.example.note?action=create",
        "did:plc:admin",
    )
    .await
    .unwrap();
    db::bind_did(&app.state, &grant.id, "did:plc:target", None)
        .await
        .unwrap();
    let grant = db::get(&app.state, &grant.id).await.unwrap().unwrap();

    let err = pds::delete_record(&app.state, &grant, "com.example.note", "abc")
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("action=delete"), "got: {err}");
}

#[tokio::test]
#[serial]
async fn upload_blob_refuses_an_unscoped_mime() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::pds;

    let grant = db::create(
        &app.state,
        None,
        None,
        None,
        "blob:image/*",
        "did:plc:admin",
    )
    .await
    .unwrap();
    db::bind_did(&app.state, &grant.id, "did:plc:target", None)
        .await
        .unwrap();
    let grant = db::get(&app.state, &grant.id).await.unwrap().unwrap();

    let err = pds::upload_blob(&app.state, &grant, "video/mp4", vec![0u8; 4])
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("video/mp4"), "got: {err}");
}

#[tokio::test]
#[serial]
async fn put_record_refuses_a_grant_scoped_to_update_only() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::pds;

    let grant = db::create(
        &app.state,
        None,
        None,
        None,
        "repo:com.example.note?action=update",
        "did:plc:admin",
    )
    .await
    .unwrap();
    db::bind_did(&app.state, &grant.id, "did:plc:target", None)
        .await
        .unwrap();
    let grant = db::get(&app.state, &grant.id).await.unwrap().unwrap();

    let err = pds::put_record(
        &app.state,
        &grant,
        "com.example.note",
        "abc",
        serde_json::json!({ "text": "nope" }),
        None,
    )
    .await
    .unwrap_err();

    let msg = format!("{err}");
    assert!(
        msg.contains("action=create") && msg.contains("action=update"),
        "expected the refusal to name both actions, got: {msg}"
    );
    assert!(
        msg.contains("putRecord can create"),
        "expected the refusal to explain why both are needed, got: {msg}"
    );
    assert!(
        msg.contains("swap_cid"),
        "expected the refusal to mention swap_cid as the way out, got: {msg}"
    );
}

#[tokio::test]
#[serial]
async fn put_record_passes_scope_check_with_both_create_and_update() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::pds;

    let grant = db::create(
        &app.state,
        None,
        None,
        None,
        "repo:com.example.note?action=create&action=update",
        "did:plc:admin",
    )
    .await
    .unwrap();
    db::bind_did(&app.state, &grant.id, "did:plc:target", None)
        .await
        .unwrap();
    let grant = db::get(&app.state, &grant.id).await.unwrap().unwrap();

    let err = pds::put_record(
        &app.state,
        &grant,
        "com.example.note",
        "abc",
        serde_json::json!({ "text": "ok" }),
        None,
    )
    .await
    .unwrap_err();

    let msg = format!("{err}");
    assert!(
        !msg.contains("lacks the scope"),
        "scope check should have passed, got: {msg}"
    );
    assert!(
        msg.contains("needs reauthorization"),
        "expected a session/auth failure, got: {msg}"
    );

    let reloaded = db::get(&app.state, &grant.id).await.unwrap().unwrap();
    assert_eq!(
        reloaded.status,
        types::STATUS_NEEDS_REAUTH,
        "reaching session restoration proves the scope pre-check passed"
    );
}

#[tokio::test]
#[serial]
async fn put_record_with_swap_cid_passes_scope_check_on_update_only_grant() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::pds;

    let grant = db::create(
        &app.state,
        None,
        None,
        None,
        "repo:com.example.note?action=update",
        "did:plc:admin",
    )
    .await
    .unwrap();
    db::bind_did(&app.state, &grant.id, "did:plc:target", None)
        .await
        .unwrap();
    let grant = db::get(&app.state, &grant.id).await.unwrap().unwrap();

    let err = pds::put_record(
        &app.state,
        &grant,
        "com.example.note",
        "abc",
        serde_json::json!({ "text": "ok" }),
        Some("bafyreiabc123"),
    )
    .await
    .unwrap_err();

    let msg = format!("{err}");
    assert!(
        !msg.contains("lacks the scope"),
        "scope check should have passed, got: {msg}"
    );
    assert!(
        msg.contains("needs reauthorization"),
        "expected a session/auth failure, got: {msg}"
    );

    let reloaded = db::get(&app.state, &grant.id).await.unwrap().unwrap();
    assert_eq!(
        reloaded.status,
        types::STATUS_NEEDS_REAUTH,
        "reaching session restoration proves the scope pre-check passed"
    );
}

#[tokio::test]
#[serial]
async fn put_record_with_swap_cid_still_refuses_a_create_only_grant() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::pds;

    let grant = db::create(
        &app.state,
        None,
        None,
        None,
        "repo:com.example.note?action=create",
        "did:plc:admin",
    )
    .await
    .unwrap();
    db::bind_did(&app.state, &grant.id, "did:plc:target", None)
        .await
        .unwrap();
    let grant = db::get(&app.state, &grant.id).await.unwrap().unwrap();

    let err = pds::put_record(
        &app.state,
        &grant,
        "com.example.note",
        "abc",
        serde_json::json!({ "text": "nope" }),
        Some("bafyreiabc123"),
    )
    .await
    .unwrap_err();

    let msg = format!("{err}");
    assert!(
        msg.contains("action=update"),
        "expected the refusal to name the update scope, got: {msg}"
    );
    assert!(
        msg.contains("swap_cid"),
        "expected the refusal to explain the swap_cid path, got: {msg}"
    );
}

#[tokio::test]
#[serial]
async fn pds_call_refuses_a_grant_in_needs_reauth() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::pds;

    let grant = db::create(
        &app.state,
        None,
        None,
        None,
        "repo:com.example.note?action=create",
        "did:plc:admin",
    )
    .await
    .unwrap();
    db::bind_did(&app.state, &grant.id, "did:plc:target", None)
        .await
        .unwrap();
    db::mark_needs_reauth(&app.state, &grant.id, "refresh failed: invalid_grant")
        .await
        .unwrap();
    let grant = db::get(&app.state, &grant.id).await.unwrap().unwrap();

    let err = pds::create_record(
        &app.state,
        &grant,
        "com.example.note",
        None,
        serde_json::json!({ "text": "nope" }),
    )
    .await
    .unwrap_err();

    let msg = format!("{err}");
    assert!(!msg.contains("lacks the scope"), "got: {msg}");
    assert!(msg.contains("needs reauthorization"), "got: {msg}");
}

#[tokio::test]
#[serial]
async fn pds_call_refuses_an_unbound_grant() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::pds;

    let grant = db::create(
        &app.state,
        None,
        None,
        None,
        "repo:com.example.note?action=create",
        "did:plc:admin",
    )
    .await
    .unwrap();

    let err = pds::create_record(
        &app.state,
        &grant,
        "com.example.note",
        None,
        serde_json::json!({ "text": "nope" }),
    )
    .await
    .unwrap_err();

    let msg = format!("{err}");
    assert!(!msg.contains("lacks the scope"), "got: {msg}");
    assert!(msg.contains("has not been authorized yet"), "got: {msg}");
}

async fn mint_invite_via_api(app: &TestApp, grant_id: &str) -> (String, String) {
    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/linked-repos/{grant_id}/invite"),
            app.admin_cookie(),
            &serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    let url = body["invite_url"].as_str().unwrap().to_string();
    let token = url.split("token=").nth(1).unwrap().to_string();
    (token, url)
}

async fn get_invites(app: &TestApp, grant_id: &str) -> axum::response::Response {
    app.router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/admin/linked-repos/{grant_id}/invites"))
                .header(app.admin_cookie().0, app.admin_cookie().1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn revoke_invite_via_api(
    app: &TestApp,
    grant_id: &str,
    invite_id: &str,
) -> axum::response::Response {
    app.router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!(
                    "/admin/linked-repos/{grant_id}/invites/{invite_id}"
                ))
                .header(app.admin_cookie().0, app.admin_cookie().1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn seed_grant(app: &TestApp) -> types::LinkedRepo {
    db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap()
}

#[tokio::test]
#[serial]
async fn invite_list_returns_the_hash_not_the_raw_token() {
    common::require_db!();
    let app = TestApp::new().await;
    let grant = seed_grant(&app).await;

    let (token, _) = mint_invite_via_api(&app, &grant.id).await;

    let body = json_body(get_invites(&app, &grant.id).await).await;
    let invites = body["invites"].as_array().unwrap();
    assert_eq!(invites.len(), 1);

    let invite_id = invites[0]["invite_id"].as_str().unwrap();
    assert_ne!(
        invite_id, token,
        "the raw token must never be recoverable from the list endpoint"
    );
    assert_eq!(invite_id.len(), 64, "expected a hex sha256");
    assert!(invites[0]["expires_at"].is_string());

    let raw = serde_json::to_string(&body).unwrap();
    assert!(!raw.contains(&token), "list response leaked the raw token");
}

#[tokio::test]
#[serial]
async fn invite_list_excludes_expired_and_oauth_state_rows() {
    common::require_db!();
    let app = TestApp::new().await;
    let grant = seed_grant(&app).await;

    // A live invite.
    mint_invite_via_api(&app, &grant.id).await;

    let insert = happyview::db::adapt_sql(
        "INSERT INTO happyview_linked_repo_auth_state (state, grant_id, token_hash, expires_at) \
         VALUES (?, ?, ?, ?)",
        app.state.db_backend,
    );

    // An expired invite.
    happyview::db::query(&insert)
        .bind("invite:deadbeef")
        .bind(&grant.id)
        .bind("deadbeef")
        .bind("2020-01-01T00:00:00+00:00")
        .execute(&app.state.db)
        .await
        .unwrap();

    let insert_state = happyview::db::adapt_sql(
        "INSERT INTO happyview_linked_repo_auth_state (state, grant_id, expires_at) \
         VALUES (?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&insert_state)
        .bind("oauth-state-key")
        .bind(&grant.id)
        .bind("2999-01-01T00:00:00+00:00")
        .execute(&app.state.db)
        .await
        .unwrap();

    let body = json_body(get_invites(&app, &grant.id).await).await;
    let invites = body["invites"].as_array().unwrap();
    assert_eq!(
        invites.len(),
        1,
        "only the live invite should be listed, got: {invites:?}"
    );
    assert_ne!(invites[0]["invite_id"].as_str().unwrap(), "deadbeef");
}

#[tokio::test]
#[serial]
async fn invite_list_404s_for_an_unknown_grant() {
    common::require_db!();
    let app = TestApp::new().await;
    let resp = get_invites(&app, "00000000-0000-0000-0000-000000000000").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[serial]
async fn revoking_an_invite_makes_it_unusable() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::flow;

    let grant = seed_grant(&app).await;
    let (token, _) = mint_invite_via_api(&app, &grant.id).await;

    let body = json_body(get_invites(&app, &grant.id).await).await;
    let invite_id = body["invites"][0]["invite_id"]
        .as_str()
        .unwrap()
        .to_string();

    let resp = revoke_invite_via_api(&app, &grant.id, &invite_id).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json_body(resp).await["revoked"], true);

    // the link no longer works
    assert!(
        !flow::invite_exists(&app.state, &token).await.unwrap(),
        "a revoked invite must not be consumable"
    );

    let body = json_body(get_invites(&app, &grant.id).await).await;
    assert!(body["invites"].as_array().unwrap().is_empty());
}

#[tokio::test]
#[serial]
async fn revoking_through_another_grant_is_refused_and_leaves_the_invite_usable() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::flow;

    let owner = seed_grant(&app).await;
    let other = seed_grant(&app).await;
    let (token, _) = mint_invite_via_api(&app, &owner.id).await;

    let body = json_body(get_invites(&app, &owner.id).await).await;
    let invite_id = body["invites"][0]["invite_id"]
        .as_str()
        .unwrap()
        .to_string();

    let resp = revoke_invite_via_api(&app, &other.id, &invite_id).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    assert!(
        flow::invite_exists(&app.state, &token).await.unwrap(),
        "revoking via the wrong grant must not affect the owner's invite"
    );
}

#[tokio::test]
#[serial]
async fn revoking_an_unknown_invite_404s() {
    common::require_db!();
    let app = TestApp::new().await;
    let grant = seed_grant(&app).await;

    let resp = revoke_invite_via_api(&app, &grant.id, "not-a-real-hash").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[serial]
async fn invite_honours_expires_in_and_rejects_out_of_range() {
    common::require_db!();
    let app = TestApp::new().await;
    let grant = seed_grant(&app).await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/linked-repos/{}/invite", grant.id),
            app.admin_cookie(),
            &serde_json::json!({ "expires_in": 120 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let expires_at = json_body(resp).await["expires_at"]
        .as_str()
        .unwrap()
        .to_string();

    // 120s out should be within the 7-day TTL
    let parsed = chrono::DateTime::parse_from_rfc3339(&expires_at).unwrap();
    let delta = parsed.with_timezone(&chrono::Utc) - chrono::Utc::now();
    assert!(
        delta.num_seconds() > 0 && delta.num_seconds() <= 130,
        "expected ~120s TTL, got {}s",
        delta.num_seconds()
    );

    for bad in [30_i64, 60 * 60 * 24 * 60] {
        let resp = app
            .router
            .clone()
            .oneshot(admin_post(
                &format!("/admin/linked-repos/{}/invite", grant.id),
                app.admin_cookie(),
                &serde_json::json!({ "expires_in": bad }),
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "expires_in={bad} should be rejected"
        );
    }
}

#[tokio::test]
#[serial]
async fn invite_endpoint_accepts_a_json_content_type_with_an_empty_body() {
    common::require_db!();
    let app = TestApp::new().await;
    let grant = seed_grant(&app).await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/admin/linked-repos/{}/invite", grant.id))
                .header(app.admin_cookie().0, app.admin_cookie().1)
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(json_body(resp).await["invite_url"].is_string());
}

#[tokio::test]
#[serial]
async fn invite_endpoint_still_accepts_no_body() {
    common::require_db!();
    let app = TestApp::new().await;
    let grant = seed_grant(&app).await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/admin/linked-repos/{}/invite", grant.id))
                .header(app.admin_cookie().0, app.admin_cookie().1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(json_body(resp).await["invite_url"].is_string());
}

#[tokio::test]
#[serial]
async fn reaching_the_pds_redirect_does_not_burn_the_invite() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::flow;

    let grant = seed_grant(&app).await;
    let (token, _) = mint_invite_via_api(&app, &grant.id).await;

    let _ = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/auth/linked-repo/start?token={token}&handle=nonexistent.invalid"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        flow::invite_exists(&app.state, &token).await.unwrap(),
        "the invite must survive a trip to the PDS that was never completed"
    );
}

#[tokio::test]
#[serial]
async fn linking_a_grant_clears_its_outstanding_invites() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::flow;

    let grant = seed_grant(&app).await;
    let (first, _) = mint_invite_via_api(&app, &grant.id).await;
    let (second, _) = mint_invite_via_api(&app, &grant.id).await;

    // An unrelated grant's invite must be left alone.
    let other = seed_grant(&app).await;
    let (other_token, _) = mint_invite_via_api(&app, &other.id).await;

    let cleared = flow::invalidate_invites_for_grant(&app.state, &grant.id)
        .await
        .unwrap();
    assert_eq!(cleared, 2, "both of this grant's invites should be cleared");

    assert!(!flow::invite_exists(&app.state, &first).await.unwrap());
    assert!(!flow::invite_exists(&app.state, &second).await.unwrap());
    assert!(
        flow::invite_exists(&app.state, &other_token).await.unwrap(),
        "another grant's invite must be untouched"
    );
}

#[tokio::test]
#[serial]
async fn invalidating_invites_leaves_in_flight_oauth_state_alone() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::flow;

    let grant = seed_grant(&app).await;
    mint_invite_via_api(&app, &grant.id).await;
    // An in-flight OAuth state row for the same grant shares this table.
    insert_auth_state(&app, "state-inflight", &grant.id, None, &future_rfc3339()).await;

    flow::invalidate_invites_for_grant(&app.state, &grant.id)
        .await
        .unwrap();

    // The authorization currently being completed must not be destroyed by
    // clearing invites.
    let sql = happyview::db::adapt_sql(
        "SELECT COUNT(*) FROM happyview_linked_repo_auth_state WHERE state = ?",
        app.state.db_backend,
    );
    let (count,): (i64,) = happyview::db::query_as(&sql)
        .bind("state-inflight")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(count, 1, "in-flight OAuth state must survive");
}

/// Removing a repo takes its invite links with it — a link for a grant that no
/// longer exists must not stay usable.
#[tokio::test]
#[serial]
async fn deleting_a_grant_deletes_its_invites() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::linked_repos::flow;

    let grant = seed_grant(&app).await;
    let (token, _) = mint_invite_via_api(&app, &grant.id).await;
    assert!(flow::invite_exists(&app.state, &token).await.unwrap());

    // Another grant's invite must survive.
    let other = seed_grant(&app).await;
    let (other_token, _) = mint_invite_via_api(&app, &other.id).await;

    assert!(db::delete(&app.state, &grant.id).await.unwrap());

    assert!(
        !flow::invite_exists(&app.state, &token).await.unwrap(),
        "an invite for a deleted grant must stop working"
    );
    assert!(
        flow::invite_exists(&app.state, &other_token).await.unwrap(),
        "another grant's invite must be untouched"
    );
}

/// ...but it deliberately leaves in-flight OAuth state alone.
///
/// If an admin removes a repo while someone is partway through authorizing it,
/// that person's callback still has to be *recognised* as a linked-repo
/// callback so they get the "request withdrawn" page. Delete the state row and
/// the callback falls through to the dashboard-login exchange instead — which
/// is how a linked account's elevated-scope tokens could end up in the
/// dashboard session table.
#[tokio::test]
#[serial]
async fn deleting_a_grant_preserves_in_flight_oauth_state() {
    common::require_db!();
    let app = TestApp::new().await;

    let grant = seed_grant(&app).await;
    mint_invite_via_api(&app, &grant.id).await;
    insert_auth_state(&app, "state-midflight", &grant.id, None, &future_rfc3339()).await;

    assert!(db::delete(&app.state, &grant.id).await.unwrap());

    let sql = happyview::db::adapt_sql(
        "SELECT COUNT(*) FROM happyview_linked_repo_auth_state WHERE state = ?",
        app.state.db_backend,
    );
    let (count,): (i64,) = happyview::db::query_as(&sql)
        .bind("state-midflight")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(
        count, 1,
        "in-flight OAuth state must outlive the grant so the callback can \
         report it was withdrawn rather than falling through to dashboard login"
    );

    // And the callback does exactly that.
    let resp = app
        .router
        .clone()
        .oneshot(callback_request("bogus-code", "state-midflight"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(
        location.contains("/link/result?status=gone"),
        "got: {location}"
    );
    assert!(
        resp.headers().get("set-cookie").is_none(),
        "must never issue a dashboard session cookie"
    );
}

// ---------------------------------------------------------------------------
// Expired OAuth-state cleanup
// ---------------------------------------------------------------------------

/// Abandoned flows leave rows in both state tables. Nothing stale is ever
/// honoured — every read filters on `expires_at` — but without a sweep the rows
/// accumulate with no ceiling.
#[tokio::test]
#[serial]
async fn expired_state_gc_purges_both_tables_and_spares_live_rows() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::auth::state_gc::purge_expired_state;

    let grant = seed_grant(&app).await;

    // A live invite and a live in-flight authorization.
    let (live_token, _) = mint_invite_via_api(&app, &grant.id).await;
    insert_auth_state(&app, "state-live", &grant.id, None, &future_rfc3339()).await;

    // Their expired counterparts.
    insert_auth_state(&app, "state-dead", &grant.id, None, &past_rfc3339()).await;
    let insert_invite = happyview::db::adapt_sql(
        "INSERT INTO happyview_linked_repo_auth_state (state, grant_id, token_hash, expires_at) \
         VALUES (?, ?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&insert_invite)
        .bind("invite:expired")
        .bind(&grant.id)
        .bind("expiredhash")
        .bind(past_rfc3339())
        .execute(&app.state.db)
        .await
        .unwrap();

    // And the sibling table the same sweep covers.
    let insert_redirect = happyview::db::adapt_sql(
        "INSERT INTO happyview_auth_login_redirects (state, redirect_uri, created_at, expires_at) \
         VALUES (?, ?, ?, ?)",
        app.state.db_backend,
    );
    for (key, expires) in [
        ("redirect-dead", past_rfc3339()),
        ("redirect-live", future_rfc3339()),
    ] {
        happyview::db::query(&insert_redirect)
            .bind(key)
            .bind("/dashboard")
            .bind(happyview::db::now_rfc3339())
            .bind(expires)
            .execute(&app.state.db)
            .await
            .unwrap();
    }

    let removed = purge_expired_state(&app.state.db, app.state.db_backend).await;
    assert_eq!(
        removed, 3,
        "expected the expired invite, in-flight state, and redirect to be swept"
    );

    let count = |table: &'static str, key: &'static str| {
        let sql = happyview::db::adapt_sql(
            &format!("SELECT COUNT(*) FROM {table} WHERE state = ?"),
            app.state.db_backend,
        );
        let pool = app.state.db.clone();
        async move {
            let (n,): (i64,) = happyview::db::query_as(&sql)
                .bind(key)
                .fetch_one(&pool)
                .await
                .unwrap();
            n
        }
    };

    assert_eq!(
        count("happyview_linked_repo_auth_state", "state-dead").await,
        0
    );
    assert_eq!(
        count("happyview_linked_repo_auth_state", "invite:expired").await,
        0
    );
    assert_eq!(
        count("happyview_auth_login_redirects", "redirect-dead").await,
        0
    );

    // Live rows are untouched, and the live invite still works.
    assert_eq!(
        count("happyview_linked_repo_auth_state", "state-live").await,
        1
    );
    assert_eq!(
        count("happyview_auth_login_redirects", "redirect-live").await,
        1
    );
    assert!(
        happyview::linked_repos::flow::invite_exists(&app.state, &live_token)
            .await
            .unwrap(),
        "a live invite must survive the sweep"
    );
}

/// Sweeping an already-clean database is a no-op, so the hourly tick is free.
#[tokio::test]
#[serial]
async fn expired_state_gc_is_a_no_op_when_nothing_has_expired() {
    common::require_db!();
    let app = TestApp::new().await;
    use happyview::auth::state_gc::purge_expired_state;

    let grant = seed_grant(&app).await;
    mint_invite_via_api(&app, &grant.id).await;

    assert_eq!(
        purge_expired_state(&app.state.db, app.state.db_backend).await,
        0
    );
}
