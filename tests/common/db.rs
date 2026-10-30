use happyview::db::{self, DatabaseBackend};
use sqlx::AnyPool;

// The test-database plumbing every test instance shares, `ProviderApp`'s
// included.
pub use happyview::testkit::app::{
    TestLock, acquire_test_lock, test_backend, test_pool, truncate_all,
};

/// Insert a minimal `happyview_oauth_sessions` row directly, bypassing
/// `DbSessionStore::set()`. Rotation tests need to plant a session pinned to
/// a specific (or absent) `signing_kid` without driving a real OAuth flow —
/// `session_data` is never read by anything these tests exercise, so an
/// empty JSON object is enough.
pub async fn insert_oauth_session(
    pool: &AnyPool,
    backend: DatabaseBackend,
    did: &str,
    signing_kid: Option<&str>,
) {
    let sql = db::adapt_sql(
        "INSERT INTO happyview_oauth_sessions (did, session_data, signing_kid) VALUES (?, ?, ?)",
        backend,
    );
    db::query(&sql)
        .bind(did)
        .bind("{}")
        .bind(signing_kid)
        .execute(pool)
        .await
        .expect("failed to insert oauth session fixture");
}

/// The encryption key every test uses.
///
/// Must match `happyview::test_support::TEST_ENCRYPTION_KEY`, which is
/// `#[cfg(test)]` and so not visible here. Integration and lib tests share a
/// database; see that constant for why the values must agree.
pub const TEST_ENCRYPTION_KEY: [u8; 32] = [0x42u8; 32];

/// Provision the `#atproto_space` signing key, replacing one this state cannot
/// decrypt.
///
/// A running instance provisions the key at startup and in `create_space`, and
/// the space write path only reads it. Tests that seed spaces straight into the
/// DB bypass both, so without this every space write returns 500 on a missing
/// key.
pub async fn provision_space_signing_key(
    pool: &AnyPool,
    backend: DatabaseBackend,
    encryption_key: &[u8; 32],
) {
    let usable = happyview::verification_methods::get_private_key_bytes(
        pool,
        backend,
        "#atproto_space",
        encryption_key,
    )
    .await
    .ok()
    .flatten()
    .is_some();

    if !usable {
        let sql = happyview::db::adapt_sql(
            "DELETE FROM happyview_verification_methods WHERE fragment_id = ?",
            backend,
        );
        let _ = happyview::db::query(&sql)
            .bind("#atproto_space")
            .execute(pool)
            .await;
    }

    happyview::verification_methods::ensure_atproto_space_method(pool, backend, encryption_key)
        .await
        .expect("provision #atproto_space key");
}
