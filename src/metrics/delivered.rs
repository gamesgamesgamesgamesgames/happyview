//! Records delivered: how many records ingest was handed, by origin, per
//! minute, counted independently of the script-metrics setting. See
//! [`super::counter`] for how they're kept.

use std::collections::BTreeMap;

use serde::Serialize;
use sqlx::AnyPool;

use crate::db::DatabaseBackend;

use super::counter::MinuteCounter;
use super::ingest::{ORIGINS, RecordOrigin};

pub use super::counter::{MIN_KEPT_DAYS, merge};

/// A delivered row's `kind`.
pub const KIND: &str = "delivered";

pub struct Delivered(MinuteCounter<{ ORIGINS.len() }>);

impl Default for Delivered {
    fn default() -> Self {
        Self(MinuteCounter::new(KIND, ORIGINS))
    }
}

impl std::ops::Deref for Delivered {
    type Target = MinuteCounter<{ ORIGINS.len() }>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Delivered {
    /// One record from `origin`, handed to ingest at `at` (seconds).
    pub fn count_at(&self, at: u64, origin: RecordOrigin) {
        self.0.add_at(at, origin.index());
    }

    /// The records delivered from `from` up to `to`, read at `now`, as one
    /// point per `width`-second step with any.
    pub async fn read(
        &self,
        pool: &AnyPool,
        backend: DatabaseBackend,
        now: u64,
        from: u64,
        to: u64,
        width: u64,
    ) -> Result<Vec<DeliveredPoint>, sqlx::Error> {
        Ok(self
            .0
            .read_steps(pool, backend, now, from, to, width)
            .await?
            .into_iter()
            .map(|(start, counts)| DeliveredPoint {
                start: super::rfc3339(start),
                records: counts.iter().sum(),
                origins: self.0.named(counts),
            })
            .collect())
    }
}

/// The records ingest was handed in one step, by origin.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct DeliveredPoint {
    /// RFC 3339, UTC.
    pub start: String,
    pub records: u64,
    /// Every origin that saw records in the step.
    pub origins: BTreeMap<&'static str, u64>,
}

#[cfg(test)]
mod tests {
    use super::super::counter::MAX_HELD_MINUTES;
    use super::*;

    const T0: u64 = 1_793_318_400;
    const SQLITE: DatabaseBackend = DatabaseBackend::Sqlite;

    #[tokio::test]
    async fn counts_each_origin_from_the_table_and_memory_once() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let d = Delivered::default();
        d.count_at(T0 + 10, RecordOrigin::Live);
        d.count_at(T0 + 20, RecordOrigin::Live);
        d.count_at(T0 + 70, RecordOrigin::Backfill);
        // Still open: nothing to write.
        assert_eq!(
            d.write_closed(&pool, SQLITE, "a", T0 + 30).await.unwrap(),
            0
        );
        assert_eq!(
            d.write_closed(&pool, SQLITE, "a", T0 + 130).await.unwrap(),
            2
        );
        // In memory, in the open minute.
        d.count_at(T0 + 130, RecordOrigin::StorePass);
        // Another instance's row.
        crate::db::query(&super::super::persist::upsert_sql(SQLITE))
            .bind(KIND)
            .bind("retry")
            .bind("")
            .bind("minute")
            .bind(super::super::rfc3339(T0 + 300))
            .bind(r#"{"records":4}"#)
            .bind("{}")
            .bind("b")
            .execute(&pool)
            .await
            .unwrap();

        let hour = d
            .read(&pool, SQLITE, T0 + 140, T0, T0 + 3_600, 3_600)
            .await
            .unwrap();
        assert_eq!(
            hour,
            vec![DeliveredPoint {
                start: super::super::rfc3339(T0),
                records: 8,
                origins: BTreeMap::from([
                    ("backfill", 1),
                    ("live", 2),
                    ("retry", 4),
                    ("store_pass", 1)
                ]),
            }]
        );
        // Written again: the minute moves from memory to the table, and is
        // still counted once.
        d.write_closed(&pool, SQLITE, "a", T0 + 200).await.unwrap();
        let again = d
            .read(&pool, SQLITE, T0 + 210, T0, T0 + 3_600, 3_600)
            .await
            .unwrap();
        assert_eq!(again[0].records, 8);
        // `to` is exclusive, and an empty range has no points.
        let none = d
            .read(&pool, SQLITE, T0 + 210, T0 + 3_600, T0 + 7_200, 3_600)
            .await
            .unwrap();
        assert!(none.is_empty());
    }

    #[tokio::test]
    async fn a_late_count_and_a_second_writer_add_to_the_row() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let d = Delivered::default();
        d.count_at(T0 + 10, RecordOrigin::Live);
        d.write_closed(&pool, SQLITE, "a", T0 + 70).await.unwrap();
        // Late, for a minute already written.
        d.count_at(T0 + 20, RecordOrigin::Live);
        // Another process under the same id.
        let other = Delivered::default();
        other.count_at(T0 + 30, RecordOrigin::Live);
        other
            .write_closed(&pool, SQLITE, "a", T0 + 70)
            .await
            .unwrap();
        let read = d
            .read(&pool, SQLITE, T0 + 80, T0, T0 + 60, 60)
            .await
            .unwrap();
        assert_eq!(read[0].records, 3, "the late count is read from memory");
        d.write_closed(&pool, SQLITE, "a", T0 + 130).await.unwrap();
        assert!(d.0.lock().minutes.is_empty());
        let read = d
            .read(&pool, SQLITE, T0 + 140, T0, T0 + 60, 60)
            .await
            .unwrap();
        assert_eq!(read[0].records, 3);
    }

    #[tokio::test]
    async fn folded_rows_count_the_same() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let d = Delivered::default();
        for m in 0..5 {
            d.count_at(T0 + m * 60, RecordOrigin::Live);
            d.count_at(T0 + m * 60 + 1, RecordOrigin::Live);
        }
        d.write_closed(&pool, SQLITE, "a", T0 + 600).await.unwrap();
        let now = T0 + super::super::persist::FOLD_AFTER_SECS + 7_200;
        let folded = super::super::persist::fold(&pool, SQLITE, now, "a")
            .await
            .unwrap();
        assert_eq!(folded, 5);
        let points = d
            .read(&pool, SQLITE, now, T0, T0 + 86_400, 86_400)
            .await
            .unwrap();
        assert_eq!(points[0].records, 10);
        // The XRPC counter's rows fold the same way.
        let x = super::super::xrpc::XrpcRequests::default();
        x.count_at(T0, &axum::http::Method::POST);
        x.count_at(T0 + 61, &axum::http::Method::POST);
        x.write_closed(&pool, SQLITE, "a", T0 + 600).await.unwrap();
        assert_eq!(
            super::super::persist::fold(&pool, SQLITE, now, "a")
                .await
                .unwrap(),
            2
        );
        let xs = x
            .read(&pool, SQLITE, now, T0, T0 + 86_400, 86_400)
            .await
            .unwrap();
        assert_eq!(xs[0].methods.get("procedure"), Some(&2));
    }

    #[tokio::test]
    async fn a_failed_write_keeps_the_minutes() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let d = Delivered::default();
        d.count_at(T0 + 10, RecordOrigin::Live);
        pool.close().await;
        assert!(d.write_closed(&pool, SQLITE, "a", T0 + 120).await.is_err());
        assert_eq!(d.0.lock().minutes.len(), 1);
    }

    #[tokio::test]
    async fn delivered_rows_outlive_a_short_retention() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let d = Delivered::default();
        d.count_at(T0, RecordOrigin::Live);
        d.write_closed(&pool, SQLITE, "a", T0 + 60).await.unwrap();
        let sweep = |days_later: u64, keep: u32| {
            super::super::persist::sweep(&pool, SQLITE, T0 + days_later * 86_400, keep)
        };
        let x = super::super::xrpc::XrpcRequests::default();
        x.count_at(T0, &axum::http::Method::GET);
        x.write_closed(&pool, SQLITE, "a", T0 + 60).await.unwrap();
        assert_eq!(sweep(10, 1).await.unwrap(), 0);
        assert_eq!(sweep(36, 1).await.unwrap(), 2, "both counters' rows");
    }

    #[test]
    fn memory_is_bounded() {
        let d = Delivered::default();
        for m in 0..(MAX_HELD_MINUTES as u64 + 10) {
            d.count_at(T0 + m * 60, RecordOrigin::Live);
        }
        let held = d.0.lock();
        assert_eq!(held.minutes.len(), MAX_HELD_MINUTES);
        assert_eq!(*held.minutes.keys().next().unwrap(), T0 + 600);
    }

    #[test]
    fn merging_rows_adds_their_records() {
        let rows = vec![
            (r#"{"records":2}"#.to_string(), "{}".to_string()),
            (r#"{"records":5}"#.to_string(), "{}".to_string()),
        ];
        assert_eq!(merge(&rows), r#"{"records":7}"#);
    }
}
