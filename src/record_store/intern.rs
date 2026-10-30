//! Interned identity: each DID and collection NSID stored once in
//! `happyview_dids`/`happyview_collections` and referenced by id.
//!
//! Rows there are never deleted and an id never changes meaning once
//! committed, so a cached id stays right for the life of the process —
//! provided only committed ids are cached. A transaction that inserts a DID
//! and then rolls back frees its id, and SQLite hands that id to the next
//! DID inserted. An [`InternScope`] therefore collects what one transaction
//! learned, and [`Interner::remember`] adds it to the cache only once the
//! caller has committed.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::{Mutex, RwLock};

use lru::LruCache;
use sqlx::AnyConnection;

use crate::db::{DatabaseBackend, adapt_sql};

/// DIDs kept in the LRU: a few megabytes, and enough for the accounts an
/// instance hears from in a busy hour.
pub const DID_CACHE_CAPACITY: usize = 100_000;

/// Names per insert-and-select round.
const INTERN_CHUNK: usize = 500;

pub struct Interner {
    dids: Mutex<LruCache<String, i64>>,
    collections: RwLock<HashMap<String, i64>>,
}

/// The ids one transaction used or created.
#[derive(Debug, Default)]
pub struct InternScope {
    pub dids: HashMap<String, i64>,
    pub collections: HashMap<String, i64>,
}

#[derive(Clone, Copy)]
enum Names {
    Dids,
    Collections,
}

impl Names {
    fn table(self) -> &'static str {
        match self {
            Self::Dids => "happyview_dids",
            Self::Collections => "happyview_collections",
        }
    }

    fn column(self) -> &'static str {
        match self {
            Self::Dids => "did",
            Self::Collections => "nsid",
        }
    }
}

impl Interner {
    pub fn new(did_capacity: usize) -> Self {
        Self {
            dids: Mutex::new(LruCache::new(
                NonZeroUsize::new(did_capacity).unwrap_or(NonZeroUsize::MIN),
            )),
            collections: RwLock::new(HashMap::new()),
        }
    }

    /// Put the ids of `dids` and `collections` into `scope`, inserting the
    /// names not yet interned, on the caller's transaction. Cached names cost
    /// no query.
    pub async fn intern(
        &self,
        conn: &mut AnyConnection,
        backend: DatabaseBackend,
        scope: &mut InternScope,
        dids: &[&str],
        collections: &[&str],
    ) -> Result<(), sqlx::Error> {
        let mut want_dids: Vec<&str> = Vec::new();
        {
            let mut cache = self.dids.lock().expect("did cache poisoned");
            for did in dids {
                if scope.dids.contains_key(*did) {
                    continue;
                }
                match cache.get(*did) {
                    Some(id) => {
                        scope.dids.insert((*did).to_string(), *id);
                    }
                    None => want_dids.push(*did),
                }
            }
        }
        let mut want_collections: Vec<&str> = Vec::new();
        {
            let cache = self.collections.read().expect("collection cache poisoned");
            for nsid in collections {
                if scope.collections.contains_key(*nsid) {
                    continue;
                }
                match cache.get(*nsid) {
                    Some(id) => {
                        scope.collections.insert((*nsid).to_string(), *id);
                    }
                    None => want_collections.push(*nsid),
                }
            }
        }
        // Sorted, so two Postgres transactions interning overlapping names
        // insert them in the same order and cannot deadlock on them.
        want_dids.sort_unstable();
        want_dids.dedup();
        want_collections.sort_unstable();
        want_collections.dedup();
        scope
            .dids
            .extend(intern_names(conn, backend, Names::Dids, &want_dids).await?);
        scope
            .collections
            .extend(intern_names(conn, backend, Names::Collections, &want_collections).await?);
        Ok(())
    }

    /// Cache what a committed transaction's scope holds.
    pub fn remember(&self, scope: InternScope) {
        let mut dids = self.dids.lock().expect("did cache poisoned");
        for (did, id) in scope.dids {
            dids.put(did, id);
        }
        drop(dids);
        self.collections
            .write()
            .expect("collection cache poisoned")
            .extend(scope.collections);
    }

    pub fn cached_did(&self, did: &str) -> Option<i64> {
        self.dids
            .lock()
            .expect("did cache poisoned")
            .peek(did)
            .copied()
    }

    pub fn cached_collection(&self, nsid: &str) -> Option<i64> {
        self.collections
            .read()
            .expect("collection cache poisoned")
            .get(nsid)
            .copied()
    }
}

async fn intern_names(
    conn: &mut AnyConnection,
    backend: DatabaseBackend,
    names: Names,
    wanted: &[&str],
) -> Result<HashMap<String, i64>, sqlx::Error> {
    let (table, column) = (names.table(), names.column());
    let mut ids = HashMap::with_capacity(wanted.len());
    for chunk in wanted.chunks(INTERN_CHUNK) {
        let insert = adapt_sql(
            &format!(
                "INSERT INTO {table} ({column}) VALUES {} ON CONFLICT ({column}) DO NOTHING",
                vec!["(?)"; chunk.len()].join(", ")
            ),
            backend,
        );
        let mut query = crate::db::query(&insert);
        for name in chunk {
            query = query.bind(*name);
        }
        query.execute(&mut *conn).await?;

        let select = adapt_sql(
            &format!(
                "SELECT id, {column} FROM {table} WHERE {column} IN ({})",
                vec!["?"; chunk.len()].join(", ")
            ),
            backend,
        );
        let mut query = crate::db::query_as::<(i64, String)>(&select);
        for name in chunk {
            query = query.bind(*name);
        }
        for (id, name) in query.fetch_all(&mut *conn).await? {
            ids.insert(name, id);
        }
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_pools;

    fn unique(prefix: &str) -> String {
        format!("{prefix}{}", uuid::Uuid::new_v4().simple())
    }

    #[tokio::test]
    async fn interning_returns_stable_ids_and_caches_them_after_commit() {
        for (pool, backend) in test_pools().await {
            let interner = Interner::new(16);
            let (a, b, nsid) = (
                unique("did:plc:a"),
                unique("did:plc:b"),
                unique("app.test.c"),
            );
            let mut tx = crate::db::begin_write(&pool, backend).await.expect("begin");
            let mut scope = InternScope::default();
            interner
                .intern(
                    &mut tx,
                    backend,
                    &mut scope,
                    &[a.as_str(), b.as_str(), a.as_str()],
                    &[nsid.as_str()],
                )
                .await
                .expect("intern");
            assert_eq!(
                interner.cached_did(&a),
                None,
                "nothing is cached before commit"
            );
            tx.commit().await.expect("commit");
            let (a_id, b_id, c_id) = (scope.dids[&a], scope.dids[&b], scope.collections[&nsid]);
            assert_ne!(a_id, b_id);
            interner.remember(scope);
            assert_eq!(interner.cached_did(&a), Some(a_id), "{backend:?}");
            assert_eq!(interner.cached_collection(&nsid), Some(c_id));

            let fresh = Interner::new(16);
            let mut tx = crate::db::begin_write(&pool, backend).await.expect("begin");
            let mut again = InternScope::default();
            fresh
                .intern(
                    &mut tx,
                    backend,
                    &mut again,
                    &[a.as_str()],
                    &[nsid.as_str()],
                )
                .await
                .expect("intern again");
            tx.commit().await.expect("commit");
            assert_eq!(
                (again.dids[&a], again.collections[&nsid]),
                (a_id, c_id),
                "{backend:?}: ids are stable"
            );
        }
    }

    #[test]
    fn the_did_cache_is_bounded() {
        let interner = Interner::new(2);
        let mut scope = InternScope::default();
        for (i, did) in ["a", "b", "c"].iter().enumerate() {
            scope.dids.insert((*did).to_string(), i as i64);
        }
        interner.remember(scope);
        assert_eq!(
            ["a", "b", "c"]
                .iter()
                .filter(|d| interner.cached_did(d).is_some())
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn the_v2_tables_and_indexes_exist() {
        for (pool, backend) in test_pools().await {
            let sql = match backend {
                DatabaseBackend::Sqlite => {
                    "SELECT name FROM sqlite_master WHERE type = 'index' AND name LIKE 'idx_record%v2%' ORDER BY name"
                }
                DatabaseBackend::Postgres => {
                    "SELECT indexname::text FROM pg_indexes WHERE indexname LIKE 'idx_record%v2%' ORDER BY indexname"
                }
            };
            let names: Vec<String> = crate::db::query_as::<(String,)>(sql)
                .fetch_all(&pool)
                .await
                .expect("list indexes")
                .into_iter()
                .map(|(n,)| n)
                .collect();
            for expected in [
                "idx_record_refs_v2_target",
                "idx_records_v2_collection_created_at_id",
                "idx_records_v2_collection_indexed_at",
                "idx_records_v2_identity",
            ] {
                assert!(
                    names.iter().any(|n| n == expected),
                    "{backend:?} lacks {expected}: {names:?}"
                );
            }
        }
    }

    /// A regression guard: sqlx's SQLite driver turns foreign keys on for
    /// every connection by default, and `with_sqlite_pragmas` sets it again
    /// so the refs cascade never rests on that default alone.
    #[tokio::test]
    async fn foreign_keys_are_on_for_every_pooled_sqlite_connection() {
        let path = std::env::temp_dir().join(format!("hv-fk-{}.db", uuid::Uuid::new_v4()));
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = crate::db::connect(&url, DatabaseBackend::Sqlite).await;
        let mut conns = Vec::new();
        for _ in 0..3 {
            conns.push(pool.acquire().await.expect("acquire"));
        }
        for conn in &mut conns {
            let (on,): (i64,) = crate::db::query_as("PRAGMA foreign_keys")
                .fetch_one(&mut **conn)
                .await
                .expect("read pragma");
            assert_eq!(on, 1);
        }
        drop(conns);
        pool.close().await;
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[tokio::test]
    async fn the_postgres_record_column_is_lz4() {
        for (pool, backend) in test_pools().await {
            if backend != DatabaseBackend::Postgres {
                continue;
            }
            let (method,): (String,) = crate::db::query_as(
                "SELECT attcompression::text FROM pg_attribute \
                 WHERE attrelid = 'happyview_records_v2'::regclass AND attname = 'record'",
            )
            .fetch_one(&pool)
            .await
            .expect("read the column's compression");
            assert_eq!(method, "l");
        }
    }

    /// The migration's lz4 block, run against a scratch table with the
    /// statement broken the ways a server without lz4 breaks it, must keep
    /// the default compression and succeed rather than fail the migration.
    #[tokio::test]
    async fn the_lz4_setting_falls_back_to_the_default_when_refused() {
        const MIGRATION: &str =
            include_str!("../../migrations/postgres/20261011000001_records_v2.sql");
        let start = MIGRATION
            .find("DO $$")
            .expect("the migration has a DO block");
        let end = start
            + MIGRATION[start..]
                .find("END $$;")
                .expect("the DO block ends")
            + 7;
        for (pool, backend) in test_pools().await {
            if backend != DatabaseBackend::Postgres {
                continue;
            }
            let table = format!("hv_lz4_probe_{}", uuid::Uuid::new_v4().simple());
            let block = MIGRATION[start..end].replace("happyview_records_v2", &table);
            let compression = format!(
                "SELECT attcompression::text FROM pg_attribute \
                 WHERE attrelid = '{table}'::regclass AND attname = 'record'"
            );
            crate::db::query(&format!("CREATE TABLE {table} (record TEXT NOT NULL)"))
                .execute(&pool)
                .await
                .expect("create the scratch table");
            for (refusal, statement) in [
                // A build without lz4 raises feature_not_supported; an
                // unknown method (invalid_parameter_value) is the nearest a
                // server with lz4 can give.
                (
                    "an unknown method",
                    block.replace("COMPRESSION lz4", "COMPRESSION hv_none"),
                ),
                // What Postgres 13 and older raise.
                (
                    "unknown syntax",
                    block.replace("SET COMPRESSION lz4", "SET COMPRESSION"),
                ),
            ] {
                assert_ne!(statement, block, "{refusal}: the substitution applied");
                crate::db::query(&statement)
                    .execute(&pool)
                    .await
                    .unwrap_or_else(|e| panic!("{refusal} failed the block: {e}"));
                let (method,): (String,) = crate::db::query_as(&compression)
                    .fetch_one(&pool)
                    .await
                    .expect("read the column's compression");
                assert_eq!(method, "", "{refusal}: the default compression is kept");
            }
            crate::db::query(&block)
                .execute(&pool)
                .await
                .expect("the unbroken block runs");
            let (method,): (String,) = crate::db::query_as(&compression)
                .fetch_one(&pool)
                .await
                .expect("read the column's compression");
            assert_eq!(method, "l", "the unbroken block sets lz4");
            crate::db::query(&format!("DROP TABLE {table}"))
                .execute(&pool)
                .await
                .expect("drop the scratch table");
        }
    }
}
