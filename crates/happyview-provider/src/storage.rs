use async_trait::async_trait;

use crate::{db::DatabaseBackend, types::RecordKey};

#[derive(Clone)]
pub struct ReadRoute {
    /// A pool on which `happyview_records_v2`, `happyview_dids`,
    /// `happyview_collections` and `happyview_record_refs_v2` resolve to
    /// relations with the Phase 3 columns.
    pub pool: sqlx::AnyPool,
    pub backend: DatabaseBackend,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Delta {
    /// Show the stored record as it is (clears any delta).
    Proceed,
    Replace {
        cid: String,
        rev: Option<String>,
        body: String,
    },
    Hide,
    /// A local write awaiting its echo.
    Pending {
        cid: String,
        rev: Option<String>,
        body: String,
        created_at_us: i64,
    },
}

#[async_trait]
pub trait RecordStorage: Send + Sync + 'static {
    fn claims(&self, collection: &str) -> bool;
    fn read_route(&self, collection: &str) -> ReadRoute;
    /// `for_rev: Some(r)`: apply only while the stored record's rev is
    /// still `r` (the delta was computed from that version; a newer one is
    /// on its way). `None`: apply unconditionally (local writes, admin
    /// deletes, and record deletes).
    ///
    /// A record delete from the store's feed arrives as `Delta::Proceed`
    /// with `for_rev: None`: clear this instance's delta for the key
    /// whatever rev it was made at, so no Replace or Hide outlives the
    /// record and covers a later re-create of the same key. An admin delete
    /// is a `Delta::Hide` with `for_rev: None` instead.
    async fn apply_delta(
        &self,
        key: &RecordKey,
        delta: Delta,
        for_rev: Option<&str>,
    ) -> Result<(), sqlx::Error>;
    async fn forget_account(
        &self,
        did: &str,
        collections: Option<&[String]>,
    ) -> Result<u64, sqlx::Error>;
    async fn delete_collection_batch(
        &self,
        collection: &str,
        limit: i64,
    ) -> Result<u64, sqlx::Error>;
    async fn is_live(&self, uri: &str) -> Result<bool, sqlx::Error>;
    fn unavailable(&self, e: &sqlx::Error) -> bool {
        crate::db::is_unavailable(e)
    }
    /// Collections it currently claims, for stats and label GC.
    fn claimed(&self) -> Vec<String>;
}
