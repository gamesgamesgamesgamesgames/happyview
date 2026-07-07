mod common;

use happyview::db::now_rfc3339;
use happyview::spaces::db as spaces_db;
use happyview::spaces::notifications;
use happyview::spaces::oplog;
use happyview::spaces::types::*;
use serial_test::serial;
use uuid::Uuid;

use common::db as test_db;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn make_space(id: &str, did: &str, type_nsid: &str, skey: &str) -> Space {
    let now = now_rfc3339();
    Space {
        id: id.to_string(),
        did: did.to_string(),
        authority_did: did.to_string(),
        creator_did: did.to_string(),
        type_nsid: type_nsid.to_string(),
        skey: skey.to_string(),
        display_name: Some("Test Space".to_string()),
        description: None,
        mint_policy: MintPolicy::MemberList,
        app_access: AppAccess::Open,
        managing_app_did: None,
        config: SpaceConfig::default(),
        revision: None,
        created_at: now.clone(),
        updated_at: now,
    }
}

// ---------------------------------------------------------------------------
// Space CRUD
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn create_and_get_space_roundtrip() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let id = new_id();
    let did = "did:plc:spaces-test-owner";
    let space = make_space(&id, did, "com.example.test", "myspace");

    spaces_db::create_space(&pool, backend, &space)
        .await
        .expect("create_space failed");

    let fetched = spaces_db::get_space(&pool, backend, &id)
        .await
        .expect("get_space failed")
        .expect("space not found after creation");

    assert_eq!(fetched.id, id);
    assert_eq!(fetched.did, did);
    assert_eq!(fetched.type_nsid, "com.example.test");
    assert_eq!(fetched.skey, "myspace");
    assert_eq!(fetched.display_name, Some("Test Space".to_string()));
    assert_eq!(fetched.mint_policy, MintPolicy::MemberList);
    assert!(matches!(fetched.app_access, AppAccess::Open));
}

#[tokio::test]
#[serial]
async fn get_space_by_address_roundtrip() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let id = new_id();
    let did = "did:plc:addr-test";
    let space = make_space(&id, did, "com.example.addr", "addr-skey");
    spaces_db::create_space(&pool, backend, &space)
        .await
        .expect("create_space failed");

    let fetched =
        spaces_db::get_space_by_address(&pool, backend, did, "com.example.addr", "addr-skey")
            .await
            .expect("get_space_by_address failed")
            .expect("space not found by address");

    assert_eq!(fetched.id, id);
}

#[tokio::test]
#[serial]
async fn list_spaces_by_owner() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let owner = "did:plc:list-owner";
    let other = "did:plc:other-owner";

    for i in 0..3 {
        let space = make_space(&new_id(), owner, "com.example.list", &format!("space-{i}"));
        spaces_db::create_space(&pool, backend, &space)
            .await
            .expect("create_space failed");
    }
    let space = make_space(&new_id(), other, "com.example.list", "other-space");
    spaces_db::create_space(&pool, backend, &space)
        .await
        .expect("create_space failed");

    let owned = spaces_db::list_spaces_by_owner(&pool, backend, owner)
        .await
        .expect("list_spaces_by_owner failed");
    assert_eq!(owned.len(), 3);

    let other_owned = spaces_db::list_spaces_by_owner(&pool, backend, other)
        .await
        .expect("list_spaces_by_owner failed");
    assert_eq!(other_owned.len(), 1);
}

#[tokio::test]
#[serial]
async fn delete_space_removes_it() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let id = new_id();
    let space = make_space(&id, "did:plc:del-owner", "com.example.del", "del-skey");
    spaces_db::create_space(&pool, backend, &space)
        .await
        .expect("create_space failed");

    let deleted = spaces_db::delete_space(&pool, backend, &id)
        .await
        .expect("delete_space failed");
    assert!(deleted);

    let after = spaces_db::get_space(&pool, backend, &id)
        .await
        .expect("get_space failed");
    assert!(after.is_none());
}

// ---------------------------------------------------------------------------
// Repo state
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn get_or_create_repo_state_creates_default() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let space_id = new_id();
    let space = make_space(
        &space_id,
        "did:plc:repo-owner",
        "com.example.repo",
        "repo-skey",
    );
    spaces_db::create_space(&pool, backend, &space)
        .await
        .expect("create_space failed");

    let author_did = "did:plc:repo-author";
    let state = spaces_db::get_or_create_repo_state(&pool, backend, &space_id, author_did)
        .await
        .expect("get_or_create_repo_state failed");

    assert_eq!(state.space_id, space_id);
    assert_eq!(state.author_did, author_did);
    assert_eq!(state.lthash_state, vec![0u8; 2048]);
    assert!(state.rev.is_none());
    assert!(state.hash.is_none());
}

#[tokio::test]
#[serial]
async fn get_or_create_repo_state_is_idempotent() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let space_id = new_id();
    let space = make_space(
        &space_id,
        "did:plc:idem-owner",
        "com.example.idem",
        "idem-skey",
    );
    spaces_db::create_space(&pool, backend, &space)
        .await
        .expect("create_space failed");

    let author_did = "did:plc:idem-author";
    let first = spaces_db::get_or_create_repo_state(&pool, backend, &space_id, author_did)
        .await
        .expect("first call failed");
    let second = spaces_db::get_or_create_repo_state(&pool, backend, &space_id, author_did)
        .await
        .expect("second call failed");

    assert_eq!(first.id, second.id);
}

#[tokio::test]
#[serial]
async fn update_repo_state_persists_fields() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let space_id = new_id();
    let space = make_space(
        &space_id,
        "did:plc:update-owner",
        "com.example.upd",
        "upd-skey",
    );
    spaces_db::create_space(&pool, backend, &space)
        .await
        .expect("create_space failed");

    let author_did = "did:plc:update-author";
    let mut state = spaces_db::get_or_create_repo_state(&pool, backend, &space_id, author_did)
        .await
        .expect("get_or_create failed");

    state.rev = Some("rev-001".to_string());
    state.hash = Some(vec![0xde, 0xad, 0xbe, 0xef]);

    spaces_db::update_repo_state(&pool, backend, &state)
        .await
        .expect("update_repo_state failed");

    let reloaded = spaces_db::get_or_create_repo_state(&pool, backend, &space_id, author_did)
        .await
        .expect("reload failed");

    assert_eq!(reloaded.rev, Some("rev-001".to_string()));
    assert_eq!(reloaded.hash, Some(vec![0xde, 0xad, 0xbe, 0xef]));
}

// ---------------------------------------------------------------------------
// Oplog
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn oplog_append_and_list() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let space_id = new_id();
    let space = make_space(
        &space_id,
        "did:plc:oplog-owner",
        "com.example.oplog",
        "oplog-skey",
    );
    spaces_db::create_space(&pool, backend, &space)
        .await
        .expect("create_space failed");

    let author_did = "did:plc:oplog-author";

    for (i, action) in [
        OplogAction::Create,
        OplogAction::Update,
        OplogAction::Delete,
    ]
    .iter()
    .enumerate()
    {
        let entry = OplogEntry {
            id: new_id(),
            space_id: space_id.clone(),
            author_did: author_did.to_string(),
            rev: format!("rev-{:04}", i + 1),
            idx: 0,
            action: *action,
            collection: "com.example.item".to_string(),
            rkey: format!("item-{i}"),
            cid: Some(format!("bafy{i}")),
            prev: if i > 0 {
                Some(format!("rev-{:04}", i))
            } else {
                None
            },
            value: None,
            created_at: now_rfc3339(),
        };
        oplog::append_op(&pool, backend, &entry)
            .await
            .expect("append_op failed");
    }

    let all_ops = oplog::list_ops(&pool, backend, &space_id, author_did, None, 10)
        .await
        .expect("list_ops failed");
    assert_eq!(all_ops.len(), 3);
    assert!(matches!(all_ops[0].action, OplogAction::Create));
    assert!(matches!(all_ops[1].action, OplogAction::Update));
    assert!(matches!(all_ops[2].action, OplogAction::Delete));
}

#[tokio::test]
#[serial]
async fn oplog_list_with_since_rev_cursor() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let space_id = new_id();
    let space = make_space(
        &space_id,
        "did:plc:cursor-owner",
        "com.example.cursor",
        "cursor-skey",
    );
    spaces_db::create_space(&pool, backend, &space)
        .await
        .expect("create_space failed");

    let author_did = "did:plc:cursor-author";

    for i in 0..5 {
        let entry = OplogEntry {
            id: new_id(),
            space_id: space_id.clone(),
            author_did: author_did.to_string(),
            rev: format!("rev-{:04}", i + 1),
            idx: 0,
            action: OplogAction::Create,
            collection: "com.example.item".to_string(),
            rkey: format!("item-{i}"),
            cid: Some(format!("bafy{i}")),
            prev: None,
            value: None,
            created_at: now_rfc3339(),
        };
        oplog::append_op(&pool, backend, &entry)
            .await
            .expect("append_op failed");
    }

    let after_rev2 = oplog::list_ops(&pool, backend, &space_id, author_did, Some("rev-0002"), 10)
        .await
        .expect("list_ops with cursor failed");

    assert_eq!(after_rev2.len(), 3);
    assert_eq!(after_rev2[0].rev, "rev-0003");
}

// ---------------------------------------------------------------------------
// Notification registrations
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn register_and_list_notify_registrations() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let space_id = new_id();
    let space = make_space(
        &space_id,
        "did:plc:notify-owner",
        "com.example.notify",
        "notify-skey",
    );
    spaces_db::create_space(&pool, backend, &space)
        .await
        .expect("create_space failed");

    let service_did = "did:plc:notify-service";
    let endpoint = "https://service.example.com/notify";
    let registered_by = "did:plc:notify-owner";

    let reg_id = notifications::register(
        &pool,
        backend,
        &space_id,
        service_did,
        endpoint,
        registered_by,
    )
    .await
    .expect("register failed");
    assert!(!reg_id.is_empty());

    let all_regs = spaces_db::list_notify_registrations(&pool, backend, &space_id, None)
        .await
        .expect("list_notify_registrations failed");
    assert_eq!(all_regs.len(), 1);
    assert_eq!(all_regs[0].id, reg_id);
    assert_eq!(all_regs[0].endpoint, endpoint);
    assert_eq!(all_regs[0].author_did, Some(service_did.to_string()));

    let by_did = spaces_db::list_notify_registrations(&pool, backend, &space_id, Some(service_did))
        .await
        .expect("list_notify_registrations by did failed");
    assert_eq!(by_did.len(), 1);
}

#[tokio::test]
#[serial]
async fn delete_notify_registration_removes_it() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let space_id = new_id();
    let space = make_space(
        &space_id,
        "did:plc:del-notify-owner",
        "com.example.delnotify",
        "dn-skey",
    );
    spaces_db::create_space(&pool, backend, &space)
        .await
        .expect("create_space failed");

    let reg_id = notifications::register(
        &pool,
        backend,
        &space_id,
        "did:plc:svc",
        "https://svc.example.com/n",
        "did:plc:del-notify-owner",
    )
    .await
    .expect("register failed");

    let deleted = spaces_db::delete_notify_registration(&pool, backend, &reg_id)
        .await
        .expect("delete_notify_registration failed");
    assert!(deleted);

    let remaining = spaces_db::list_notify_registrations(&pool, backend, &space_id, None)
        .await
        .expect("list after delete failed");
    assert!(remaining.is_empty());
}

// ---------------------------------------------------------------------------
// Space members
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn add_and_get_member() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let space_id = new_id();
    let space = make_space(
        &space_id,
        "did:plc:member-owner",
        "com.example.member",
        "member-skey",
    );
    spaces_db::create_space(&pool, backend, &space)
        .await
        .expect("create_space failed");

    let member_did = "did:plc:new-member";
    let member = SpaceMember {
        id: new_id(),
        space_id: space_id.clone(),
        did: member_did.to_string(),
        access: SpaceAccess::Read,
        is_delegation: false,
        granted_by: Some("did:plc:member-owner".to_string()),
        created_at: now_rfc3339(),
    };
    spaces_db::add_member(&pool, backend, &member)
        .await
        .expect("add_member failed");

    let fetched = spaces_db::get_member(&pool, backend, &space_id, member_did)
        .await
        .expect("get_member failed")
        .expect("member not found");

    assert_eq!(fetched.did, member_did);
    assert_eq!(fetched.access, SpaceAccess::Read);
    assert!(!fetched.is_delegation);
}

#[tokio::test]
#[serial]
async fn remove_member() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let space_id = new_id();
    let space = make_space(&space_id, "did:plc:rm-owner", "com.example.rm", "rm-skey");
    spaces_db::create_space(&pool, backend, &space)
        .await
        .expect("create_space failed");

    let member_did = "did:plc:rm-member";
    let member = SpaceMember {
        id: new_id(),
        space_id: space_id.clone(),
        did: member_did.to_string(),
        access: SpaceAccess::Write,
        is_delegation: false,
        granted_by: None,
        created_at: now_rfc3339(),
    };
    spaces_db::add_member(&pool, backend, &member)
        .await
        .expect("add_member failed");

    let removed = spaces_db::remove_member(&pool, backend, &space_id, member_did)
        .await
        .expect("remove_member failed");
    assert!(removed);

    let after = spaces_db::get_member(&pool, backend, &space_id, member_did)
        .await
        .expect("get_member after remove failed");
    assert!(after.is_none());
}

// ---------------------------------------------------------------------------
// Space records
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn upsert_and_get_space_record() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let space_id = new_id();
    let space = make_space(
        &space_id,
        "did:plc:rec-owner",
        "com.example.rec",
        "rec-skey",
    );
    spaces_db::create_space(&pool, backend, &space)
        .await
        .expect("create_space failed");

    let author_did = "did:plc:rec-author";
    let uri = format!("at://{author_did}/com.example.item/rec001");
    let record = SpaceRecord {
        uri: uri.clone(),
        space_id: space_id.clone(),
        author_did: author_did.to_string(),
        collection: "com.example.item".to_string(),
        rkey: "rec001".to_string(),
        record: serde_json::json!({"title": "hello"}),
        cid: "bafycid001".to_string(),
        indexed_at: now_rfc3339(),
    };

    spaces_db::upsert_space_record(&pool, backend, &record)
        .await
        .expect("upsert_space_record failed");

    let fetched = spaces_db::get_space_record(&pool, backend, &uri)
        .await
        .expect("get_space_record failed")
        .expect("record not found");

    assert_eq!(fetched.uri, uri);
    assert_eq!(fetched.record["title"], "hello");
    assert_eq!(fetched.cid, "bafycid001");
}

#[tokio::test]
#[serial]
async fn upsert_space_record_overwrites_existing() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let space_id = new_id();
    let space = make_space(
        &space_id,
        "did:plc:upsert-owner",
        "com.example.upsert",
        "upsert-skey",
    );
    spaces_db::create_space(&pool, backend, &space)
        .await
        .expect("create_space failed");

    let uri = "at://did:plc:upsert-author/com.example.item/upd001";
    let first = SpaceRecord {
        uri: uri.to_string(),
        space_id: space_id.clone(),
        author_did: "did:plc:upsert-author".to_string(),
        collection: "com.example.item".to_string(),
        rkey: "upd001".to_string(),
        record: serde_json::json!({"v": 1}),
        cid: "cid-v1".to_string(),
        indexed_at: now_rfc3339(),
    };
    spaces_db::upsert_space_record(&pool, backend, &first)
        .await
        .expect("first upsert failed");

    let second = SpaceRecord {
        record: serde_json::json!({"v": 2}),
        cid: "cid-v2".to_string(),
        ..first
    };
    spaces_db::upsert_space_record(&pool, backend, &second)
        .await
        .expect("second upsert failed");

    let fetched = spaces_db::get_space_record(&pool, backend, uri)
        .await
        .expect("get_space_record failed")
        .expect("record not found");
    assert_eq!(fetched.record["v"], 2);
    assert_eq!(fetched.cid, "cid-v2");
}
