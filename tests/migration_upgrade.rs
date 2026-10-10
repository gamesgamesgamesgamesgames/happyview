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
/// migrations on top and assert the row survived.
async fn assert_upgrade_preserves_data(pool: &AnyPool, migrations_dir: &str) {
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

    let _ = std::fs::remove_dir_all(&baseline_dir);
}

#[tokio::test]
async fn sqlite_upgrade_from_v2_0_0_applies_and_preserves_data() {
    sqlx::any::install_default_drivers();

    let tmp_db = std::env::temp_dir().join(format!("hv-upgrade-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}?mode=rwc", tmp_db.display());
    let pool = AnyPool::connect(&url)
        .await
        .expect("connect to fresh sqlite database");

    assert_upgrade_preserves_data(&pool, "migrations/sqlite").await;

    pool.close().await;
    let _ = std::fs::remove_file(&tmp_db);
}

#[tokio::test]
#[serial_test::serial]
async fn postgres_upgrade_from_v2_0_0_applies_and_preserves_data() {
    common::require_db!();
    sqlx::any::install_default_drivers();
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
    let outcome =
        std::panic::AssertUnwindSafe(assert_upgrade_preserves_data(&pool, "migrations/postgres"))
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
    sqlx::any::install_default_drivers();

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
    sqlx::any::install_default_drivers();
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

/// A `CREATE INDEX CONCURRENTLY` that fails partway leaves an invalid index of
/// its name, and sqlx records nothing, so the migration runs again at the next
/// boot. That run must build the index rather than let `IF NOT EXISTS` keep
/// the invalid one.
#[tokio::test]
#[serial_test::serial]
async fn postgres_boot_rebuilds_an_index_a_failed_concurrent_build_left_invalid() {
    let ran = with_scratch_postgres("hv_invalid_index", |url| async move {
        use happyview::db::{self, DatabaseBackend};
        let pool = db::connect(&url, DatabaseBackend::Postgres).await;

        // Rewind to before the index's migration, then fail a concurrent
        // build of that name: two rows share a collection, so a unique build
        // on it fails and leaves the index invalid.
        db::query("DELETE FROM _sqlx_migrations WHERE version >= 20261009000001")
            .execute(&pool)
            .await
            .expect("rewind migrations");
        db::query("DROP INDEX idx_records_collection_created_at_uri")
            .execute(&pool)
            .await
            .expect("drop the built index");
        db::query(
            "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, created_at) VALUES \
             ('at://did:plc:a/app.test.post/1', 'did:plc:a', 'app.test.post', '1', '{}', 'c1', NOW()), \
             ('at://did:plc:a/app.test.post/2', 'did:plc:a', 'app.test.post', '2', '{}', 'c2', NOW())",
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
        pool.close().await;

        let pool = db::connect(&url, DatabaseBackend::Postgres).await;
        let (valid, unique): (bool, bool) = db::query_as(
            "SELECT i.indisvalid, i.indisunique FROM pg_index i \
             JOIN pg_class c ON c.oid = i.indexrelid \
             WHERE c.relname = 'idx_records_collection_created_at_uri'",
        )
        .fetch_one(&pool)
        .await
        .expect("the index exists");
        assert!(valid, "the index is valid after the retry");
        assert!(!unique, "the retry built the migration's index");
        let (applied,): (i64,) =
            db::query_as("SELECT COUNT(*) FROM _sqlx_migrations WHERE version >= 20261009000001")
                .fetch_one(&pool)
                .await
                .expect("count migrations");
        assert_eq!(applied, 3);
        pool.close().await;
    })
    .await;
    if ran.is_none() {
        eprintln!("skipped (TEST_DATABASE_URL is not Postgres)");
    }
}
