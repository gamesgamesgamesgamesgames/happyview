use crate::db::{DatabaseBackend, adapt_sql};
use crate::error::AppError;
use crate::spaces::types::{OplogAction, OplogEntry};

pub async fn append_op(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    entry: &OplogEntry,
) -> Result<(), AppError> {
    let sql = adapt_sql(
        "INSERT INTO happyview_space_record_oplog (id, space_id, author_did, rev, idx, action, collection, rkey, cid, prev, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        backend,
    );
    sqlx::query(&sql)
        .bind(&entry.id)
        .bind(&entry.space_id)
        .bind(&entry.author_did)
        .bind(&entry.rev)
        .bind(entry.idx)
        .bind(entry.action.as_str())
        .bind(&entry.collection)
        .bind(&entry.rkey)
        .bind(&entry.cid)
        .bind(&entry.prev)
        .bind(&entry.created_at)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to append oplog entry: {e}")))?;
    Ok(())
}

pub async fn list_ops(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    space_id: &str,
    author_did: &str,
    since_rev: Option<&str>,
    limit: i64,
) -> Result<Vec<OplogEntry>, AppError> {
    let sql = if since_rev.is_some() {
        adapt_sql(
            "SELECT id, space_id, author_did, rev, idx, action, collection, rkey, cid, prev, created_at FROM happyview_space_record_oplog WHERE space_id = ? AND author_did = ? AND rev > ? ORDER BY rev, idx LIMIT ?",
            backend,
        )
    } else {
        adapt_sql(
            "SELECT id, space_id, author_did, rev, idx, action, collection, rkey, cid, prev, created_at FROM happyview_space_record_oplog WHERE space_id = ? AND author_did = ? ORDER BY rev, idx LIMIT ?",
            backend,
        )
    };

    type OplogRow = (
        String,
        String,
        String,
        String,
        i32,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
    );

    let mut query = sqlx::query_as::<_, OplogRow>(&sql)
        .bind(space_id)
        .bind(author_did);
    if let Some(rev) = since_rev {
        query = query.bind(rev);
    }
    query = query.bind(limit);

    let rows = query
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list oplog entries: {e}")))?;

    rows.into_iter()
        .map(|r| {
            let action = OplogAction::parse(&r.5)
                .ok_or_else(|| AppError::Internal(format!("invalid oplog action: {}", r.5)))?;
            Ok(OplogEntry {
                id: r.0,
                space_id: r.1,
                author_did: r.2,
                rev: r.3,
                idx: r.4,
                action,
                collection: r.6,
                rkey: r.7,
                cid: r.8,
                prev: r.9,
                value: None,
                created_at: r.10,
            })
        })
        .collect()
}

pub async fn list_ops_with_values(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    space_id: &str,
    author_did: &str,
    since_rev: Option<&str>,
    limit: i64,
) -> Result<Vec<OplogEntry>, AppError> {
    let sql = if since_rev.is_some() {
        adapt_sql(
            "SELECT o.id, o.space_id, o.author_did, o.rev, o.idx, o.action, o.collection, o.rkey, o.cid, o.prev, o.created_at, r.record FROM happyview_space_record_oplog o LEFT JOIN happyview_space_records r ON r.space_id = o.space_id AND r.author_did = o.author_did AND r.collection = o.collection AND r.rkey = o.rkey AND r.cid = o.cid WHERE o.space_id = ? AND o.author_did = ? AND o.rev > ? ORDER BY o.rev, o.idx LIMIT ?",
            backend,
        )
    } else {
        adapt_sql(
            "SELECT o.id, o.space_id, o.author_did, o.rev, o.idx, o.action, o.collection, o.rkey, o.cid, o.prev, o.created_at, r.record FROM happyview_space_record_oplog o LEFT JOIN happyview_space_records r ON r.space_id = o.space_id AND r.author_did = o.author_did AND r.collection = o.collection AND r.rkey = o.rkey AND r.cid = o.cid WHERE o.space_id = ? AND o.author_did = ? ORDER BY o.rev, o.idx LIMIT ?",
            backend,
        )
    };

    type OplogWithValueRow = (
        String,
        String,
        String,
        String,
        i32,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        Option<String>,
    );

    let mut query = sqlx::query_as::<_, OplogWithValueRow>(&sql)
        .bind(space_id)
        .bind(author_did);
    if let Some(rev) = since_rev {
        query = query.bind(rev);
    }
    query = query.bind(limit);

    let rows = query.fetch_all(pool).await.map_err(|e| {
        AppError::Internal(format!("failed to list oplog entries with values: {e}"))
    })?;

    rows.into_iter()
        .map(|r| {
            let action = OplogAction::parse(&r.5)
                .ok_or_else(|| AppError::Internal(format!("invalid oplog action: {}", r.5)))?;
            let value = r
                .11
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .map_err(|e| AppError::Internal(format!("failed to parse record value: {e}")))?;
            Ok(OplogEntry {
                id: r.0,
                space_id: r.1,
                author_did: r.2,
                rev: r.3,
                idx: r.4,
                action,
                collection: r.6,
                rkey: r.7,
                cid: r.8,
                prev: r.9,
                value,
                created_at: r.10,
            })
        })
        .collect()
}
