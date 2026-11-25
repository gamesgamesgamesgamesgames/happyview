//! Counters kept apart from the script metrics, and counted whether or not
//! those are on: per minute, by key, for one `kind` of row.
//!
//! Each closed minute is added to its row in `happyview_script_metrics`, of
//! that kind and keyed by name, and taken out of memory once the write
//! commits; the rows fold into hour rows with the rest of the table. Memory
//! therefore holds exactly what isn't written, and a read adds the two. The
//! writer and the reader share a lock, so a minute written while a read runs
//! is counted once. Adding rather than replacing means a count that arrives
//! late, or from a second process under the same instance id, adds to the
//! row instead of overwriting it.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sqlx::AnyPool;

use crate::db::{DatabaseBackend, adapt_sql};

const MINUTE_SECS: u64 = 60;

/// The fewest days counter rows are kept, whatever the table's retention:
/// a month and a margin, for the usage meter's reads.
pub const MIN_KEPT_DAYS: u32 = 35;

/// Minutes held for a database that can't be written, two days' worth.
/// Past that the oldest are dropped, so a long outage can't grow memory.
pub(crate) const MAX_HELD_MINUTES: usize = 2 * 24 * 60;

pub(crate) fn align(at: u64, width: u64) -> u64 {
    at - at % width
}

/// A row's `counters`. `records` is the count, whatever is counted: the
/// name is the first counter's.
#[derive(Serialize, Deserialize, Default, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct Counters {
    pub records: u64,
}

pub(crate) struct Held<const N: usize> {
    /// Minute start to counts by key, not yet written.
    pub(crate) minutes: BTreeMap<u64, [u64; N]>,
}

/// Counts by key, per minute, for rows of one `kind`. A stored key this
/// build doesn't know counts as the last of `keys`.
pub struct MinuteCounter<const N: usize> {
    kind: &'static str,
    keys: [&'static str; N],
    held: Mutex<Held<N>>,
    /// Held across a write and across a read, so neither sees the other
    /// half done.
    io: tokio::sync::Mutex<()>,
}

impl<const N: usize> MinuteCounter<N> {
    pub fn new(kind: &'static str, keys: [&'static str; N]) -> Self {
        Self {
            kind,
            keys,
            held: Mutex::new(Held {
                minutes: BTreeMap::new(),
            }),
            io: tokio::sync::Mutex::new(()),
        }
    }

    pub fn kind(&self) -> &'static str {
        self.kind
    }

    pub fn keys(&self) -> &[&'static str; N] {
        &self.keys
    }

    fn index_of(&self, key: &str) -> usize {
        self.keys.iter().position(|k| *k == key).unwrap_or(N - 1)
    }
    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, Held<N>> {
        self.held.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// One count of `keys[index]` at `at` (seconds).
    pub fn add_at(&self, at: u64, index: usize) {
        let mut held = self.lock();
        let minute = align(at, MINUTE_SECS);
        held.minutes.entry(minute).or_insert([0; N])[index.min(N - 1)] += 1;
        while held.minutes.len() > MAX_HELD_MINUTES {
            if let Some((minute, counts)) = held.minutes.pop_first() {
                tracing::warn!(
                    minute,
                    kind = self.kind,
                    count = counts.iter().sum::<u64>(),
                    "dropping counts that could not be written"
                );
            }
        }
    }

    /// Add every minute closed by `now` to its row. A failed write leaves
    /// memory as it was, for the next tick. Returns the rows written.
    pub async fn write_closed(
        &self,
        pool: &AnyPool,
        backend: DatabaseBackend,
        instance: &str,
        now: u64,
    ) -> Result<u64, sqlx::Error> {
        let _io = self.io.lock().await;
        let open = align(now, MINUTE_SECS);
        let closed: Vec<(u64, [u64; N])> = self
            .lock()
            .minutes
            .range(..open)
            .map(|(m, c)| (*m, *c))
            .collect();
        let mut written = 0;
        if !closed.is_empty() {
            let sql = super::persist::upsert_sql(backend);
            let held_sql = adapt_sql(
                "SELECT counters FROM happyview_script_metrics \
                 WHERE kind = ? AND key = ? AND variant = '' AND resolution = 'minute' \
                 AND bucket_start = ? AND instance = ?",
                backend,
            );
            let mut tx = crate::db::begin_write(pool, backend).await?;
            // Another process writing under the same instance id adds to the
            // same rows: on Postgres, take turns, so neither read goes stale.
            // SQLite's write transaction already excludes the other.
            if backend == DatabaseBackend::Postgres {
                crate::db::query(&adapt_sql(
                    "SELECT pg_advisory_xact_lock(hashtext(?))",
                    backend,
                ))
                .bind(format!("happyview_{}:{instance}", self.kind))
                .execute(&mut *tx)
                .await?;
            }
            for (minute, counts) in &closed {
                for (i, n) in counts.iter().enumerate() {
                    if *n == 0 {
                        continue;
                    }
                    let start = super::rfc3339(*minute);
                    let before: Option<String> = crate::db::query_scalar(&held_sql)
                        .bind(self.kind)
                        .bind(self.keys[i])
                        .bind(&start)
                        .bind(instance)
                        .fetch_optional(&mut *tx)
                        .await?;
                    let before = before
                        .and_then(|c| serde_json::from_str::<Counters>(&c).ok())
                        .map_or(0, |c| c.records);
                    let counters = serde_json::to_string(&Counters {
                        records: before.saturating_add(*n),
                    })
                    .unwrap_or_else(|_| "{}".into());
                    crate::db::query(&sql)
                        .bind(self.kind)
                        .bind(self.keys[i])
                        .bind("")
                        .bind("minute")
                        .bind(&start)
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
        // What was written leaves memory; what arrived since stays.
        let mut held = self.lock();
        for (minute, counts) in &closed {
            if let Some(left) = held.minutes.get_mut(minute) {
                for (slot, n) in left.iter_mut().zip(counts) {
                    *slot = slot.saturating_sub(*n);
                }
                if left.iter().all(|n| *n == 0) {
                    held.minutes.remove(minute);
                }
            }
        }
        Ok(written)
    }

    /// The counts from `from` up to `to`, read at `now`: every instance's
    /// rows, and the minutes this one holds that aren't written, as one
    /// `(start, counts by key)` per `width`-second step with any. `from` and
    /// `to` are aligned down to `width`.
    pub async fn read_steps(
        &self,
        pool: &AnyPool,
        backend: DatabaseBackend,
        now: u64,
        from: u64,
        to: u64,
        width: u64,
    ) -> Result<Vec<(u64, [u64; N])>, sqlx::Error> {
        let (from, to) = (align(from, width), align(to, width));
        let _io = self.io.lock().await;
        let rows: Vec<(String, String, String)> = crate::db::query_as(&adapt_sql(
            "SELECT key, bucket_start, counters FROM happyview_script_metrics \
             WHERE kind = ? AND bucket_start >= ? AND bucket_start < ?",
            backend,
        ))
        .bind(self.kind)
        .bind(super::rfc3339(from))
        .bind(super::rfc3339(to))
        .fetch_all(pool)
        .await?;
        let mut steps: BTreeMap<u64, [u64; N]> = BTreeMap::new();
        for (key, start, counters) in rows {
            let Some(start) = parse_start(&start) else {
                continue;
            };
            let Ok(c) = serde_json::from_str::<Counters>(&counters) else {
                tracing::warn!(
                    kind = self.kind,
                    "skipping a counter row that does not read"
                );
                continue;
            };
            steps.entry(align(start, width)).or_insert([0; N])[self.index_of(&key)] += c.records;
        }
        let held = self.lock();
        for (minute, counts) in &held.minutes {
            if *minute < from || *minute >= to || *minute > now {
                continue;
            }
            let step = steps.entry(align(*minute, width)).or_insert([0; N]);
            for (slot, n) in step.iter_mut().zip(counts) {
                *slot += n;
            }
        }
        Ok(steps
            .into_iter()
            .filter(|(_, counts)| counts.iter().any(|n| *n > 0))
            .collect())
    }

    /// `counts` as a map of the keys that saw any.
    pub fn named(&self, counts: [u64; N]) -> BTreeMap<&'static str, u64> {
        self.keys
            .iter()
            .zip(counts)
            .filter(|(_, n)| *n > 0)
            .map(|(name, n)| (*name, n))
            .collect()
    }
}

fn parse_start(raw: &str) -> Option<u64> {
    let at = chrono::DateTime::parse_from_rfc3339(raw).ok()?;
    u64::try_from(at.timestamp()).ok()
}

/// Fold a set of counter rows' counters into one.
pub fn merge(rows: &[(String, String)]) -> String {
    let records = rows
        .iter()
        .filter_map(|(counters, _)| serde_json::from_str::<Counters>(counters).ok())
        .map(|c| c.records)
        .sum();
    serde_json::to_string(&Counters { records }).unwrap_or_else(|_| "{}".into())
}
