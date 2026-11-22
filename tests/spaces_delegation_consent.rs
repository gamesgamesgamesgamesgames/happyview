//! Space-as-member entries (`isDelegation`): naming another space as a member
//! brings in its members only when whoever adds the entry administers that
//! space too.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::db::now_rfc3339;
use happyview::spaces::db as spaces_db;
use happyview::spaces::types::*;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

const TYPE: &str = "com.example.delegation";

fn rand_did(label: &str) -> String {
    format!("did:plc:{label}{}", Uuid::new_v4().simple())
}

async fn setup() -> TestApp {
    let app = TestApp::new().await;
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
    app
}

/// A member-list space created by `creator`, with `creator` as its only
/// member. Returns (id, address).
async fn create_space(app: &TestApp, creator: &str) -> (String, String) {
    create_space_at(app, creator, creator, &Uuid::new_v4().simple().to_string()).await
}

/// [`create_space`] at the address `did` and `skey`.
async fn create_space_at(app: &TestApp, creator: &str, did: &str, skey: &str) -> (String, String) {
    let now = now_rfc3339();
    let id = Uuid::new_v4().to_string();
    let skey = skey.to_string();
    let space = Space {
        id: id.clone(),
        did: did.to_string(),
        authority_did: creator.to_string(),
        creator_did: creator.to_string(),
        type_nsid: TYPE.to_string(),
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
    add_row(app, &id, creator, false, creator).await;
    (id, format!("at://{did}/space/{TYPE}/{skey}"))
}

/// A member row stored directly, as an older version would have stored it.
async fn add_row(app: &TestApp, space_id: &str, did: &str, is_delegation: bool, granted_by: &str) {
    spaces_db::add_member(
        &app.state.db,
        app.state.db_backend,
        &SpaceMember {
            id: Uuid::new_v4().to_string(),
            space_id: space_id.to_string(),
            did: did.to_string(),
            access: MemberAccess::READ,
            is_delegation,
            granted_by: Some(granted_by.to_string()),
            created_at: now_rfc3339(),
        },
    )
    .await
    .unwrap();
}

async fn post(app: &TestApp, did: &str, nsid: &str, body: Value) -> (StatusCode, Value) {
    let (name, value) = common::auth::admin_cookie_header(did, &app.state.cookie_key);
    let req = Request::builder()
        .method("POST")
        .uri(format!("/xrpc/{nsid}"))
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap_or(json!(null)))
}

async fn listed(app: &TestApp, actor: &str, address: &str) -> Vec<String> {
    let (name, value) = common::auth::admin_cookie_header(actor, &app.state.cookie_key);
    let req = Request::builder()
        .uri(format!(
            "/xrpc/com.atproto.simplespace.listMembers?space={}",
            urlencoding::encode(address)
        ))
        .header(name, value)
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    body["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["did"].as_str().unwrap().to_string())
        .collect()
}

async fn is_member(app: &TestApp, space_id: &str, did: &str) -> bool {
    happyview::spaces::members::is_member(&app.state.db, app.state.db_backend, space_id, did)
        .await
        .unwrap()
        .is_some()
}

async fn stored(app: &TestApp, space_id: &str, did: &str) -> bool {
    spaces_db::get_member(&app.state.db, app.state.db_backend, space_id, did)
        .await
        .unwrap()
        .is_some()
}

#[tokio::test]
#[serial]
async fn delegating_to_a_space_requires_administering_it() {
    common::require_db!();
    let app = setup().await;
    let (x_owner, y_owner, y_member) = (rand_did("x"), rand_did("y"), rand_did("ym"));
    let (x, x_addr) = create_space(&app, &x_owner).await;
    let (y, y_addr) = create_space(&app, &y_owner).await;
    add_row(&app, &y, &y_member, false, &y_owner).await;

    // By address or by id, through either method.
    for target in [&y_addr, &y] {
        for (nsid, body) in [
            (
                "com.atproto.simplespace.putMember",
                json!({ "space": x_addr, "did": target, "read": true, "write": false, "isDelegation": true }),
            ),
            (
                "com.atproto.simplespace.addMember",
                json!({ "space": x_addr, "did": target, "isDelegation": true }),
            ),
        ] {
            let (status, body) = post(&app, &x_owner, nsid, body).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{nsid} {target}: {body}");
            assert!(!stored(&app, &x, target).await, "{nsid} {target}");
        }
    }
    assert!(!is_member(&app, &x, &y_member).await);
    assert!(!listed(&app, &x_owner, &x_addr).await.contains(&y_member));
}

#[tokio::test]
#[serial]
async fn delegating_to_a_space_you_administer_brings_in_its_members() {
    common::require_db!();
    let app = setup().await;
    let (owner, member) = (rand_did("o"), rand_did("m"));
    let (x, x_addr) = create_space(&app, &owner).await;
    let (y, y_addr) = create_space(&app, &owner).await;
    add_row(&app, &y, &member, false, &owner).await;

    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.simplespace.putMember",
        json!({ "space": x_addr, "did": y_addr, "read": true, "write": false, "isDelegation": true }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(is_member(&app, &x, &member).await);
    // listMembers keeps its shape: the named space's members are listed.
    assert!(listed(&app, &owner, &x_addr).await.contains(&member));

    // By id, through addMember.
    let (z, z_addr) = create_space(&app, &owner).await;
    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.simplespace.addMember",
        json!({ "space": z_addr, "did": y, "isDelegation": true }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(is_member(&app, &z, &member).await);
}

#[tokio::test]
#[serial]
async fn an_entry_stored_without_the_named_spaces_admin_brings_in_no_members() {
    common::require_db!();
    let app = setup().await;
    let (x_owner, y_owner, y_member) = (rand_did("x"), rand_did("y"), rand_did("ym"));
    let (x, x_addr) = create_space(&app, &x_owner).await;
    let (y, y_addr) = create_space(&app, &y_owner).await;
    add_row(&app, &y, &y_member, false, &y_owner).await;

    // Stored as an older version stored it, by X's creator alone.
    add_row(&app, &x, &y_addr, true, &x_owner).await;
    assert!(stored(&app, &x, &y_addr).await);
    assert!(!is_member(&app, &x, &y_member).await);
    assert!(!listed(&app, &x_owner, &x_addr).await.contains(&y_member));
}

/// Entries naming a space not on this host are stored as they always were,
/// and bring in nothing, even once a space with that address appears.
#[tokio::test]
#[serial]
async fn an_entry_naming_a_space_not_here_brings_in_nothing() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("o");
    let (x, x_addr) = create_space(&app, &owner).await;
    let (remote, remote_member) = (rand_did("r"), rand_did("rm"));
    let elsewhere = format!("at://{remote}/space/{TYPE}/elsewhere");
    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.simplespace.putMember",
        json!({ "space": x_addr, "did": elsewhere, "read": true, "write": false, "isDelegation": true }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(stored(&app, &x, &elsewhere).await);
    assert!(
        spaces_db::consented_delegations(&app.state.db, app.state.db_backend, &x)
            .await
            .unwrap()
            .is_empty()
    );

    // The space appears here later, with a member.
    let (later, later_addr) = create_space_at(&app, &remote, &remote, "elsewhere").await;
    assert_eq!(later_addr, elsewhere);
    add_row(&app, &later, &remote_member, false, &remote).await;
    assert!(!is_member(&app, &x, &remote_member).await);
}

/// An entry stored with the consent of the space `target`, as one its
/// administrator added.
async fn add_consented(app: &TestApp, space_id: &str, reference: &str, target: &str) {
    assert!(
        spaces_db::insert_delegation_if_allowed(
            &app.state.db,
            app.state.db_backend,
            &delegation(space_id, reference),
            spaces_db::DelegationConsent::Granted(target),
        )
        .await
        .unwrap()
    );
}

fn delegation(space_id: &str, reference: &str) -> SpaceMember {
    SpaceMember {
        id: Uuid::new_v4().to_string(),
        space_id: space_id.to_string(),
        did: reference.to_string(),
        access: MemberAccess::READ,
        is_delegation: true,
        granted_by: None,
        created_at: now_rfc3339(),
    }
}

async fn consented(app: &TestApp, space_id: &str, reference: &str) -> bool {
    spaces_db::consented_delegations(&app.state.db, app.state.db_backend, space_id)
        .await
        .unwrap()
        .contains_key(reference)
}

/// Putting an entry that already brings in members again, unchanged, keeps it
/// without the named space's administrator: a client re-sending its whole
/// member list goes on working. Changing its access, or naming another space,
/// needs that administrator.
#[tokio::test]
#[serial]
async fn putting_a_consented_entry_again_unchanged_keeps_it() {
    common::require_db!();
    let app = setup().await;
    let (x_owner, y_owner, y_member) = (rand_did("x"), rand_did("y"), rand_did("ym"));
    let (x, x_addr) = create_space(&app, &x_owner).await;
    let (y, y_addr) = create_space(&app, &y_owner).await;
    add_row(&app, &y, &y_member, false, &y_owner).await;
    // Added by the instance's super admin, who administers both.
    add_consented(&app, &x, &y_addr, &y).await;
    assert!(is_member(&app, &x, &y_member).await);

    let put = |read: bool, write: bool, did: &str| json!({ "space": x_addr, "did": did, "read": read, "write": write, "isDelegation": true });
    // Unchanged: kept, by X's creator alone.
    let (status, body) = post(
        &app,
        &x_owner,
        "com.atproto.simplespace.putMember",
        put(true, false, &y_addr),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(consented(&app, &x, &y_addr).await);
    assert!(is_member(&app, &x, &y_member).await);

    // A change of access needs Y's administrator; the entry is left as it was.
    let (status, body) = post(
        &app,
        &x_owner,
        "com.atproto.simplespace.putMember",
        put(true, true, &y_addr),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let row = spaces_db::get_member(&app.state.db, app.state.db_backend, &x, &y_addr)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.access, MemberAccess::READ);
    assert!(consented(&app, &x, &y_addr).await);

    // Naming Y by its id is another entry, and needs Y's administrator.
    let (status, body) = post(
        &app,
        &x_owner,
        "com.atproto.simplespace.putMember",
        put(true, false, &y),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(!stored(&app, &x, &y).await);
}

/// Putting an entry stored without the named space's administrator again
/// needs that administrator, and leaves it bringing in no members.
#[tokio::test]
#[serial]
async fn putting_an_unconsented_entry_again_needs_the_named_spaces_admin() {
    common::require_db!();
    let app = setup().await;
    let (x_owner, y_owner, y_member) = (rand_did("x"), rand_did("y"), rand_did("ym"));
    let (x, x_addr) = create_space(&app, &x_owner).await;
    let (y, y_addr) = create_space(&app, &y_owner).await;
    add_row(&app, &y, &y_member, false, &y_owner).await;
    add_row(&app, &x, &y_addr, true, &x_owner).await;

    let (status, body) = post(
        &app,
        &x_owner,
        "com.atproto.simplespace.putMember",
        json!({ "space": x_addr, "did": y_addr, "read": true, "write": false, "isDelegation": true }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(!consented(&app, &x, &y_addr).await);
    assert!(!is_member(&app, &x, &y_member).await);
}

/// Consent is checked at every level: X brings in Y's members, but not those
/// of a space Y names without its administrator.
#[tokio::test]
#[serial]
async fn a_nested_entry_brings_in_members_only_when_consented() {
    common::require_db!();
    let app = setup().await;
    let (owner, z_owner) = (rand_did("o"), rand_did("z"));
    let (y_member, z_member) = (rand_did("ym"), rand_did("zm"));
    let (x, _) = create_space(&app, &owner).await;
    let (y, y_addr) = create_space(&app, &owner).await;
    let (z, z_addr) = create_space(&app, &z_owner).await;
    add_row(&app, &y, &y_member, false, &owner).await;
    add_row(&app, &z, &z_member, false, &z_owner).await;
    add_consented(&app, &x, &y_addr, &y).await;
    add_row(&app, &y, &z_addr, true, &owner).await;

    assert!(is_member(&app, &x, &y_member).await);
    assert!(!is_member(&app, &x, &z_member).await);
    assert!(!is_member(&app, &y, &z_member).await);
}

/// The migration's backfill counts an entry already stored as added by the
/// named space's administrator when its adder is that space's creator or a
/// super admin, or both spaces have the same creator.
#[tokio::test]
#[serial]
async fn stored_entries_are_backfilled_by_who_added_them() {
    common::require_db!();
    let app = setup().await;
    let (x_owner, y_owner, y_member) = (rand_did("x"), rand_did("y"), rand_did("ym"));
    let (y, y_addr) = create_space(&app, &y_owner).await;
    add_row(&app, &y, &y_member, false, &y_owner).await;

    // Unconsented: X's creator alone.
    let (unconsented, _) = create_space(&app, &x_owner).await;
    add_row(&app, &unconsented, &y_addr, true, &x_owner).await;
    // Added by Y's creator.
    let (by_target, _) = create_space(&app, &x_owner).await;
    add_row(&app, &by_target, &y_addr, true, &y_owner).await;
    // Same creator, by id.
    let (same_creator, _) = create_space(&app, &y_owner).await;
    add_row(&app, &same_creator, &y, true, &y_owner).await;
    // By the instance's super admin.
    let (by_super, _) = create_space(&app, &x_owner).await;
    add_row(&app, &by_super, &y_addr, true, &app.admin_did).await;
    // An address with a record path, naming a space Z whose creator is not
    // the listing space's. Z's address differs from W's only where W's skey
    // has a `_`, and W shares the listing space's creator (y_owner): the
    // address names Z, not W, so the entry is not consented.
    let (host, z_owner, z_member) = (rand_did("h"), rand_did("z"), rand_did("zm"));
    let u = Uuid::new_v4().simple().to_string();
    create_space_at(&app, &y_owner, &host, &format!("w_{u}")).await;
    let (z, z_addr) = create_space_at(&app, &z_owner, &host, &format!("wz{u}")).await;
    add_row(&app, &z, &z_member, false, &z_owner).await;
    let (wildcard, _) = create_space(&app, &y_owner).await;
    add_row(
        &app,
        &wildcard,
        &format!("{z_addr}/{z_member}/{TYPE}.post/abc"),
        true,
        &y_owner,
    )
    .await;

    let migration = match app.state.db_backend {
        happyview::db::DatabaseBackend::Postgres => {
            include_str!("../migrations/postgres/20261120000001_space_delegation_consent.sql")
        }
        happyview::db::DatabaseBackend::Sqlite => {
            include_str!("../migrations/sqlite/20261120000001_space_delegation_consent.sql")
        }
    };
    let backfill = &migration[migration.find("UPDATE").unwrap()..];
    for statement in backfill.split(';').filter(|s| !s.trim().is_empty()) {
        happyview::db::query(statement)
            .execute(&app.state.db)
            .await
            .unwrap();
    }

    assert!(!is_member(&app, &unconsented, &y_member).await);
    assert!(is_member(&app, &by_target, &y_member).await);
    assert!(is_member(&app, &same_creator, &y_member).await);
    assert!(is_member(&app, &by_super, &y_member).await);
    assert!(!is_member(&app, &wildcard, &z_member).await);
}

/// An entry brings in the members of the space it was added for, not of
/// whatever space takes that address after it is deleted.
#[tokio::test]
#[serial]
async fn an_entry_stops_when_the_space_it_names_is_deleted() {
    common::require_db!();
    let app = setup().await;
    let (owner, other) = (rand_did("o"), rand_did("other"));
    let (y_member, other_member) = (rand_did("ym"), rand_did("om"));
    let host = rand_did("h");
    let (x, _) = create_space(&app, &owner).await;

    // Deleted through deleteSpace, then taken by another user at the same
    // address.
    let skey = Uuid::new_v4().simple().to_string();
    let (y, y_addr) = create_space_at(&app, &owner, &host, &skey).await;
    add_row(&app, &y, &y_member, false, &owner).await;
    add_consented(&app, &x, &y_addr, &y).await;
    assert!(is_member(&app, &x, &y_member).await);
    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.simplespace.deleteSpace",
        json!({ "space": y_addr }),
    )
    .await;
    assert!(status.is_success(), "{status}: {body}");
    assert!(!consented(&app, &x, &y_addr).await);
    let (again, again_addr) = create_space_at(&app, &other, &host, &skey).await;
    assert_eq!(again_addr, y_addr);
    add_row(&app, &again, &other_member, false, &other).await;
    assert!(!is_member(&app, &x, &other_member).await);

    // Gone some other way: the entry is still stored as consented, but names
    // a different space now.
    let skey = Uuid::new_v4().simple().to_string();
    let (w, w_addr) = create_space_at(&app, &owner, &host, &skey).await;
    add_consented(&app, &x, &w_addr, &w).await;
    happyview::db::query(&happyview::db::adapt_sql(
        "DELETE FROM happyview_spaces WHERE id = ?",
        app.state.db_backend,
    ))
    .bind(&w)
    .execute(&app.state.db)
    .await
    .unwrap();
    let (again, _) = create_space_at(&app, &other, &host, &skey).await;
    add_row(&app, &again, &other_member, false, &other).await;
    assert!(consented(&app, &x, &w_addr).await);
    assert!(!is_member(&app, &x, &other_member).await);
}

/// Keeping an entry's consent is decided in the write: an entry that has none
/// by then, or is gone, is stored with none.
#[tokio::test]
#[serial]
async fn keeping_consent_needs_it_at_write_time() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("o");
    let (x, _) = create_space(&app, &owner).await;
    let (y, y_addr) = create_space(&app, &owner).await;
    let keep = |member: SpaceMember| {
        let (db, backend) = (app.state.db.clone(), app.state.db_backend);
        async move {
            spaces_db::insert_delegation_if_allowed(
                &db,
                backend,
                &member,
                spaces_db::DelegationConsent::Kept,
            )
            .await
            .unwrap()
        }
    };

    // No entry: stored without consent.
    assert!(keep(delegation(&x, &y_addr)).await);
    assert!(stored(&app, &x, &y_addr).await);
    assert!(!consented(&app, &x, &y_addr).await);

    // An entry without consent stays without.
    assert!(keep(delegation(&x, &y_addr)).await);
    assert!(!consented(&app, &x, &y_addr).await);

    // A consented entry, unchanged, keeps it; with other access, loses it.
    add_consented(&app, &x, &y_addr, &y).await;
    assert!(keep(delegation(&x, &y_addr)).await);
    assert!(consented(&app, &x, &y_addr).await);
    let mut changed = delegation(&x, &y_addr);
    changed.access = MemberAccess::WRITE;
    assert!(keep(changed).await);
    assert!(!consented(&app, &x, &y_addr).await);
}

/// How many spaces `space_id` nests.
async fn nested_count(app: &TestApp, space_id: &str) -> i64 {
    let (count,): (i64,) = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT COUNT(*) FROM happyview_space_nested WHERE space_id = ?",
        app.state.db_backend,
    ))
    .bind(space_id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    count
}

/// An entry naming a memberspace, stored before memberspaces nested one
/// another, put again unchanged by the nesting space's administrator alone
/// stays the entry it was, rather than becoming a nested space.
#[tokio::test]
#[serial]
async fn an_older_entry_put_again_unchanged_stays_an_entry() {
    common::require_db!();
    let app = setup().await;
    let (x_owner, y_owner, y_member) = (rand_did("x"), rand_did("y"), rand_did("ym"));
    let (x, x_addr) = create_space(&app, &x_owner).await;
    let (y, y_addr) = create_space(&app, &y_owner).await;
    add_row(&app, &y, &y_member, false, &y_owner).await;
    add_consented(&app, &x, &y_addr, &y).await;

    for (nsid, body, expected) in [
        (
            "com.atproto.simplespace.putMember",
            json!({ "space": x_addr, "did": y_addr, "read": true, "write": false, "isDelegation": true }),
            StatusCode::CREATED,
        ),
        // Naming no isDelegation leaves it an entry.
        (
            "com.atproto.simplespace.putMember",
            json!({ "space": x_addr, "did": y_addr, "read": true, "write": false }),
            StatusCode::CREATED,
        ),
        // addMember on an existing entry is a conflict, as it always was.
        (
            "com.atproto.simplespace.addMember",
            json!({ "space": x_addr, "did": y_addr, "read": true, "write": false, "isDelegation": true }),
            StatusCode::CONFLICT,
        ),
    ] {
        let (status, body) = post(&app, &x_owner, nsid, body).await;
        assert_eq!(status, expected, "{nsid}: {body}");
        assert_eq!(nested_count(&app, &x).await, 0, "{nsid}");
        assert!(consented(&app, &x, &y_addr).await, "{nsid}");
        assert!(is_member(&app, &x, &y_member).await, "{nsid}");
        assert!(
            spaces_db::get_member(&app.state.db, app.state.db_backend, &x, &y_addr)
                .await
                .unwrap()
                .unwrap()
                .is_delegation,
            "{nsid}"
        );
    }

    // Changing its access needs Y's administrator.
    let (status, body) = post(
        &app,
        &x_owner,
        "com.atproto.simplespace.putMember",
        json!({ "space": x_addr, "did": y_addr, "read": true, "write": true, "isDelegation": true }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(nested_count(&app, &x).await, 0);
    assert_eq!(
        spaces_db::get_member(&app.state.db, app.state.db_backend, &x, &y_addr)
            .await
            .unwrap()
            .unwrap()
            .access,
        MemberAccess::READ
    );
}

/// A space whose access rules predate space policies can't nest, so an entry
/// naming a space is stored as a member row, as it always was. Storing one
/// still needs the named space's administrator, whether it names the space
/// by address or by id.
#[tokio::test]
#[serial]
async fn a_space_with_older_rules_delegates_only_to_spaces_its_admin_administers() {
    common::require_db!();
    let app = setup().await;
    let (x_owner, y_owner, y_member, z_member) =
        (rand_did("x"), rand_did("y"), rand_did("ym"), rand_did("zm"));
    let (x, x_addr) = create_space(&app, &x_owner).await;
    // Read by a managing app, written by members: rules no policy matches.
    let older = Policy::ManagingApp {
        managing_app: "did:web:app.example".into(),
    };
    happyview::db::query(&happyview::db::adapt_sql(
        "UPDATE happyview_spaces SET read_policy = ? WHERE id = ?",
        app.state.db_backend,
    ))
    .bind(serde_json::to_string(&older).unwrap())
    .bind(&x)
    .execute(&app.state.db)
    .await
    .unwrap();
    let (y, y_addr) = create_space(&app, &y_owner).await;
    add_row(&app, &y, &y_member, false, &y_owner).await;
    let (z, z_addr) = create_space(&app, &x_owner).await;
    add_row(&app, &z, &z_member, false, &x_owner).await;

    for nsid in [
        "com.atproto.simplespace.putMember",
        "com.atproto.simplespace.addMember",
    ] {
        for named in [&y_addr, &y] {
            let (status, body) = post(
                &app,
                &x_owner,
                nsid,
                json!({ "space": x_addr, "did": named, "read": true, "write": false, "isDelegation": true }),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{nsid} {named}: {body}");
            assert!(!stored(&app, &x, named).await, "{nsid} {named}");
        }
    }
    assert!(!is_member(&app, &x, &y_member).await);

    let (status, body) = post(
        &app,
        &x_owner,
        "com.atproto.simplespace.putMember",
        json!({ "space": x_addr, "did": z_addr, "read": true, "write": false, "isDelegation": true }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(nested_count(&app, &x).await, 0);
    assert!(consented(&app, &x, &z_addr).await);
    assert!(is_member(&app, &x, &z_member).await);
}

/// An entry stored as a member row changes in place when the administrator
/// of both spaces changes its access, as it always did, rather than gaining
/// a nested space beside it.
#[tokio::test]
#[serial]
async fn an_older_entry_changes_in_place() {
    common::require_db!();
    let app = setup().await;
    let (owner, y_member) = (rand_did("o"), rand_did("ym"));
    let (x, x_addr) = create_space(&app, &owner).await;
    let (y, y_addr) = create_space(&app, &owner).await;
    add_row(&app, &y, &y_member, false, &owner).await;
    add_consented(&app, &x, &y_addr, &y).await;

    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.simplespace.putMember",
        json!({ "space": x_addr, "did": y_addr, "read": true, "write": true, "isDelegation": true }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(nested_count(&app, &x).await, 0);
    let row = spaces_db::get_member(&app.state.db, app.state.db_backend, &x, &y_addr)
        .await
        .unwrap()
        .unwrap();
    assert!(row.access.read && row.access.write);
    assert!(consented(&app, &x, &y_addr).await);

    // The named space's member reads there, and gains no write: the entry
    // grants at most what the member has in the named space.
    assert_eq!(
        access_of(&app, &x, &y_member).await,
        Some(MemberAccess::READ)
    );
}

/// A member named by a space's address is a space-as-member entry unless the
/// caller says it isn't, whether or not the space is here.
#[tokio::test]
#[serial]
async fn a_space_address_named_without_is_delegation_is_an_entry() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("o");
    let (x, x_addr) = create_space(&app, &owner).await;
    for (nsid, explicit) in [
        ("com.atproto.simplespace.putMember", None),
        ("com.atproto.simplespace.addMember", None),
        ("com.atproto.simplespace.putMember", Some(false)),
    ] {
        let elsewhere = format!("at://{}/space/{TYPE}/elsewhere", rand_did("r"));
        let mut body = json!({ "space": x_addr, "did": elsewhere, "read": true, "write": false });
        if let Some(explicit) = explicit {
            body["isDelegation"] = json!(explicit);
        }
        let (status, body) = post(&app, &owner, nsid, body).await;
        assert!(status.is_success(), "{nsid} {explicit:?}: {body}");
        let row = spaces_db::get_member(&app.state.db, app.state.db_backend, &x, &elsewhere)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.is_delegation,
            explicit.unwrap_or(true),
            "{nsid} {explicit:?}"
        );
        assert!(
            !consented(&app, &x, &elsewhere).await,
            "{nsid} {explicit:?}"
        );
    }
}

/// A member row stored directly with `access`.
async fn add_row_with(app: &TestApp, space_id: &str, did: &str, access: MemberAccess) {
    spaces_db::add_member(
        &app.state.db,
        app.state.db_backend,
        &SpaceMember {
            access,
            is_delegation: false,
            ..delegation(space_id, did)
        },
    )
    .await
    .unwrap();
}

/// [`add_consented`] with the entry granting `access`.
async fn add_consented_with(
    app: &TestApp,
    space_id: &str,
    reference: &str,
    target: &str,
    access: MemberAccess,
) {
    assert!(
        spaces_db::insert_delegation_if_allowed(
            &app.state.db,
            app.state.db_backend,
            &SpaceMember {
                access,
                ..delegation(space_id, reference)
            },
            spaces_db::DelegationConsent::Granted(target),
        )
        .await
        .unwrap()
    );
}

/// The access `did` has to the space, as access checks resolve it.
async fn access_of(app: &TestApp, space_id: &str, did: &str) -> Option<MemberAccess> {
    happyview::spaces::members::is_member(&app.state.db, app.state.db_backend, space_id, did)
        .await
        .unwrap()
}

/// `did`'s read and write as `listMembers` answers them.
async fn listed_access(app: &TestApp, actor: &str, address: &str, did: &str) -> (Value, Value) {
    let (name, value) = common::auth::admin_cookie_header(actor, &app.state.cookie_key);
    let req = Request::builder()
        .uri(format!(
            "/xrpc/com.atproto.simplespace.listMembers?space={}",
            urlencoding::encode(address)
        ))
        .header(name, value)
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let member = body["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["did"] == json!(did))
        .unwrap_or_else(|| panic!("{did} listed"))
        .clone();
    (member["read"].clone(), member["write"].clone())
}

const READ_ONLY: MemberAccess = MemberAccess::READ;
const READ_WRITE: MemberAccess = MemberAccess::WRITE;
const WRITE_ONLY: MemberAccess = MemberAccess {
    read: false,
    write: true,
    read_self: false,
};

/// A member brought in through an entry gets the lesser of their own access
/// in the named space and the entry's: a read-only entry grants a member who
/// reads and writes there only read, and a write-only entry grants no read.
#[tokio::test]
#[serial]
async fn an_entry_grants_its_members_no_more_than_its_own_access() {
    common::require_db!();
    let app = setup().await;
    let (owner, member) = (rand_did("o"), rand_did("m"));
    let (y, y_addr) = create_space(&app, &owner).await;
    add_row_with(&app, &y, &member, READ_WRITE).await;

    for (entry, expected) in [
        (READ_ONLY, READ_ONLY),
        (WRITE_ONLY, WRITE_ONLY),
        (READ_WRITE, READ_WRITE),
    ] {
        let (x, x_addr) = create_space(&app, &owner).await;
        add_consented_with(&app, &x, &y_addr, &y, entry).await;
        assert_eq!(
            access_of(&app, &x, &member).await,
            Some(expected),
            "{entry:?}"
        );
        assert_eq!(
            listed_access(&app, &owner, &x_addr, &member).await,
            (json!(expected.read), json!(expected.write)),
            "{entry:?}"
        );
    }

    // A member who only reads the named space gains no write from an entry
    // that grants it.
    let reader = rand_did("r");
    add_row_with(&app, &y, &reader, READ_ONLY).await;
    let (x, _) = create_space(&app, &owner).await;
    add_consented_with(&app, &x, &y_addr, &y, READ_WRITE).await;
    assert_eq!(access_of(&app, &x, &reader).await, Some(READ_ONLY));
}

/// The cap applies at every entry on the way: X naming Y naming Z gives Z's
/// members at most what both entries grant.
#[tokio::test]
#[serial]
async fn entries_cap_their_members_at_every_level() {
    common::require_db!();
    let app = setup().await;
    let (owner, member) = (rand_did("o"), rand_did("m"));
    let (z, z_addr) = create_space(&app, &owner).await;
    add_row_with(&app, &z, &member, READ_WRITE).await;

    for (x_to_y, y_to_z, expected) in [
        (READ_ONLY, READ_WRITE, READ_ONLY),
        (READ_WRITE, READ_ONLY, READ_ONLY),
        (READ_WRITE, WRITE_ONLY, WRITE_ONLY),
        (
            WRITE_ONLY,
            READ_ONLY,
            MemberAccess {
                read: false,
                write: false,
                read_self: false,
            },
        ),
        (READ_WRITE, READ_WRITE, READ_WRITE),
    ] {
        let (y, y_addr) = create_space(&app, &owner).await;
        add_consented_with(&app, &y, &z_addr, &z, y_to_z).await;
        let (x, _) = create_space(&app, &owner).await;
        add_consented_with(&app, &x, &y_addr, &y, x_to_y).await;
        assert_eq!(
            access_of(&app, &x, &member).await,
            Some(expected),
            "{x_to_y:?} then {y_to_z:?}"
        );
    }

    // Reached through two entries, the member gets the most either grants.
    let (y1, y1_addr) = create_space(&app, &owner).await;
    add_consented_with(&app, &y1, &z_addr, &z, READ_ONLY).await;
    let (y2, y2_addr) = create_space(&app, &owner).await;
    add_consented_with(&app, &y2, &z_addr, &z, WRITE_ONLY).await;
    let (x, _) = create_space(&app, &owner).await;
    add_consented_with(&app, &x, &y1_addr, &y1, READ_WRITE).await;
    add_consented_with(&app, &x, &y2_addr, &y2, READ_WRITE).await;
    assert_eq!(access_of(&app, &x, &member).await, Some(READ_WRITE));
}
