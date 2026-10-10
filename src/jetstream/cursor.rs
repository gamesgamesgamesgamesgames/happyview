//! The persisted stream position: the applied watermark, in
//! `happyview_instance_settings`.
//!
//! - `jetstream_seq`: the v2 seq. Seqs are local to one Jetstream instance,
//!   so it is only used on `jetstream_host`.
//! - `jetstream_cursor`: when Jetstream witnessed that event, in unix
//!   microseconds. This is the v1 cursor HappyView has always stored; it is
//!   kept current on v2 too, so v1, another host, or an older HappyView can
//!   still resume from it.
//! - `jetstream_host`: the host the seq belongs to.

use sqlx::AnyPool;

use super::config::Protocol;
use super::event::CursorPoint;
use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};

pub const SEQ_KEY: &str = "jetstream_seq";
pub const TIME_KEY: &str = "jetstream_cursor";
pub const HOST_KEY: &str = "jetstream_host";
/// How far before the saved time a resume on a different host starts, to
/// cover clock skew between instances. Re-applied events are idempotent.
pub const HOST_CHANGE_REWIND_US: i64 = 5_000_000;
/// Cursors at or above this are unix microseconds; below, seqs.
pub const TIMESTAMP_CURSOR_MIN: i64 = 1_000_000_000_000_000;

/// What `load` found. `time_us` is also the fallback when a seq resume is
/// refused as too old.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoredCursor {
    pub seq: Option<i64>,
    pub time_us: Option<i64>,
    pub host: Option<String>,
}

pub async fn load(db: &AnyPool, backend: DatabaseBackend) -> Result<StoredCursor, sqlx::Error> {
    let sql = adapt_sql(
        "SELECT key, value FROM happyview_instance_settings WHERE key IN (?, ?, ?)",
        backend,
    );
    let rows: Vec<(String, String)> = crate::db::query_as(&sql)
        .bind(SEQ_KEY)
        .bind(TIME_KEY)
        .bind(HOST_KEY)
        .fetch_all(db)
        .await?;
    let mut stored = StoredCursor::default();
    for (key, value) in rows {
        match key.as_str() {
            SEQ_KEY => stored.seq = value.parse().ok(),
            TIME_KEY => {
                stored.time_us = value
                    .parse()
                    .ok()
                    .filter(|t: &i64| *t >= TIMESTAMP_CURSOR_MIN)
            }
            HOST_KEY => stored.host = Some(value).filter(|h| !h.is_empty()),
            _ => {}
        }
    }
    Ok(stored)
}

/// Save `point` as the applied watermark on `host`, in one transaction.
pub async fn save(
    db: &AnyPool,
    backend: DatabaseBackend,
    host: &str,
    point: &CursorPoint,
) -> Result<(), sqlx::Error> {
    let now = now_rfc3339();
    let upsert = adapt_sql(
        "INSERT INTO happyview_instance_settings (key, value, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        backend,
    );
    let mut values = vec![
        (HOST_KEY, host.to_string()),
        (TIME_KEY, point.time_us().to_string()),
    ];
    let mut tx = db.begin().await?;
    match point.seq() {
        Some(seq) => values.push((SEQ_KEY, seq.to_string())),
        None => {
            crate::db::query(&adapt_sql(
                "DELETE FROM happyview_instance_settings WHERE key = ?",
                backend,
            ))
            .bind(SEQ_KEY)
            .execute(&mut *tx)
            .await?;
        }
    }
    for (key, value) in values {
        crate::db::query(&upsert)
            .bind(key)
            .bind(value)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

/// The `cursor` to subscribe with, or `None` for the live tip.
pub fn resume_cursor(stored: &StoredCursor, host: &str, protocol: Protocol) -> Option<i64> {
    let same_host = stored.host.as_deref().is_none_or(|h| h == host);
    if !same_host {
        return stored.time_us.map(|t| t - HOST_CHANGE_REWIND_US);
    }
    match protocol {
        Protocol::V2 => stored.seq.or(stored.time_us),
        Protocol::V1 => stored.time_us,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_pools;
    use serial_test::serial;

    const T: i64 = 1_779_719_000_000_000;

    fn stored(seq: Option<i64>, time_us: Option<i64>, host: Option<&str>) -> StoredCursor {
        StoredCursor {
            seq,
            time_us,
            host: host.map(str::to_string),
        }
    }

    #[test]
    fn where_to_resume() {
        let host = "js.example.com";
        assert_eq!(
            resume_cursor(&StoredCursor::default(), host, Protocol::V2),
            None,
            "a fresh instance starts at the tip"
        );
        assert_eq!(
            resume_cursor(&stored(None, Some(T), None), host, Protocol::V2),
            Some(T),
            "the pre-upgrade v1 time cursor is a v2 timestamp cursor"
        );
        assert_eq!(
            resume_cursor(&stored(Some(42), Some(T), Some(host)), host, Protocol::V2),
            Some(42)
        );
        assert_eq!(
            resume_cursor(&stored(Some(42), Some(T), Some(host)), host, Protocol::V1),
            Some(T),
            "v1 never gets a seq"
        );
        assert_eq!(
            resume_cursor(
                &stored(Some(42), Some(T), Some("old.example.com")),
                host,
                Protocol::V2
            ),
            Some(T - HOST_CHANGE_REWIND_US),
            "seqs are per instance, so another host resumes by time"
        );
        assert_eq!(
            resume_cursor(
                &stored(Some(42), None, Some("old.example.com")),
                host,
                Protocol::V2
            ),
            None
        );
    }

    async fn clear(pool: &AnyPool, backend: DatabaseBackend) {
        crate::db::query(&adapt_sql(
            "DELETE FROM happyview_instance_settings WHERE key IN (?, ?, ?)",
            backend,
        ))
        .bind(SEQ_KEY)
        .bind(TIME_KEY)
        .bind(HOST_KEY)
        .execute(pool)
        .await
        .expect("clear cursor");
    }

    #[tokio::test]
    #[serial]
    async fn the_cursor_round_trips() {
        for (pool, backend) in test_pools().await {
            clear(&pool, backend).await;
            assert_eq!(load(&pool, backend).await.unwrap(), StoredCursor::default());

            save(
                &pool,
                backend,
                "js.example.com",
                &CursorPoint::V2 {
                    seq: 42,
                    witnessed_us: T + 42,
                },
            )
            .await
            .unwrap();
            assert_eq!(
                load(&pool, backend).await.unwrap(),
                stored(Some(42), Some(T + 42), Some("js.example.com")),
                "{backend:?}"
            );

            save(
                &pool,
                backend,
                "js.example.com",
                &CursorPoint::V1 { time_us: T + 100 },
            )
            .await
            .unwrap();
            assert_eq!(
                load(&pool, backend).await.unwrap(),
                stored(None, Some(T + 100), Some("js.example.com")),
                "a v1 save drops the seq, which would otherwise shadow the newer time"
            );
            clear(&pool, backend).await;
        }
    }

    #[tokio::test]
    #[serial]
    async fn a_time_value_below_the_timestamp_range_is_ignored() {
        for (pool, backend) in test_pools().await {
            clear(&pool, backend).await;
            crate::db::query(&adapt_sql(
                "INSERT INTO happyview_instance_settings (key, value, updated_at) VALUES (?, ?, ?)",
                backend,
            ))
            .bind(TIME_KEY)
            .bind("12345")
            .bind(now_rfc3339())
            .execute(&pool)
            .await
            .unwrap();
            assert_eq!(load(&pool, backend).await.unwrap().time_us, None);
            clear(&pool, backend).await;
        }
    }
}
