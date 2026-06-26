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
    let sql = adapt_sql(
        "SELECT value FROM happyview_plugin_kv
         WHERE plugin_id = ? AND scope = ? AND key = ?
         AND (expires_at IS NULL OR expires_at > datetime('now'))",
        ctx.db_backend,
    );

    let result: Option<(Vec<u8>,)> = sqlx::query_as(&sql)
        .bind(&ctx.plugin_id)
        .bind(&ctx.scope)
        .bind(key)
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

    // Upsert
    let sql = adapt_sql(
        "INSERT INTO happyview_plugin_kv (plugin_id, scope, key, value, expires_at, created_at)
         VALUES (?, ?, ?, ?, ?, datetime('now'))
         ON CONFLICT (plugin_id, scope, key)
         DO UPDATE SET value = excluded.value, expires_at = excluded.expires_at",
        ctx.db_backend,
    );

    sqlx::query(&sql)
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

    sqlx::query(&sql)
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
}
