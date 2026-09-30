use super::{HostContext, MAX_KV_SIZE_PER_USER, ResourceUsage};
use crate::db::adapt_sql;

#[derive(Debug, thiserror::Error)]
pub enum KvError {
    #[error("Storage quota exceeded: {0} > {MAX_KV_SIZE_PER_USER}")]
    QuotaExceeded(u64),
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
}

pub async fn kv_get(ctx: &HostContext, key: &str) -> Result<Option<Vec<u8>>, KvError> {
    // `expires_at` is TEXT, and `adapt_sql` turns `datetime('now')` into
    // Postgres's `NOW()`, which is a timestamp — so comparing the two there
    // failed every read with `operator does not exist: text > timestamp with
    // time zone`, whatever rows the table held. Binding the instant as text
    // compares text to text, which is what every other `expires_at` in this
    // codebase does and is sound because each writer emits the same offset
    // rather than a `Z` suffix.
    let sql = adapt_sql(
        "SELECT value FROM happyview_plugin_kv
         WHERE plugin_id = ? AND scope = ? AND key = ?
         AND (expires_at IS NULL OR expires_at > ?)",
        ctx.db_backend,
    );

    let result: Option<(Vec<u8>,)> = crate::db::query_as(&sql)
        .bind(&ctx.plugin_id)
        .bind(&ctx.scope)
        .bind(key)
        .bind(crate::db::now_rfc3339())
        .fetch_optional(&ctx.db)
        .await?;

    Ok(result.map(|(v,)| v))
}

pub async fn kv_set(
    ctx: &HostContext,
    usage: &mut ResourceUsage,
    key: &str,
    value: Vec<u8>,
    ttl_secs: Option<u32>,
) -> Result<(), KvError> {
    // Check quota (simple check - full implementation would sum all keys)
    usage.kv_bytes_used += value.len() as u64;
    if usage.kv_bytes_used > MAX_KV_SIZE_PER_USER {
        return Err(KvError::QuotaExceeded(usage.kv_bytes_used));
    }

    let expires_at = ttl_secs
        .map(|secs| (chrono::Utc::now() + chrono::Duration::seconds(secs as i64)).to_rfc3339());

    // Upsert. `created_at` takes a SQL clock, which `adapt_sql` turns into
    // Postgres's `NOW()`, whose assignment cast into the TEXT column stores a
    // space-separated `+00` form rather than the `+00:00` RFC 3339 `expires_at`
    // beside it carries. Nothing compares or orders `created_at`, so that
    // costs nothing; anything that starts to must bind `now_rfc3339()` here
    // first, since the two shapes do not sort against each other.
    let sql = adapt_sql(
        "INSERT INTO happyview_plugin_kv (plugin_id, scope, key, value, expires_at, created_at)
         VALUES (?, ?, ?, ?, ?, datetime('now'))
         ON CONFLICT (plugin_id, scope, key)
         DO UPDATE SET value = excluded.value, expires_at = excluded.expires_at",
        ctx.db_backend,
    );

    crate::db::query(&sql)
        .bind(&ctx.plugin_id)
        .bind(&ctx.scope)
        .bind(key)
        .bind(&value)
        .bind(expires_at)
        .execute(&ctx.db)
        .await?;

    Ok(())
}

pub async fn kv_delete(ctx: &HostContext, key: &str) -> Result<(), KvError> {
    let sql = adapt_sql(
        "DELETE FROM happyview_plugin_kv WHERE plugin_id = ? AND scope = ? AND key = ?",
        ctx.db_backend,
    );

    crate::db::query(&sql)
        .bind(&ctx.plugin_id)
        .bind(&ctx.scope)
        .bind(key)
        .execute(&ctx.db)
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quota_exceeded_check() {
        let mut usage = ResourceUsage {
            kv_bytes_used: MAX_KV_SIZE_PER_USER,
            ..Default::default()
        };

        // Adding more would exceed quota
        usage.kv_bytes_used += 1;
        assert!(usage.kv_bytes_used > MAX_KV_SIZE_PER_USER);
    }

    #[test]
    fn test_kv_error_display() {
        let err = KvError::QuotaExceeded(2_000_000);
        assert!(err.to_string().contains("exceeded"));
    }
    use crate::db::DatabaseBackend;
    use serial_test::serial;

    /// Postgres, and only Postgres: `expires_at` is TEXT on both backends, but
    /// `adapt_sql` rewrites `datetime('now')` for Postgres alone, so the
    /// comparison this pins is well-typed on SQLite whatever the query says.
    /// A SQLite-only test would pass with the fault present.
    ///
    /// The old predicate was wrong on SQLite too, less loudly: `expires_at`
    /// holds RFC 3339 (`…T13:44:00+00:00`) while `datetime('now')` yields
    /// `… 13:44:00`, and `T` sorts above a space, so a row that expired
    /// earlier the same day compared as still live. The third arm below would
    /// catch that as well, against a SQLite `TEST_DATABASE_URL`.
    macro_rules! require_test_db {
        () => {
            if std::env::var("TEST_DATABASE_URL").is_err() {
                eprintln!("skipped (TEST_DATABASE_URL not set)");
                return;
            }
        };
    }

    async fn context(plugin_id: &str) -> HostContext {
        let url = std::env::var("TEST_DATABASE_URL").expect("the caller checks it first");
        let backend = DatabaseBackend::from_url(&url);
        let db = crate::db::connect(&url, backend).await;
        context_on(plugin_id, db, backend).await
    }

    async fn context_on(
        plugin_id: &str,
        db: sqlx::AnyPool,
        backend: DatabaseBackend,
    ) -> HostContext {
        // The store has a foreign key onto the plugins table.
        crate::db::query(&adapt_sql(
            "INSERT INTO happyview_plugins (id, source, api_version) VALUES (?, 'file', '2')
             ON CONFLICT (id) DO NOTHING",
            backend,
        ))
        .bind(plugin_id)
        .execute(&db)
        .await
        .expect("the plugin row the store's foreign key needs");

        HostContext {
            plugin_id: plugin_id.to_string(),
            scope: "did:plc:test".to_string(),
            secrets: Default::default(),
            config: serde_json::Value::Null,
            db,
            db_backend: backend,
            http_client: reqwest::Client::new(),
            lexicons: std::sync::Arc::new(crate::lexicon::LexiconRegistry::new()),
        }
    }

    /// Today's midnight, as the store writes an instant. Same-day on purpose:
    /// that is the only shape in which the old SQLite comparison went wrong,
    /// since a different date compares correctly whichever format each side
    /// is in.
    fn earlier_today() -> String {
        chrono::Utc::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .expect("midnight exists")
            .and_utc()
            .to_rfc3339()
    }

    /// The SQLite half of the same fix, which is narrower and was left
    /// looking incidental by a comment alone.
    ///
    /// The old predicate was well-typed on SQLite, so it ran — and still got
    /// this wrong. `expires_at` holds RFC 3339 (`…T00:00:00+00:00`) while
    /// `datetime('now')` yields `… 13:44:00`, and `T` sorts above a space, so
    /// a row that expired **earlier the same day** compared as still live. A
    /// row that expired on an earlier date compared correctly, which is why
    /// the fault needed a same-day instant to show at all.
    ///
    /// No environment: this runs against a migrated in-memory database every
    /// time, where the Postgres arm above can only run when one is pointed at.
    #[tokio::test]
    async fn kv_get_honours_a_same_day_expiry_on_sqlite() {
        let db = crate::test_support::migrated_memory_pool().await;
        let ctx = context_on("kv_same_day_test", db, DatabaseBackend::Sqlite).await;
        let mut usage = ResourceUsage::default();

        kv_set(&ctx, &mut usage, "today", b"kept".to_vec(), Some(3600))
            .await
            .expect("a write with a future expiry");
        assert_eq!(
            kv_get(&ctx, "today").await.expect("the read should work"),
            Some(b"kept".to_vec()),
            "a value that has not expired is returned"
        );

        crate::db::query(&adapt_sql(
            "UPDATE happyview_plugin_kv SET expires_at = ? WHERE plugin_id = ? AND key = 'today'",
            ctx.db_backend,
        ))
        .bind(earlier_today())
        .bind(&ctx.plugin_id)
        .execute(&ctx.db)
        .await
        .expect("the expiry should be settable");

        assert_eq!(
            kv_get(&ctx, "today").await.expect("the read should work"),
            None,
            "a value that expired earlier today is not returned"
        );
    }

    /// The whole of the read path against the backend that broke it. The
    /// predicate compared a TEXT column against Postgres's `NOW()`, which
    /// Postgres refuses outright — so every read failed, whatever the table
    /// held and whether or not anything had expired. Both halves are asserted
    /// because only the pair distinguishes a working predicate from one that
    /// is simply absent.
    #[tokio::test]
    #[serial]
    async fn kv_get_honours_a_text_expiry_on_postgres() {
        require_test_db!();
        let ctx = context("kv_expiry_test").await;
        let mut usage = ResourceUsage::default();

        let hour = 3600;
        kv_set(&ctx, &mut usage, "fresh", b"kept".to_vec(), Some(hour))
            .await
            .expect("a write with a future expiry");
        kv_set(&ctx, &mut usage, "forever", b"kept".to_vec(), None)
            .await
            .expect("a write with no expiry");

        assert_eq!(
            kv_get(&ctx, "fresh").await.expect("the read should work"),
            Some(b"kept".to_vec()),
            "a value that has not expired is returned"
        );
        assert_eq!(
            kv_get(&ctx, "forever").await.expect("the read should work"),
            Some(b"kept".to_vec()),
            "a value with no expiry is returned"
        );

        // An expiry in the past, written directly because `kv_set` only takes
        // a positive time to live.
        crate::db::query(&adapt_sql(
            "UPDATE happyview_plugin_kv SET expires_at = ? WHERE plugin_id = ? AND key = 'fresh'",
            ctx.db_backend,
        ))
        .bind("2020-01-01T00:00:00+00:00")
        .bind(&ctx.plugin_id)
        .execute(&ctx.db)
        .await
        .expect("the expiry should be settable");

        assert_eq!(
            kv_get(&ctx, "fresh").await.expect("the read should work"),
            None,
            "a value that has expired is not returned"
        );

        kv_delete(&ctx, "fresh").await.expect("delete");
        kv_delete(&ctx, "forever").await.expect("delete");
    }
}
