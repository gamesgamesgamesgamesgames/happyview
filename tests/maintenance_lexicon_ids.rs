mod common;

use happyview::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use happyview::maintenance::lexicon_ids;
use serial_test::serial;
use sqlx::AnyPool;

async fn insert_lexicon(pool: &AnyPool, backend: DatabaseBackend, id: &str) {
    let sql = adapt_sql(
        r#"INSERT INTO happyview_lexicons (id, lexicon_json, backfill, source, created_at)
           VALUES (?, ?, 0, 'manual', ?)"#,
        backend,
    );
    happyview::db::query(&sql)
        .bind(id)
        .bind(format!(r#"{{"lexicon":1,"id":"{id}","defs":{{}}}}"#))
        .bind(now_rfc3339())
        .execute(pool)
        .await
        .expect("failed to insert lexicon");
}

async fn stored_ids(pool: &AnyPool) -> Vec<String> {
    let rows: Vec<(String,)> =
        happyview::db::query_as("SELECT id FROM happyview_lexicons ORDER BY id")
            .fetch_all(pool)
            .await
            .expect("failed to list lexicons");
    rows.into_iter().map(|(id,)| id).collect()
}

#[tokio::test]
#[serial]
async fn the_sweep_deletes_a_blank_id_row_and_keeps_every_other_row() {
    common::require_db!();
    let pool = common::db::test_pool().await;
    let backend = common::db::test_backend();
    common::db::truncate_all(&pool).await;

    insert_lexicon(&pool, backend, "").await;
    insert_lexicon(&pool, backend, "games.gamesgamesgamesgames.game").await;
    insert_lexicon(&pool, backend, "com.example").await;

    let removed = lexicon_ids::run(&pool, backend).await;

    assert_eq!(removed, 1, "expected exactly the blank id to be removed");
    assert_eq!(
        stored_ids(&pool).await,
        vec![
            "com.example".to_string(),
            "games.gamesgamesgamesgames.game".to_string()
        ]
    );
}

#[tokio::test]
#[serial]
async fn the_sweep_removes_nothing_when_every_id_is_valid() {
    common::require_db!();
    let pool = common::db::test_pool().await;
    let backend = common::db::test_backend();
    common::db::truncate_all(&pool).await;

    insert_lexicon(&pool, backend, "games.gamesgamesgamesgames.game").await;

    let removed = lexicon_ids::run(&pool, backend).await;

    assert_eq!(removed, 0);
    assert_eq!(
        stored_ids(&pool).await,
        vec!["games.gamesgamesgamesgames.game".to_string()]
    );
}
