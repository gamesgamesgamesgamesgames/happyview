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
    let base_url = std::env::var("TEST_DATABASE_URL").ok()?;
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

/// The last migration of the tombstone release, the one the rollback leaves.
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
