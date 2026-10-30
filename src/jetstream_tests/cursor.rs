//! The Jetstream cursor's load and save against HappyView's migrated
//! databases (SQLite, and Postgres when `TEST_DATABASE_URL` is set).

use serial_test::serial;
use sqlx::AnyPool;

use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::test_support::test_pools;
use happyview_source_jetstream::consumer::config::Protocol;
use happyview_source_jetstream::consumer::cursor::*;
use happyview_source_jetstream::consumer::event::CursorPoint;

const T: i64 = 1_779_719_000_000_000;

fn stored(seq: Option<i64>, time_us: Option<i64>, host: Option<&str>) -> StoredCursor {
    StoredCursor {
        seq,
        time_us,
        host: host.map(str::to_string),
    }
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

/// Ruling FR4: upgrade, roll back to a HappyView that only knows the
/// time cursor, upgrade again. The seq the first upgrade saved is older
/// than the time the old binary saved since, so it is dropped and the
/// resume goes by time.
#[tokio::test]
#[serial]
async fn a_seq_older_than_the_time_cursor_is_dropped() {
    for (pool, backend) in test_pools().await {
        clear(&pool, backend).await;
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

        // The pre-v2 binary's save: the time row only, as it writes it.
        let later = (chrono::Utc::now() + chrono::Duration::seconds(5)).to_rfc3339();
        crate::db::query(&adapt_sql(
            "INSERT INTO happyview_instance_settings (key, value, updated_at) VALUES (?, ?, ?) \
             ON CONFLICT (key) DO UPDATE SET value = ?, updated_at = ?",
            backend,
        ))
        .bind(TIME_KEY)
        .bind((T + 9_000).to_string())
        .bind(&later)
        .bind((T + 9_000).to_string())
        .bind(&later)
        .execute(&pool)
        .await
        .unwrap();

        let loaded = load(&pool, backend).await.unwrap();
        assert_eq!(
            loaded,
            stored(None, Some(T + 9_000), Some("js.example.com")),
            "{backend:?}"
        );
        assert_eq!(
            resume_cursor(&loaded, "js.example.com", Protocol::V2),
            Some(T + 9_000)
        );
        clear(&pool, backend).await;
    }
}

/// Review N1: a later time row that is not a usable time cursor drops
/// nothing, since there would be no time to resume from.
#[tokio::test]
#[serial]
async fn a_seq_is_kept_when_the_later_time_row_is_unusable() {
    for (pool, backend) in test_pools().await {
        clear(&pool, backend).await;
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
        let later = (chrono::Utc::now() + chrono::Duration::seconds(5)).to_rfc3339();
        crate::db::query(&adapt_sql(
            "UPDATE happyview_instance_settings SET value = ?, updated_at = ? WHERE key = ?",
            backend,
        ))
        .bind("12345")
        .bind(&later)
        .bind(TIME_KEY)
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            load(&pool, backend).await.unwrap(),
            stored(Some(42), None, Some("js.example.com")),
            "{backend:?}"
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
