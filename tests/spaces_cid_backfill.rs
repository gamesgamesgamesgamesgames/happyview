mod common;

use happyview::db::now_rfc3339;
use happyview::spaces::cid_backfill;
use happyview::spaces::db as spaces_db;
use happyview::spaces::lthash::{LtHashState, record_element};
use happyview::spaces::types::*;
use serde_json::{Value, json};
use serial_test::serial;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use common::db as test_db;

const COLLECTION: &str = "com.example.item";

fn legacy_cid(record: &Value) -> String {
    let bytes = serde_json::to_vec(record).unwrap();
    let hash = Sha256::digest(&bytes);
    format!("bafyrei{}", hex::encode(&hash[..20]))
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn make_space(id: &str, did: &str, skey: &str) -> Space {
    let now = now_rfc3339();
    Space {
        id: id.to_string(),
        did: did.to_string(),
        authority_did: did.to_string(),
        creator_did: did.to_string(),
        type_nsid: "com.example.backfill".to_string(),
        skey: skey.to_string(),
        display_name: None,
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

async fn seed(
    pool: &sqlx::AnyPool,
    backend: happyview::db::DatabaseBackend,
) -> (String, String, Vec<(String, Value)>) {
    let space_id = new_id();
    let author = format!("did:plc:author{}", Uuid::new_v4().simple());
    let space = make_space(&space_id, &author, &format!("s{}", Uuid::new_v4().simple()));
    spaces_db::create_space(pool, backend, &space)
        .await
        .expect("create_space failed");

    let records = vec![
        (
            "rk-one".to_string(),
            json!({ "text": "first", "aa": 2, "b": 1 }),
        ),
        ("rk-two".to_string(), json!({ "text": "second" })),
    ];

    let mut set_hash = LtHashState::new();
    for (idx, (rkey, value)) in records.iter().enumerate() {
        let cid = legacy_cid(value);
        let rec = SpaceRecord {
            uri: format!(
                "at://{}/space/{}/{}/{}/{}/{}",
                space.did, space.type_nsid, space.skey, author, COLLECTION, rkey
            ),
            space_id: space_id.clone(),
            author_did: author.clone(),
            collection: COLLECTION.to_string(),
            rkey: rkey.clone(),
            record: value.clone(),
            cid: cid.clone(),
            indexed_at: now_rfc3339(),
        };
        spaces_db::insert_space_record(pool, backend, &rec)
            .await
            .expect("insert_space_record failed");

        set_hash.add(&record_element(COLLECTION, rkey, &cid));

        happyview::spaces::oplog::append_op(
            pool,
            backend,
            &OplogEntry {
                id: new_id(),
                space_id: space_id.clone(),
                author_did: author.clone(),
                rev: format!("rev-{idx:04}"),
                idx: 0,
                action: OplogAction::Create,
                collection: COLLECTION.to_string(),
                rkey: rkey.clone(),
                cid: Some(cid),
                prev: None,
                value: None,
                created_at: now_rfc3339(),
            },
        )
        .await
        .expect("append_op failed");
    }

    let mut conn = pool.acquire().await.expect("acquire failed");
    let mut repo_state =
        spaces_db::get_or_create_repo_state(&mut conn, backend, &space_id, &author)
            .await
            .expect("get_or_create_repo_state failed");
    let space_uri = format!(
        "at://{}/space/{}/{}",
        space.did, space.type_nsid, space.skey
    );
    let signed =
        happyview::spaces::commit::sign_commit(&set_hash.hash(), &space_uri, &author, "rev-0001")
            .expect("sign_commit failed");
    repo_state.lthash_state = set_hash.as_bytes().to_vec();
    repo_state.rev = Some(signed.rev);
    repo_state.hash = Some(signed.hash.to_vec());
    repo_state.ikm = Some(signed.ikm.to_vec());
    repo_state.mac = Some(signed.mac.to_vec());
    drop(conn);
    spaces_db::update_repo_state(pool, backend, &repo_state)
        .await
        .expect("update_repo_state failed");

    (space_id, author, records)
}

async fn stored_cid(
    pool: &sqlx::AnyPool,
    backend: happyview::db::DatabaseBackend,
    space_id: &str,
    rkey: &str,
) -> String {
    let sql = happyview::db::adapt_sql(
        "SELECT cid FROM happyview_space_records WHERE space_id = ? AND rkey = ?",
        backend,
    );
    let row: (String,) = happyview::db::query_as(&sql)
        .bind(space_id)
        .bind(rkey)
        .fetch_one(pool)
        .await
        .expect("failed to read stored cid");
    row.0
}

#[tokio::test]
#[serial]
async fn repairs_legacy_cids_oplog_and_repo_state() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let (space_id, author, records) = seed(&pool, backend).await;

    let report = cid_backfill::run(&pool, backend, false)
        .await
        .expect("backfill failed");

    assert_eq!(report.records_scanned, 2);
    assert_eq!(
        report.records_updated, 2,
        "both legacy CIDs must be replaced"
    );
    assert_eq!(report.records_unencodable, 0);
    assert_eq!(
        report.oplog_rows_remapped, 2,
        "both oplog entries must be remapped"
    );
    assert_eq!(report.repos_rebuilt, 1);

    let mut expected = LtHashState::new();
    for (rkey, value) in &records {
        let cid = stored_cid(&pool, backend, &space_id, rkey).await;
        let canonical = happyview::cid_verify::compute_record_cid(value)
            .expect("encodable")
            .to_string();
        assert_eq!(cid, canonical, "stored CID must equal the canonical CID");
        assert_ne!(cid, legacy_cid(value), "the legacy CID must be gone");
        expected.add(&record_element(COLLECTION, rkey, &cid));
    }

    let (ops, _) = happyview::spaces::oplog::list_ops(&pool, backend, &space_id, &author, None, 10)
        .await
        .expect("list_ops failed");
    assert_eq!(ops.len(), 2);
    for op in &ops {
        let cid = op.cid.as_deref().expect("op must carry a cid");
        let stored = stored_cid(&pool, backend, &space_id, &op.rkey).await;
        assert_eq!(cid, stored, "oplog cid must match the record's cid");
    }

    let mut conn = pool.acquire().await.expect("acquire failed");
    let repo_state = spaces_db::get_or_create_repo_state(&mut conn, backend, &space_id, &author)
        .await
        .expect("get_or_create_repo_state failed");
    assert_eq!(
        repo_state.hash.as_deref(),
        Some(&expected.hash()[..]),
        "repo hash must equal an LtHash rebuilt from the repaired CIDs"
    );
    assert_eq!(repo_state.lthash_state, expected.as_bytes().to_vec());
}

#[tokio::test]
#[serial]
async fn repair_is_idempotent() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    seed(&pool, backend).await;

    let first = cid_backfill::run(&pool, backend, false)
        .await
        .expect("first run failed");
    assert!(!first.is_noop());

    let second = cid_backfill::run(&pool, backend, false)
        .await
        .expect("second run failed");
    assert_eq!(second.records_updated, 0, "CIDs are already canonical");
    assert_eq!(second.oplog_rows_remapped, 0);
    assert!(
        second.is_noop(),
        "a second pass must have nothing to repair"
    );
}

#[tokio::test]
#[serial]
async fn dry_run_changes_nothing() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let (space_id, _author, records) = seed(&pool, backend).await;

    let report = cid_backfill::run(&pool, backend, true)
        .await
        .expect("dry run failed");
    assert_eq!(
        report.records_updated, 2,
        "dry run still reports what it would do"
    );

    // ...but nothing was persisted.
    for (rkey, value) in &records {
        assert_eq!(
            stored_cid(&pool, backend, &space_id, rkey).await,
            legacy_cid(value),
            "dry run must roll back"
        );
    }
}

#[tokio::test]
#[serial]
async fn run_if_needed_repairs_once_then_skips() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let (space_id, _author, records) = seed(&pool, backend).await;

    // First call: does the repair and reports it.
    let first = cid_backfill::run_if_needed(&pool, backend)
        .await
        .expect("first run_if_needed failed")
        .expect("first call must actually run");
    assert_eq!(first.records_updated, 2);

    // Records were repaired.
    for (rkey, value) in &records {
        let canonical = happyview::cid_verify::compute_record_cid(value)
            .expect("encodable")
            .to_string();
        assert_eq!(stored_cid(&pool, backend, &space_id, rkey).await, canonical);
    }

    // Second call: marker is set, so it skips entirely without scanning.
    let second = cid_backfill::run_if_needed(&pool, backend)
        .await
        .expect("second run_if_needed failed");
    assert!(
        second.is_none(),
        "a completed backfill must be skipped, not re-run"
    );
}

#[tokio::test]
#[serial]
async fn run_if_needed_marks_empty_database_done() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    let first = cid_backfill::run_if_needed(&pool, backend)
        .await
        .expect("first run_if_needed failed")
        .expect("first call runs even on an empty database");
    assert!(first.is_noop());

    let second = cid_backfill::run_if_needed(&pool, backend)
        .await
        .expect("second run_if_needed failed");
    assert!(second.is_none(), "empty database must be marked done");
}

#[tokio::test]
#[serial]
async fn dry_run_does_not_set_the_marker() {
    common::require_db!();
    let pool = test_db::test_pool().await;
    let backend = test_db::test_backend();
    test_db::truncate_all(&pool).await;

    seed(&pool, backend).await;

    cid_backfill::run(&pool, backend, true)
        .await
        .expect("dry run failed");

    // The guarded entry point must still see work to do.
    let report = cid_backfill::run_if_needed(&pool, backend)
        .await
        .expect("run_if_needed failed")
        .expect("dry run must not have set the marker");
    assert_eq!(report.records_updated, 2);
}
