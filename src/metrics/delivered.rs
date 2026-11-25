//! Records delivered: how many records ingest was handed, by origin, per
//! minute. Kept apart from the script metrics, and counted whether or not
//! those are on, so the count doesn't depend on the script-metrics setting.
//!
//! Each closed minute is written to `happyview_script_metrics` as a row of
//! kind `delivered`, keyed by origin, and folded into hour rows and swept
//! with the rest of the table. A read takes the table and the minutes memory
//! holds past the watermark. The writer and the reader share a lock, so a
//! minute written while a read runs is counted once.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sqlx::AnyPool;

use crate::db::{DatabaseBackend, adapt_sql};

use super::ingest::{ORIGINS, RecordOrigin};

/// A delivered row's `kind`.
pub const KIND: &str = "delivered";

const MINUTE_SECS: u64 = 60;

/// Minutes held for a database that can't be written, two days' worth.
/// Past that the oldest are dropped, so a long outage can't grow memory.
const MAX_HELD_MINUTES: usize = 2 * 24 * 60;

fn align(at: u64, width: u64) -> u64 {
    at - at % width
}

/// A row's `counters`.
#[derive(Serialize, Deserialize, Default, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct Counters {
    pub records: u64,
}

#[derive(Default)]
struct Held {
    /// Minute start to records by origin, for the minutes not yet written.
    minutes: BTreeMap<u64, [u64; ORIGINS.len()]>,
    /// Every minute starting before this is in the table.
    persisted_to: u64,
}

#[derive(Default)]
pub struct Delivered {
    held: Mutex<Held>,
    /// Held across a write and across a read, so neither sees the other
    /// half done.
    io: tokio::sync::Mutex<()>,
}

impl Delivered {
    fn lock(&self) -> std::sync::MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// One record from `origin`, handed to ingest at `at` (seconds).
    pub fn count_at(&self, at: u64, origin: RecordOrigin) {
        let mut held = self.lock();
        let minute = align(at, MINUTE_SECS);
        held.minutes.entry(minute).or_default()[origin.index()] += 1;
        while held.minutes.len() > MAX_HELD_MINUTES {
            held.minutes.pop_first();
        }
    }

    /// Write every minute closed by `now` and move the watermark past it.
    /// Rewriting a minute replaces its row, so a failed write is retried
    /// whole on the next tick. Returns the rows written.
    pub async fn write_closed(
        &self,
        pool: &AnyPool,
        backend: DatabaseBackend,
        instance: &str,
        now: u64,
    ) -> Result<u64, sqlx::Error> {
        let _io = self.io.lock().await;
        let open = align(now, MINUTE_SECS);
        let closed: Vec<(u64, [u64; ORIGINS.len()])> = self
            .lock()
            .minutes
            .range(..open)
            .map(|(m, c)| (*m, *c))
            .collect();
        let mut written = 0;
        if !closed.is_empty() {
            let sql = super::persist::upsert_sql(backend);
            let mut tx = pool.begin().await?;
            for (minute, counts) in &closed {
                for (i, n) in counts.iter().enumerate() {
                    if *n == 0 {
                        continue;
                    }
                    let counters = serde_json::to_string(&Counters { records: *n })
                        .unwrap_or_else(|_| "{}".into());
                    crate::db::query(&sql)
                        .bind(KIND)
                        .bind(ORIGINS[i])
                        .bind("")
                        .bind("minute")
                        .bind(super::rfc3339(*minute))
                        .bind(counters)
                        .bind("{}")
                        .bind(instance)
                        .execute(&mut *tx)
                        .await?;
                    written += 1;
                }
            }
            tx.commit().await?;
        }
        let mut held = self.lock();
        held.minutes.retain(|m, _| *m >= open);
        held.persisted_to = held.persisted_to.max(open);
        Ok(written)
    }

    /// The records delivered from `from` up to `to`, read at `now`: every
    /// instance's rows, and the minutes this one holds that aren't written,
    /// as one point per `width`-second step with records. `from` and `to`
    /// are aligned down to `width`.
    pub async fn read(
        &self,
        pool: &AnyPool,
        backend: DatabaseBackend,
        now: u64,
        from: u64,
        to: u64,
        width: u64,
    ) -> Result<Vec<DeliveredPoint>, sqlx::Error> {
        let (from, to) = (align(from, width), align(to, width));
        let _io = self.io.lock().await;
        let rows: Vec<(String, String, String)> = crate::db::query_as(&adapt_sql(
            "SELECT key, bucket_start, counters FROM happyview_script_metrics \
             WHERE kind = ? AND bucket_start >= ? AND bucket_start < ?",
            backend,
        ))
        .bind(KIND)
        .bind(super::rfc3339(from))
        .bind(super::rfc3339(to))
        .fetch_all(pool)
        .await?;
        let mut steps: BTreeMap<u64, [u64; ORIGINS.len()]> = BTreeMap::new();
        for (key, start, counters) in rows {
            let Some(start) = parse_start(&start) else {
                continue;
            };
            let Ok(c) = serde_json::from_str::<Counters>(&counters) else {
                tracing::warn!("skipping a delivered row that does not read");
                continue;
            };
            steps.entry(align(start, width)).or_default()[RecordOrigin::parse(&key).index()] +=
                c.records;
        }
        let held = self.lock();
        for (minute, counts) in held.minutes.range(held.persisted_to..) {
            if *minute < from || *minute >= to || *minute > now {
                continue;
            }
            let step = steps.entry(align(*minute, width)).or_default();
            for (slot, n) in step.iter_mut().zip(counts) {
                *slot += n;
            }
        }
        Ok(steps
            .into_iter()
            .filter(|(_, counts)| counts.iter().any(|n| *n > 0))
            .map(|(start, counts)| DeliveredPoint {
                start: super::rfc3339(start),
                records: counts.iter().sum(),
                origins: ORIGINS
                    .iter()
                    .zip(counts)
                    .filter(|(_, n)| *n > 0)
                    .map(|(name, n)| (*name, n))
                    .collect(),
            })
            .collect())
    }

    /// Move the watermark to `open`, as a boot that starts empty does: the
    /// table holds everything before it.
    pub fn mark_persisted(&self, open: u64) {
        let mut held = self.lock();
        held.persisted_to = held.persisted_to.max(align(open, MINUTE_SECS));
    }
}

fn parse_start(raw: &str) -> Option<u64> {
    let at = chrono::DateTime::parse_from_rfc3339(raw).ok()?;
    u64::try_from(at.timestamp()).ok()
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

/// Fold a set of delivered rows' counters into one.
pub fn merge(rows: &[(String, String)]) -> String {
    let records = rows
        .iter()
        .filter_map(|(counters, _)| serde_json::from_str::<Counters>(counters).ok())
        .map(|c| c.records)
        .sum();
    serde_json::to_string(&Counters { records }).unwrap_or_else(|_| "{}".into())
}

#[cfg(test)]
mod tests {
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
    }

    #[tokio::test]
    async fn a_failed_write_keeps_the_minutes() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let d = Delivered::default();
        d.count_at(T0 + 10, RecordOrigin::Live);
        pool.close().await;
        assert!(d.write_closed(&pool, SQLITE, "a", T0 + 120).await.is_err());
        assert_eq!(d.lock().minutes.len(), 1);
        assert_eq!(d.lock().persisted_to, 0);
    }

    #[test]
    fn memory_is_bounded() {
        let d = Delivered::default();
        for m in 0..(MAX_HELD_MINUTES as u64 + 10) {
            d.count_at(T0 + m * 60, RecordOrigin::Live);
        }
        let held = d.lock();
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
