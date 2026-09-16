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
