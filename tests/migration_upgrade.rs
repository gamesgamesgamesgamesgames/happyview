//! Upgrade migration tests: verify the current migration set applies cleanly on
//! top of an *old* database (schema + data), not just a fresh one.
//!
//! Baseline is **v2.0.0**: we stage the migrations that shipped in v2.0.0 (those
//! whose timestamp is ≤ the v2.0.0 cutoff), apply them to an empty database to
//! reproduce a v2.0.0 install, seed a row, then run the full current migration
//! set and assert the upgrade succeeds and the data survived (e.g. across the
//! `records` → `happyview_records` prefix rename).
//!
//! - SQLite runs everywhere (isolated temp-file database).
//! - Postgres runs only when `TEST_DATABASE_URL` points at Postgres; it spins up
//!   a throwaway database so it never touches the shared test schema.

use sqlx::AnyPool;
use sqlx::migrate::Migrator;
use std::path::Path;

mod common;

/// Timestamp of v2.0.0's final migration (`20260416000003_add_dpop_session_token_hash`).
/// Migrations with a timestamp ≤ this constitute the v2.0.0 baseline.
const V2_0_0_CUTOFF: &str = "20260416000003";

// Fixed test row, written into the pre-rename `records` table and expected to
// survive into `happyview_records`. Literal SQL keeps it backend-agnostic
// (`'{"v":1}'` is a valid string literal for both TEXT and Postgres JSONB).
// All columns are set explicitly (an ISO-8601 string both TEXT and TIMESTAMPTZ
// accept) rather than relying on defaults, so the seed doesn't depend on the
// baseline's default state.
const SEED_INSERT: &str = "INSERT INTO records (uri, did, collection, rkey, record, cid, indexed_at, created_at) \
     VALUES ('at://did:plc:legacy/app.test.post/rk1', 'did:plc:legacy', 'app.test.post', 'rk1', '{\"v\":1}', 'bafylegacycid', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')";
const SEED_ASSERT: &str =
    "SELECT did FROM happyview_records WHERE uri = 'at://did:plc:legacy/app.test.post/rk1'";

// A backfill job from before the bounded queue. A UUID literal, since
// v2.0.0's Postgres `backfill_jobs.id` is a UUID column.
const SEED_JOB_INSERT: &str = "INSERT INTO backfill_jobs (id, status, created_at) \
     VALUES ('00000000-0000-0000-0000-00000000b001', 'running', '2026-01-01T00:00:00Z')";
const SEED_JOB_ASSERT: &str = "SELECT queue_version, discovery_complete FROM happyview_backfill_jobs \
     WHERE id = '00000000-0000-0000-0000-00000000b001'";

/// Copy every `.sql` migration in `src` whose 14-digit timestamp prefix is
/// ≤ `cutoff` into `dest`, reproducing the migration set of that release.
fn stage_baseline_migrations(src: &str, dest: &Path, cutoff: &str) {
    std::fs::create_dir_all(dest).expect("create staging dir");
    let mut staged = 0usize;
    for entry in std::fs::read_dir(src).expect("read migrations dir") {
        let entry = entry.expect("dir entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".sql") || name.len() < 14 {
            continue;
        }
        if &name[..14] <= cutoff {
            std::fs::copy(entry.path(), dest.join(&name)).expect("copy migration");
            staged += 1;
        }
    }
    assert!(staged > 0, "no baseline migrations staged from {src}");
}

/// Reproduce a v2.0.0 install on `pool`, seed a row, then run the current
/// migrations and the record store upgrade on top and assert the row
/// survived.
async fn assert_upgrade_preserves_data(
    pool: &AnyPool,
    migrations_dir: &str,
    backend: happyview::db::DatabaseBackend,
) {
    let baseline_dir = std::env::temp_dir().join(format!("hv-baseline-{}", uuid::Uuid::new_v4()));
    stage_baseline_migrations(migrations_dir, &baseline_dir, V2_0_0_CUTOFF);

    // 1. Reproduce a v2.0.0 install.
    Migrator::new(baseline_dir.as_path())
        .await
        .expect("load baseline migrations")
        .run(pool)
        .await
        .expect("v2.0.0 migrations should apply to an empty database");

    // 2. Seed a record using the v2.0.0 `records` schema (pre prefix-rename).
    sqlx::query(SEED_INSERT)
        .execute(pool)
        .await
        .expect("seed a v2.0.0-era record");
    sqlx::query(SEED_JOB_INSERT)
        .execute(pool)
        .await
        .expect("seed a v2.0.0-era backfill job");

    // 3. Upgrade: apply the full current migration set on top of the v2.0.0 schema.
    Migrator::new(Path::new(migrations_dir))
        .await
        .expect("load current migrations")
        .run(pool)
        .await
        .expect("current migrations should apply on top of the v2.0.0 schema");

    // 4. The seeded row survived into the renamed table.
    let (did,): (String,) = sqlx::query_as(SEED_ASSERT)
        .fetch_one(pool)
        .await
        .expect("seeded record should survive the upgrade");
    assert_eq!(did, "did:plc:legacy");

    // 5. A job from before the bounded queue keeps its repo rows and needs no
    //    discovery: it finishes on the legacy path.
    let (queue_version, discovery_complete): (i32, i32) = sqlx::query_as(SEED_JOB_ASSERT)
        .fetch_one(pool)
        .await
        .expect("seeded backfill job should survive the upgrade");
    assert_eq!((queue_version, discovery_complete), (1, 1));

    // 6. The record store upgrade moves it into the compact tables, and the
    //    view reads back the same row and the same text.
    const SEED_RECORD: &str = "SELECT CAST(record AS TEXT) FROM happyview_records \
         WHERE uri = 'at://did:plc:legacy/app.test.post/rk1'";
    let (before,): (String,) = sqlx::query_as(SEED_RECORD)
        .fetch_one(pool)
        .await
        .expect("read the record before the upgrade");
    let outcome = happyview::record_store::upgrade::run(pool, backend)
        .await
        .expect("the record store upgrade");
    assert!(
        matches!(
            outcome,
            happyview::record_store::upgrade::UpgradeOutcome::Upgraded { records: 1, .. }
        ),
        "{outcome:?}"
    );
    let (did,): (String,) = sqlx::query_as(SEED_ASSERT)
        .fetch_one(pool)
        .await
        .expect("the record reads through the view");
    assert_eq!(did, "did:plc:legacy");
    let (after,): (String,) = sqlx::query_as(SEED_RECORD)
        .fetch_one(pool)
        .await
        .expect("read the record after the upgrade");
    assert_eq!(after, before);

    let _ = std::fs::remove_dir_all(&baseline_dir);
}

#[tokio::test]
async fn sqlite_upgrade_from_v2_0_0_applies_and_preserves_data() {
    happyview::db::install_drivers();

    let tmp_db = std::env::temp_dir().join(format!("hv-upgrade-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}?mode=rwc", tmp_db.display());
    let pool = AnyPool::connect(&url)
        .await
        .expect("connect to fresh sqlite database");

    assert_upgrade_preserves_data(
        &pool,
        "migrations/sqlite",
        happyview::db::DatabaseBackend::Sqlite,
    )
    .await;

    pool.close().await;
    let _ = std::fs::remove_file(&tmp_db);
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_upgrade_from_v2_0_0_applies_and_preserves_data() {
    common::require_db!();
    happyview::db::install_drivers();
    if common::db::test_backend() != happyview::db::DatabaseBackend::Postgres {
        eprintln!("skipped (TEST_DATABASE_URL is not Postgres)");
        return;
    }

    let base_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
    let (server, _db) = base_url
        .rsplit_once('/')
        .expect("TEST_DATABASE_URL should have a database path");
    let scratch_db = format!("hv_upgrade_{}", uuid::Uuid::new_v4().simple());

    // Create a throwaway database on the same server so the upgrade runs in
    // isolation from the shared (already-migrated) test schema.
    let admin = AnyPool::connect(&base_url)
        .await
        .expect("connect to admin database");
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {scratch_db}")))
        .execute(&admin)
        .await
        .expect("create scratch database");

    let scratch_url = format!("{server}/{scratch_db}");
    let pool = AnyPool::connect(&scratch_url)
        .await
        .expect("connect to scratch database");

    // Run the upgrade, but catch a panic so the scratch database is always
    // dropped even when an assertion fails — then re-raise so the test still fails.
    use futures::FutureExt;
    let outcome = std::panic::AssertUnwindSafe(assert_upgrade_preserves_data(
        &pool,
        "migrations/postgres",
        happyview::db::DatabaseBackend::Postgres,
    ))
    .catch_unwind()
    .await;

    pool.close().await;
    // FORCE terminates any lingering backend connections so the drop can't hang.
    let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE {scratch_db} WITH (FORCE)"
    )))
    .execute(&admin)
    .await;
    admin.close().await;

    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// The migration before `20260915000000_backfill_job_scope`.
const PRE_SCOPE_CUTOFF: &str = "20260913000001";

#[tokio::test]
async fn sqlite_backfill_scope_marks_existing_single_did_jobs() {
    happyview::db::install_drivers();

    let tmp_db = std::env::temp_dir().join(format!("hv-scope-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}?mode=rwc", tmp_db.display());
    let pool = AnyPool::connect(&url)
        .await
        .expect("connect to fresh sqlite database");

    let baseline_dir = std::env::temp_dir().join(format!("hv-scope-base-{}", uuid::Uuid::new_v4()));
    stage_baseline_migrations("./migrations/sqlite", &baseline_dir, PRE_SCOPE_CUTOFF);
    Migrator::new(baseline_dir.as_path())
        .await
        .expect("load pre-scope migrations")
        .run(&pool)
        .await
        .expect("pre-scope migrations apply");

    sqlx::query(
        "INSERT INTO happyview_backfill_jobs (id, collection, did, status, stage, created_at) VALUES \
         ('network-job', NULL, NULL, 'completed', 'completed', '2026-01-01T00:00:00Z'), \
         ('did-job', NULL, 'did:plc:legacy', 'completed', 'completed', '2026-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .expect("seed legacy jobs");

    Migrator::new(Path::new("./migrations/sqlite"))
        .await
        .expect("load current migrations")
        .run(&pool)
        .await
        .expect("current migrations apply");

    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT id, scope FROM happyview_backfill_jobs ORDER BY id")
            .fetch_all(&pool)
            .await
            .expect("read scopes");
    assert_eq!(
        rows,
        vec![
            ("did-job".to_string(), "dids".to_string()),
            ("network-job".to_string(), "network".to_string()),
        ]
    );

    pool.close().await;
    let _ = std::fs::remove_dir_all(&baseline_dir);
    let _ = std::fs::remove_file(&tmp_db);
}

/// A throwaway database on the `TEST_DATABASE_URL` Postgres server, handed to
/// `body` by URL and dropped afterwards even when `body` panics. `None` when
/// the URL names no Postgres server, which the caller reports as a skip.
async fn with_scratch_postgres<F, Fut>(prefix: &str, body: F) -> Option<()>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let Ok(base_url) = std::env::var("TEST_DATABASE_URL") else {
        happyview::test_support::unavailable(happyview::test_support::NO_DATABASE);
        return None;
    };
    happyview::db::install_drivers();
    if happyview::db::DatabaseBackend::from_url(&base_url)
        != happyview::db::DatabaseBackend::Postgres
    {
        return None;
    }
    let (server, _db) = base_url
        .rsplit_once('/')
        .expect("TEST_DATABASE_URL should have a database path");
    let scratch_db = format!("{prefix}_{}", uuid::Uuid::new_v4().simple());
    let admin = AnyPool::connect(&base_url)
        .await
        .expect("connect to admin database");
    happyview::db::query(&format!("CREATE DATABASE {scratch_db}"))
        .execute(&admin)
        .await
        .expect("create scratch database");

    use futures::FutureExt;
    let outcome = std::panic::AssertUnwindSafe(body(format!("{server}/{scratch_db}")))
        .catch_unwind()
        .await;

    let _ = happyview::db::query(&format!("DROP DATABASE {scratch_db} WITH (FORCE)"))
        .execute(&admin)
        .await;
    admin.close().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
    Some(())
}

/// Instances booting together on a fresh database all come up. sqlx's own
/// migration lock blocks inside a statement, which a `CREATE INDEX
/// CONCURRENTLY` in another session waits out while that session holds the
/// lock: Postgres reported a deadlock and failed one of them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn postgres_instances_migrating_at_once_all_boot() {
    let ran = with_scratch_postgres("hv_concurrent", |url| async move {
        let boots = (0..4).map(|_| {
            let url = url.clone();
            tokio::spawn(async move {
                let pool =
                    happyview::db::connect(&url, happyview::db::DatabaseBackend::Postgres).await;
                pool.close().await;
            })
        });
        for boot in futures::future::join_all(boots).await {
            boot.expect("every instance migrates and boots");
        }
    })
    .await;
    if ran.is_none() {
        eprintln!("skipped (TEST_DATABASE_URL is not Postgres)");
    }
}

/// The last migration before the concurrently built records index.
const PRE_INDEX_CUTOFF: &str = "20261009000000";

/// A `CREATE INDEX CONCURRENTLY` that fails partway leaves an invalid index of
/// its name, and sqlx records nothing, so the migration runs again at the next
/// boot. That run must build the index rather than let `IF NOT EXISTS` keep
/// the invalid one, and the boot then goes on to the record store upgrade.
#[tokio::test]
#[serial_test::serial]
async fn postgres_boot_rebuilds_an_index_a_failed_concurrent_build_left_invalid() {
    let ran = with_scratch_postgres("hv_invalid_index", |url| async move {
        use happyview::db::{self, DatabaseBackend};
        let staged = std::env::temp_dir().join(format!("hv-pre-index-{}", uuid::Uuid::new_v4()));
        stage_baseline_migrations("migrations/postgres", &staged, PRE_INDEX_CUTOFF);
        let pool = AnyPool::connect(&url).await.expect("connect");
        Migrator::new(staged.as_path())
            .await
            .expect("load the migrations before the index")
            .run(&pool)
            .await
            .expect("migrate to before the index");
        let _ = std::fs::remove_dir_all(&staged);

        // Fail a concurrent build of the index's name: two rows share a
        // collection, so a unique build on it fails and leaves it invalid.
        db::query(
            "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, created_at) VALUES \
             ('at://did:plc:a/app.test.post/1', 'did:plc:a', 'app.test.post', '1', '{\"n\":1}', 'c1', NOW()), \
             ('at://did:plc:a/app.test.post/2', 'did:plc:a', 'app.test.post', '2', '{\"n\":2}', 'c2', NOW())",
        )
        .execute(&pool)
        .await
        .expect("seed records");
        db::query(
            "CREATE UNIQUE INDEX CONCURRENTLY idx_records_collection_created_at_uri \
             ON happyview_records (collection)",
        )
        .execute(&pool)
        .await
        .expect_err("a unique build over duplicates fails");
        let (invalid,): (i64,) = db::query_as(
            "SELECT COUNT(*) FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid \
             WHERE c.relname = 'idx_records_collection_created_at_uri' AND NOT i.indisvalid",
        )
        .fetch_one(&pool)
        .await
        .expect("find the invalid index");
        assert_eq!(invalid, 1, "the failed build left an invalid index");
        pool.close().await;

        let pool = db::connect(&url, DatabaseBackend::Postgres).await;
        let (applied,): (i64,) = db::query_as(
            "SELECT COUNT(*) FROM _sqlx_migrations WHERE version BETWEEN 20261009000001 AND 20261009000003",
        )
        .fetch_one(&pool)
        .await
        .expect("count migrations");
        assert_eq!(applied, 3);
        assert!(
            !happyview::record_store::upgrade::legacy_tables_present(&pool, DatabaseBackend::Postgres)
                .await
                .expect("check"),
            "the boot went on to the record store upgrade"
        );
        let store = happyview::record_store::RecordStore::new(pool.clone(), DatabaseBackend::Postgres);
        for (rkey, body) in [("1", r#"{"n":1}"#), ("2", r#"{"n":2}"#)] {
            let got = store
                .get(&format!("at://did:plc:a/app.test.post/{rkey}"))
                .await
                .expect("get")
                .expect("the record survived");
            assert_eq!(got.record, body);
        }
        pool.close().await;
    })
    .await;
    if ran.is_none() {
        eprintln!("skipped (TEST_DATABASE_URL is not Postgres)");
    }
}

/// Seed the legacy tables of a migrated (not yet upgraded) database, then
/// upgrade, and read every record back through the view and the store.
async fn assert_records_survive_the_compact_upgrade(
    pool: &AnyPool,
    backend: happyview::db::DatabaseBackend,
) {
    use happyview::db::adapt_sql;
    use happyview::record_store::{RecordStore, upgrade};
    const CID: &str = "bafyreie5737gdxlw5i64vzichcalba3z2v5n6icifvx5xytvske7mr3hpm";
    let insert = adapt_sql(
        "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at) \
         VALUES (?, ?, 'app.test.post', ?, ?, ?, ?, ?)",
        backend,
    );
    let mut expected = Vec::new();
    for i in 0..30 {
        let uri = format!("at://did:plc:mu{}/app.test.post/r{i:02}", i % 4);
        let record = format!(r#"{{"z":{i},"a":"é\n","via":"witchsky"}}"#);
        happyview::db::query(&insert)
            .bind(&uri)
            .bind(format!("did:plc:mu{}", i % 4))
            .bind(format!("r{i:02}"))
            .bind(&record)
            .bind(CID)
            .bind((i % 3 != 0).then_some("2026-01-01T00:00:00+00:00"))
            .bind("2026-01-01T00:00:00+00:00")
            .execute(pool)
            .await
            .expect("seed");
        expected.push((uri, record));
    }

    // An interrupted copy: one batch, then the boot-time upgrade resumes it.
    let interner = happyview::record_store::intern::Interner::new(64);
    upgrade::copy_records(pool, backend, &interner, 10, Some(1))
        .await
        .expect("one batch");
    let outcome = upgrade::run(pool, backend).await.expect("upgrade");
    assert!(
        matches!(
            outcome,
            upgrade::UpgradeOutcome::Upgraded { records: 30, .. }
        ),
        "{outcome:?}"
    );
    assert!(
        !upgrade::legacy_tables_present(pool, backend)
            .await
            .expect("check")
    );
    assert_eq!(
        upgrade::run(pool, backend).await.expect("again"),
        upgrade::UpgradeOutcome::AlreadyDone
    );

    let store = RecordStore::new(pool.clone(), backend);
    for (uri, record) in &expected {
        let got = store.get(uri).await.expect("get").expect("present");
        assert_eq!(&got.record, record, "{uri}");
        assert_eq!(got.cid.as_deref(), Some(CID));
    }
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM happyview_records")
        .fetch_one(pool)
        .await
        .expect("count through the view");
    assert_eq!(n, expected.len() as i64);
}

/// A forced mismatch: a row written behind the copy stops the swap, drops
/// nothing, and returns promptly (the reset must not wait on the refused
/// swap's locks).
async fn assert_a_row_added_behind_the_copy_stops_the_swap(
    pool: &AnyPool,
    backend: happyview::db::DatabaseBackend,
) {
    use happyview::db::adapt_sql;
    use happyview::record_store::upgrade;
    let insert = adapt_sql(
        "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, created_at) \
         VALUES (?, ?, 'app.test.post', ?, '{}', 'c', '2026-01-01T00:00:00Z')",
        backend,
    );
    for did in ["did:plc:m", "did:plc:n"] {
        happyview::db::query(&insert)
            .bind(format!("at://{did}/app.test.post/r"))
            .bind(did)
            .bind("r")
            .execute(pool)
            .await
            .expect("seed");
    }
    let interner = happyview::record_store::intern::Interner::new(8);
    upgrade::copy_records(pool, backend, &interner, 100, None)
        .await
        .expect("copy");
    // An old instance writes a row that sorts before the copy's cursor.
    happyview::db::query(&insert)
        .bind("at://did:plc:a/app.test.post/r")
        .bind("did:plc:a")
        .bind("r")
        .execute(pool)
        .await
        .expect("late write");
    let err = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        upgrade::run(pool, backend),
    )
    .await
    .expect("the refused swap returns")
    .expect_err("the swap is refused");
    assert!(
        err.to_string()
            .contains("app.test.post: 3 legacy rows, 2 copied"),
        "{err}"
    );
    assert!(
        upgrade::legacy_tables_present(pool, backend)
            .await
            .expect("check"),
        "nothing dropped"
    );
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM happyview_records")
        .fetch_one(pool)
        .await
        .expect("count the legacy rows");
    assert_eq!(n, 3);
}

#[tokio::test]
async fn sqlite_records_survive_the_compact_upgrade() {
    happyview::db::install_drivers();
    for check in [0, 1] {
        let tmp_db = std::env::temp_dir().join(format!("hv-compact-{}.db", uuid::Uuid::new_v4()));
        let pool = AnyPool::connect(&format!("sqlite://{}?mode=rwc", tmp_db.display()))
            .await
            .expect("connect");
        Migrator::new(Path::new("migrations/sqlite"))
            .await
            .expect("load")
            .run(&pool)
            .await
            .expect("migrate");
        match check {
            0 => {
                assert_records_survive_the_compact_upgrade(
                    &pool,
                    happyview::db::DatabaseBackend::Sqlite,
                )
                .await
            }
            _ => {
                assert_a_row_added_behind_the_copy_stops_the_swap(
                    &pool,
                    happyview::db::DatabaseBackend::Sqlite,
                )
                .await
            }
        }
        pool.close().await;
        let _ = std::fs::remove_file(&tmp_db);
    }
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_records_survive_the_compact_upgrade() {
    for check in [0, 1] {
        let ran = with_scratch_postgres("hv_compact", |url| async move {
            let pool = AnyPool::connect(&url).await.expect("connect");
            Migrator::new(Path::new("migrations/postgres"))
                .await
                .expect("load")
                .run(&pool)
                .await
                .expect("migrate");
            match check {
                0 => {
                    assert_records_survive_the_compact_upgrade(
                        &pool,
                        happyview::db::DatabaseBackend::Postgres,
                    )
                    .await
                }
                _ => {
                    assert_a_row_added_behind_the_copy_stops_the_swap(
                        &pool,
                        happyview::db::DatabaseBackend::Postgres,
                    )
                    .await
                }
            }
            pool.close().await;
        })
        .await;
        if ran.is_none() {
            eprintln!("skipped (TEST_DATABASE_URL is not Postgres)");
            return;
        }
    }
}

/// The last migration before Phase 2a's tombstones.
const PRE_TOMBSTONES_CUTOFF: &str = "20261009000003";

/// The tombstone release's last migration. The rollback starts from that
/// release rather than from the current set, so a later migration does not
/// leave a row the previous release cannot account for.
const TOMBSTONES_CUTOFF: &str = "20261010000001";

/// The documented rollback from the tombstone release, then an upgrade again:
/// delete the two migrations' rows so the older binary boots; on SQLite, drop
/// `rev` before upgrading again. The tables the rollback left must not stop
/// the second upgrade, and what they hold survives it.
async fn assert_rollback_then_upgrade(pool: &AnyPool, migrations_dir: &str, sqlite: bool) {
    let tombstones_dir =
        std::env::temp_dir().join(format!("hv-tombstones-{}", uuid::Uuid::new_v4()));
    stage_baseline_migrations(migrations_dir, &tombstones_dir, TOMBSTONES_CUTOFF);
    Migrator::new(tombstones_dir.as_path())
        .await
        .expect("load the tombstone release's migrations")
        .run(pool)
        .await
        .expect("upgrade");
    let _ = std::fs::remove_dir_all(&tombstones_dir);
    let current = Migrator::new(Path::new(migrations_dir))
        .await
        .expect("load current migrations");
    sqlx::query(
        "INSERT INTO happyview_record_tombstones (uri, rev, deleted_at) \
         VALUES ('at://did:plc:rb/app.test.post/1', '3l', '2026-10-10T00:00:00+00:00')",
    )
    .execute(pool)
    .await
    .expect("seed a tombstone");

    sqlx::query("DELETE FROM _sqlx_migrations WHERE version IN (20261010000000, 20261010000001)")
        .execute(pool)
        .await
        .expect("roll back");
    let previous_dir =
        std::env::temp_dir().join(format!("hv-pre-tombstones-{}", uuid::Uuid::new_v4()));
    stage_baseline_migrations(migrations_dir, &previous_dir, PRE_TOMBSTONES_CUTOFF);
    Migrator::new(previous_dir.as_path())
        .await
        .expect("load the previous release's migrations")
        .run(pool)
        .await
        .expect("the previous release boots after the rollback");

    if sqlite {
        sqlx::query("ALTER TABLE happyview_records DROP COLUMN rev")
            .execute(pool)
            .await
            .expect("drop rev before upgrading again");
    }
    current.run(pool).await.expect("upgrade again");
    let (tombstones,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM happyview_record_tombstones")
        .fetch_one(pool)
        .await
        .expect("count tombstones");
    assert_eq!(tombstones, 1);
    sqlx::query("SELECT rev FROM happyview_records LIMIT 1")
        .fetch_optional(pool)
        .await
        .expect("rev is back");

    let _ = std::fs::remove_dir_all(&previous_dir);
    let _ = std::fs::remove_dir_all(&tombstones_dir);
}

#[tokio::test]
async fn sqlite_rollback_from_the_tombstones_then_upgrade_again() {
    happyview::db::install_drivers();
    let tmp_db = std::env::temp_dir().join(format!("hv-rollback-{}.db", uuid::Uuid::new_v4()));
    let pool = AnyPool::connect(&format!("sqlite://{}?mode=rwc", tmp_db.display()))
        .await
        .expect("connect to fresh sqlite database");
    assert_rollback_then_upgrade(&pool, "migrations/sqlite", true).await;
    pool.close().await;
    let _ = std::fs::remove_file(&tmp_db);
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_rollback_from_the_tombstones_then_upgrade_again() {
    let ran = with_scratch_postgres("hv_rollback", |url| async move {
        let pool = AnyPool::connect(&url).await.expect("connect");
        assert_rollback_then_upgrade(&pool, "migrations/postgres", false).await;
        pool.close().await;
    })
    .await;
    if ran.is_none() {
        eprintln!("skipped (TEST_DATABASE_URL is not Postgres)");
    }
}

/// The last migration before clients could hold several secrets.
const PRE_CLIENT_SECRETS_CUTOFF: &str = "20261028000001";

/// A client's secret from before the secrets table becomes its "Original"
/// credential with the hash copied byte for byte; the plaintext was never
/// stored, so a re-hash could not reproduce it. A client with no secret gets
/// no credential.
async fn assert_client_secrets_carry_over(pool: &AnyPool, migrations_dir: &str) {
    let baseline_dir =
        std::env::temp_dir().join(format!("hv-pre-secrets-{}", uuid::Uuid::new_v4()));
    stage_baseline_migrations(migrations_dir, &baseline_dir, PRE_CLIENT_SECRETS_CUTOFF);
    Migrator::new(baseline_dir.as_path())
        .await
        .expect("load pre-secrets migrations")
        .run(pool)
        .await
        .expect("pre-secrets migrations apply");

    sqlx::query(
        "INSERT INTO happyview_api_clients (id, client_key, client_secret_hash, name, client_id_url, client_uri, redirect_uris, client_type, created_by, created_at, updated_at) VALUES \
         ('conf', 'hvc_conf', 'b5bb9d8014a0f9b1d61e21e796d78dccdf1352f23cd32812f4850b878ae4944c', 'Confidential', 'https://conf.example/meta.json', 'https://conf.example', '[]', 'confidential', 'did:plc:admin', '2026-05-01T00:00:00+00:00', '2026-05-01T00:00:00+00:00'), \
         ('pub', 'hvc_pub', '', 'Public', 'https://pub.example/meta.json', 'https://pub.example', '[]', 'public', 'did:plc:admin', '2026-05-01T00:00:00+00:00', '2026-05-01T00:00:00+00:00')",
    )
    .execute(pool)
    .await
    .expect("seed clients");

    Migrator::new(Path::new(migrations_dir))
        .await
        .expect("load current migrations")
        .run(pool)
        .await
        .expect("current migrations apply");

    type Row = (
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
    );
    let rows: Vec<Row> =
        sqlx::query_as(
            "SELECT client_id, label, secret_hash, created_at, last_used_at, revoked_at FROM happyview_api_client_secrets ORDER BY client_id",
        )
        .fetch_all(pool)
        .await
        .expect("read client secrets");
    assert_eq!(
        rows,
        vec![(
            "conf".to_string(),
            "Original".to_string(),
            "b5bb9d8014a0f9b1d61e21e796d78dccdf1352f23cd32812f4850b878ae4944c".to_string(),
            "2026-05-01T00:00:00+00:00".to_string(),
            None,
            None,
        )]
    );

    let _ = std::fs::remove_dir_all(&baseline_dir);
}

#[tokio::test]
async fn sqlite_client_secrets_carry_over() {
    happyview::db::install_drivers();
    let tmp_db = std::env::temp_dir().join(format!("hv-secrets-{}.db", uuid::Uuid::new_v4()));
    let pool = AnyPool::connect(&format!("sqlite://{}?mode=rwc", tmp_db.display()))
        .await
        .expect("connect to fresh sqlite database");
    assert_client_secrets_carry_over(&pool, "migrations/sqlite").await;
    pool.close().await;
    let _ = std::fs::remove_file(&tmp_db);
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_client_secrets_carry_over() {
    let ran = with_scratch_postgres("hv_secrets", |url| async move {
        let pool = AnyPool::connect(&url).await.expect("connect");
        assert_client_secrets_carry_over(&pool, "migrations/postgres").await;
        pool.close().await;
    })
    .await;
    if ran.is_none() {
        eprintln!("skipped (TEST_DATABASE_URL is not Postgres)");
    }
}

// ---------------------------------------------------------------------------
// A database a stable branch migrated boots on this one
// ---------------------------------------------------------------------------

/// `(stable version, alpha version, description)` for each migration a stable
/// branch ran under a version this branch gives to another file.
fn renumbered() -> Vec<(String, String, String)> {
    std::fs::read_to_string("migrations/renumbered.tsv")
        .expect("read migrations/renumbered.tsv")
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f[0].to_string(), f[1].to_string(), f[2].to_string())
        })
        .collect()
}

/// The migration directories of each stable branch, as `(label, dir)` where
/// `dir/<backend>` holds that backend's files.
///
/// `STABLE_MIGRATIONS` names a directory holding `<branch>/migrations`, as
/// `git archive origin/<branch> migrations` writes it, which is what CI
/// supplies. Without it, a beta directory is built from this tree: every
/// migration before the first one beta added, then the files beta ran at the
/// versions beta ran them, which `migrations/renumbered.tsv` records.
fn stable_migration_dirs() -> Vec<(String, std::path::PathBuf)> {
    if let Ok(root) = std::env::var("STABLE_MIGRATIONS") {
        let mut dirs: Vec<_> = std::fs::read_dir(&root)
            .expect("read STABLE_MIGRATIONS")
            .map(|e| e.expect("dir entry"))
            .map(|e| {
                (
                    e.file_name().to_string_lossy().into_owned(),
                    e.path().join("migrations"),
                )
            })
            .collect();
        dirs.sort();
        assert!(!dirs.is_empty(), "STABLE_MIGRATIONS holds no branches");
        return dirs;
    }
    eprintln!("STABLE_MIGRATIONS unset: building a beta directory from this tree");
    let out = std::env::temp_dir().join(format!("hv-beta-{}", uuid::Uuid::new_v4()));
    for backend in ["sqlite", "postgres"] {
        let dest = out.join(backend);
        stage_baseline_migrations(&format!("migrations/{backend}"), &dest, "20261005999999");
        for entry in std::fs::read_dir(format!("migrations/{backend}")).expect("read") {
            let name = entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned();
            if name.contains("space_access") {
                let mut stable_name = name.clone();
                for (from, to, _) in renumbered() {
                    if name.starts_with(&to) {
                        stable_name = format!("{from}{}", &name[to.len()..]);
                    }
                }
                std::fs::copy(
                    format!("migrations/{backend}/{name}"),
                    dest.join(stable_name),
                )
                .expect("copy");
            }
        }
    }
    vec![("beta (from this tree)".into(), out)]
}

async fn applied(pool: &AnyPool) -> Vec<(i64, String)> {
    happyview::db::query_as("SELECT version, description FROM _sqlx_migrations ORDER BY version")
        .fetch_all(pool)
        .await
        .expect("read _sqlx_migrations")
}

/// Migrate `url` as the stable branch in `dir` would have, then boot this
/// branch on it, twice, and check every migration of this branch is recorded
/// once under its own version.
async fn assert_stable_database_boots(
    url: &str,
    dir: &std::path::Path,
    backend: happyview::db::DatabaseBackend,
) {
    let sub = match backend {
        happyview::db::DatabaseBackend::Sqlite => "sqlite",
        happyview::db::DatabaseBackend::Postgres => "postgres",
    };
    let pool = AnyPool::connect(url).await.expect("connect");
    Migrator::new(dir.join(sub))
        .await
        .expect("load the stable migrations")
        .run(&pool)
        .await
        .expect("the stable branch migrates");
    pool.close().await;

    for _ in 0..2 {
        happyview::db::connect(url, backend).await.close().await;
    }

    let pool = AnyPool::connect(url).await.expect("connect");
    let ours = Migrator::new(Path::new(&format!("migrations/{sub}")))
        .await
        .expect("load");
    let rows = applied(&pool).await;
    for m in ours.iter() {
        let recorded: Vec<_> = rows.iter().filter(|(v, _)| *v == m.version).collect();
        assert_eq!(
            recorded,
            vec![&(m.version, m.description.to_string())],
            "{sub}: version {}",
            m.version
        );
    }
    pool.close().await;
}

#[tokio::test]
async fn sqlite_databases_from_stable_branches_boot() {
    happyview::db::install_drivers();
    for (label, dir) in stable_migration_dirs() {
        let path = std::env::temp_dir().join(format!("hv-stable-{}.db", uuid::Uuid::new_v4()));
        let url = format!("sqlite://{}?mode=rwc", path.display());
        eprintln!("{label} -> this branch (sqlite)");
        assert_stable_database_boots(&url, &dir, happyview::db::DatabaseBackend::Sqlite).await;
        let _ = std::fs::remove_file(&path);
    }
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_databases_from_stable_branches_boot() {
    for (label, dir) in stable_migration_dirs() {
        eprintln!("{label} -> this branch (postgres)");
        let ran = with_scratch_postgres("hv_stable", |url| async move {
            assert_stable_database_boots(&url, &dir, happyview::db::DatabaseBackend::Postgres)
                .await;
        })
        .await;
        if ran.is_none() {
            eprintln!("skipped (TEST_DATABASE_URL is not Postgres)");
            return;
        }
    }
}

/// A database this branch created holds another migration at a renumbered
/// stable version. Booting leaves every row of it as it was.
#[tokio::test]
async fn sqlite_boot_leaves_this_branchs_own_migrations_alone() {
    happyview::db::install_drivers();
    let path = std::env::temp_dir().join(format!("hv-own-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}?mode=rwc", path.display());
    happyview::db::connect(&url, happyview::db::DatabaseBackend::Sqlite)
        .await
        .close()
        .await;
    let pool = AnyPool::connect(&url).await.expect("connect");
    let checksums = |pool: AnyPool| async move {
        let rows: Vec<(i64, String, Vec<u8>)> = happyview::db::query_as(
            "SELECT version, description, checksum FROM _sqlx_migrations ORDER BY version",
        )
        .fetch_all(&pool)
        .await
        .expect("read");
        rows
    };
    let before = checksums(pool.clone()).await;
    for (from, _, description) in renumbered() {
        let at: i64 = from.parse().unwrap();
        let row = before
            .iter()
            .find(|(v, _, _)| *v == at)
            .expect("a row at the stable version");
        assert_ne!(row.1, description, "this branch's own migration is at {at}");
    }
    happyview::db::connect(&url, happyview::db::DatabaseBackend::Sqlite)
        .await
        .close()
        .await;
    assert_eq!(checksums(pool.clone()).await, before);
    pool.close().await;
    let _ = std::fs::remove_file(&path);
}

/// A row at a renumbered stable version whose checksum isn't the stable
/// file's is not moved: the boot fails as it would have, and the row stays.
#[tokio::test]
async fn sqlite_boot_does_not_move_a_row_it_does_not_recognise() {
    happyview::db::install_drivers();
    let (label, dir) = stable_migration_dirs().remove(0);
    let path = std::env::temp_dir().join(format!("hv-odd-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let pool = AnyPool::connect(&url).await.expect("connect");
    Migrator::new(dir.join("sqlite"))
        .await
        .expect("load")
        .run(&pool)
        .await
        .unwrap_or_else(|e| panic!("{label} migrates: {e}"));
    let (from, to, _) = renumbered().remove(0);
    happyview::db::query(&format!(
        "UPDATE _sqlx_migrations SET checksum = X'00' WHERE version = {from}"
    ))
    .execute(&pool)
    .await
    .expect("alter the checksum");
    let row = |pool: AnyPool, version: String| async move {
        let rows: Vec<(String, Vec<u8>)> = happyview::db::query_as(&format!(
            "SELECT description, checksum FROM _sqlx_migrations WHERE version = {version}"
        ))
        .fetch_all(&pool)
        .await
        .expect("read");
        rows
    };
    let before = row(pool.clone(), from.clone()).await;

    let boot = tokio::spawn(async move {
        happyview::db::connect(&url, happyview::db::DatabaseBackend::Sqlite)
            .await
            .close()
            .await;
    })
    .await;
    assert!(boot.is_err(), "a boot on an unrecognised row must fail");
    assert_eq!(
        row(pool.clone(), from).await,
        before,
        "the row stays where it was"
    );
    assert!(
        row(pool.clone(), to).await.is_empty(),
        "nothing is moved to the new version"
    );
    pool.close().await;
    let _ = std::fs::remove_file(&path);
}

/// Instances booting together on a stable branch's database all come up, and
/// the stable migration is recorded once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn postgres_instances_booting_together_on_a_stable_database_all_boot() {
    let (_, dir) = stable_migration_dirs().remove(0);
    let ran = with_scratch_postgres("hv_stable_concurrent", |url| async move {
        let pool = AnyPool::connect(&url).await.expect("connect");
        Migrator::new(dir.join("postgres"))
            .await
            .expect("load")
            .run(&pool)
            .await
            .expect("the stable branch migrates");
        let boots = (0..4).map(|_| {
            let url = url.clone();
            tokio::spawn(async move {
                happyview::db::connect(&url, happyview::db::DatabaseBackend::Postgres)
                    .await
                    .close()
                    .await;
            })
        });
        for boot in futures::future::join_all(boots).await {
            boot.expect("every instance migrates and boots");
        }
        let rows = applied(&pool).await;
        for (from, to, description) in renumbered() {
            let to: i64 = to.parse().unwrap();
            let from: i64 = from.parse().unwrap();
            assert_eq!(
                rows.iter()
                    .filter(|(_, d)| *d == description)
                    .map(|(v, _)| *v)
                    .collect::<Vec<_>>(),
                vec![to]
            );
            assert!(rows.iter().any(|(v, d)| *v == from && *d != description));
        }
        pool.close().await;
    })
    .await;
    if ran.is_none() {
        eprintln!("skipped (TEST_DATABASE_URL is not Postgres)");
    }
}
