use crate::db::{DatabaseBackend, adapt_sql};
use crate::error::AppError;
use crate::spaces::types::{OplogAction, OplogEntry};

pub async fn append_op(
    executor: impl sqlx::Executor<'_, Database = sqlx::Any>,
    backend: DatabaseBackend,
    entry: &OplogEntry,
) -> Result<(), AppError> {
    let sql = adapt_sql(
        "INSERT INTO happyview_space_record_oplog (id, space_id, author_did, rev, idx, action, collection, rkey, cid, prev, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        backend,
    );
    crate::db::query(&sql)
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
        .execute(executor)
        .await
        .map_err(|e| AppError::Internal(format!("failed to append oplog entry: {e}")))?;
    Ok(())
}

pub fn encode_cursor(rev: &str, idx: i32) -> String {
    format!("{rev}:{idx}")
}

/// Whether `value` is a TID: 13 base32-sortable characters whose top bit is 0.
pub fn is_tid(value: &str) -> bool {
    value.len() == 13
        && happyview_plugin_sdk::tid::tid_to_number(value).is_some()
        && matches!(value.as_bytes()[0], b'2'..=b'7' | b'a'..=b'j')
}

/// Whether `cursor` is one this host issues: `{rev}:{idx}`, or a bare rev.
///
/// [`parse_cursor`] reads anything, so the routes that predate the lexicons'
/// `InvalidCursor` keep reading what they always did; requests in the
/// lexicons' shape check with this first.
pub fn is_valid_cursor(cursor: &str) -> bool {
    match cursor.rsplit_once(':') {
        Some((rev, idx)) => is_tid(rev) && idx.parse::<u32>().is_ok(),
        None => is_tid(cursor),
    }
}

/// Where a listing starts: after `cursor` or after every op of `since`,
/// whichever is later. `since` is exclusive, as a bare-rev cursor is.
pub fn start_after(cursor: Option<&str>, since: Option<&str>) -> Option<String> {
    match (cursor, since) {
        (Some(cursor), Some(since)) => {
            let (rev, idx) = parse_cursor(cursor);
            if (rev, idx) >= (since, i32::MAX) {
                Some(cursor.to_string())
            } else {
                Some(since.to_string())
            }
        }
        (Some(cursor), None) => Some(cursor.to_string()),
        (None, since) => since.map(str::to_string),
    }
}

fn parse_cursor(cursor: &str) -> (&str, i32) {
    match cursor.rsplit_once(':') {
        Some((rev, idx)) => match idx.parse::<i32>() {
            Ok(idx) => (rev, idx),
            Err(_) => (cursor, i32::MAX),
        },
        None => (cursor, i32::MAX),
    }
}

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

fn paginate<T>(
    mut rows: Vec<T>,
    limit: i64,
    key: impl Fn(&T) -> (String, i32),
) -> (Vec<T>, Option<String>) {
    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit.max(0) as usize);
    let next = if has_more {
        rows.last().map(|r| {
            let (rev, idx) = key(r);
            encode_cursor(&rev, idx)
        })
    } else {
        None
    };
    (rows, next)
}

pub async fn list_ops(
    executor: impl sqlx::Executor<'_, Database = sqlx::Any>,
    backend: DatabaseBackend,
    space_id: &str,
    author_did: &str,
    cursor: Option<&str>,
    limit: i64,
) -> Result<(Vec<OplogEntry>, Option<String>), AppError> {
    let sql = if cursor.is_some() {
        adapt_sql(
            "SELECT id, space_id, author_did, rev, idx, action, collection, rkey, cid, prev, created_at FROM happyview_space_record_oplog WHERE space_id = ? AND author_did = ? AND (rev > ? OR (rev = ? AND idx > ?)) ORDER BY rev, idx LIMIT ?",
            backend,
        )
    } else {
        adapt_sql(
            "SELECT id, space_id, author_did, rev, idx, action, collection, rkey, cid, prev, created_at FROM happyview_space_record_oplog WHERE space_id = ? AND author_did = ? ORDER BY rev, idx LIMIT ?",
            backend,
        )
    };

    let mut query = crate::db::query_as::<OplogRow>(&sql)
        .bind(space_id)
        .bind(author_did);
    if let Some(c) = cursor {
        let (rev, idx) = parse_cursor(c);
        query = query.bind(rev.to_string()).bind(rev.to_string()).bind(idx);
    }
    // Over-fetch by one to detect whether another page exists.
    query = query.bind(limit + 1);

    let rows = query
        .fetch_all(executor)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list oplog entries: {e}")))?;

    let (rows, next_cursor) = paginate(rows, limit, |r| (r.3.clone(), r.4));

    let entries = rows
        .into_iter()
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
        .collect::<Result<Vec<_>, AppError>>()?;

    Ok((entries, next_cursor))
}

pub async fn list_ops_with_values(
    executor: impl sqlx::Executor<'_, Database = sqlx::Any>,
    backend: DatabaseBackend,
    space_id: &str,
    author_did: &str,
    cursor: Option<&str>,
    limit: i64,
) -> Result<(Vec<OplogEntry>, Option<String>), AppError> {
    let sql = if cursor.is_some() {
        adapt_sql(
            "SELECT o.id, o.space_id, o.author_did, o.rev, o.idx, o.action, o.collection, o.rkey, o.cid, o.prev, o.created_at, r.record FROM happyview_space_record_oplog o LEFT JOIN happyview_space_records r ON r.space_id = o.space_id AND r.author_did = o.author_did AND r.collection = o.collection AND r.rkey = o.rkey AND r.cid = o.cid WHERE o.space_id = ? AND o.author_did = ? AND (o.rev > ? OR (o.rev = ? AND o.idx > ?)) ORDER BY o.rev, o.idx LIMIT ?",
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

    let mut query = crate::db::query_as::<OplogWithValueRow>(&sql)
        .bind(space_id)
        .bind(author_did);
    if let Some(c) = cursor {
        let (rev, idx) = parse_cursor(c);
        query = query.bind(rev.to_string()).bind(rev.to_string()).bind(idx);
    }
    // Over-fetch by one to detect whether another page exists.
    query = query.bind(limit + 1);

    let rows = query.fetch_all(executor).await.map_err(|e| {
        AppError::Internal(format!("failed to list oplog entries with values: {e}"))
    })?;

    let (rows, next_cursor) = paginate(rows, limit, |r| (r.3.clone(), r.4));

    let entries = rows
        .into_iter()
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
        .collect::<Result<Vec<_>, AppError>>()?;

    Ok((entries, next_cursor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_roundtrip() {
        let c = encode_cursor("3k2rev1", 4);
        assert_eq!(c, "3k2rev1:4");
        assert_eq!(parse_cursor(&c), ("3k2rev1", 4));
    }

    #[test]
    fn bare_rev_cursor_skips_whole_rev() {
        // Backwards-compat: a rev-only cursor must exclude every op of that rev, so
        // the `rev = ? AND idx > ?` arm can never match.
        assert_eq!(parse_cursor("3k2rev1"), ("3k2rev1", i32::MAX));
    }

    #[test]
    fn malformed_idx_falls_back_to_whole_rev() {
        assert_eq!(
            parse_cursor("3k2rev1:notanint"),
            ("3k2rev1:notanint", i32::MAX)
        );
    }

    #[test]
    fn a_cursor_this_host_issues_is_valid() {
        assert!(is_valid_cursor(&encode_cursor("3mwcj3b6uak2f", 4)));
        assert!(is_valid_cursor("3mwcj3b6uak2f"));
        for bad in [
            "",
            "garbage",
            "3mwcj3b6uak2f:",
            "3mwcj3b6uak2f:-1",
            "3mwcj3b6uak2f:x",
            "3mwcj3b6uak2:1",
            "zmwcj3b6uak2f:1",
            "3mwcj3b6uak2f1:1",
        ] {
            assert!(!is_valid_cursor(bad), "{bad:?} should be refused");
        }
    }

    #[test]
    fn a_listing_starts_after_the_later_of_cursor_and_since() {
        let early = "3mwcj3b6uak2a";
        let late = "3mwcj3b6uak2f";
        assert_eq!(start_after(None, None), None);
        assert_eq!(start_after(None, Some(late)).as_deref(), Some(late));
        assert_eq!(
            start_after(Some(&encode_cursor(early, 1)), None).as_deref(),
            Some("3mwcj3b6uak2a:1")
        );
        // since is past the cursor: start after every op of since.
        assert_eq!(
            start_after(Some(&encode_cursor(early, 1)), Some(late)).as_deref(),
            Some(late)
        );
        // the cursor is past since: keep paging from the cursor.
        assert_eq!(
            start_after(Some(&encode_cursor(late, 2)), Some(early)).as_deref(),
            Some("3mwcj3b6uak2f:2")
        );
        // a cursor inside since's own rev is not past it: since excludes that
        // whole rev.
        assert_eq!(
            start_after(Some(&encode_cursor(late, 2)), Some(late)).as_deref(),
            Some(late)
        );
    }

    #[test]
    fn paginate_returns_no_cursor_when_page_not_full() {
        let rows = vec![("a".to_string(), 0), ("b".to_string(), 1)];
        let (out, cursor) = paginate(rows, 5, |r| (r.0.clone(), r.1));
        assert_eq!(out.len(), 2);
        assert!(cursor.is_none());
    }

    #[test]
    fn paginate_truncates_and_emits_cursor_from_last_kept_row() {
        let rows = vec![
            ("rev1".to_string(), 0),
            ("rev1".to_string(), 1),
            ("rev1".to_string(), 2),
        ];
        let (out, cursor) = paginate(rows, 2, |r| (r.0.clone(), r.1));
        assert_eq!(out.len(), 2);
        assert_eq!(cursor.as_deref(), Some("rev1:1"));
    }
}
