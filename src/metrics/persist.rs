//! The table behind the windows longer than memory holds.
//!
//! Once a minute the closed minute buckets of every key that saw runs are
//! written to `happyview_script_metrics`, so a key idle for a minute writes
//! nothing that minute. Once an hour, rows past retention are deleted and
//! minute rows older than [`FOLD_AFTER_SECS`] are folded into hour rows: the
//! counters add and the sketches merge, so an hour row answers exactly what
//! its minutes would have. At boot the last day's minute rows are read back
//! into the rings.
//!
//! What has been written is tracked as one watermark, the start of the
//! oldest minute not yet in the table. A read over a stored window takes
//! the table and, from memory, the minutes from the watermark on, so the
//! minute still open is counted once and only once.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::Hash;
use std::sync::atomic::Ordering;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::AnyPool;

use crate::db::{DatabaseBackend, adapt_sql};
use crate::plugin::ScriptKind;

use super::ingest::{DECISIONS, IngestBucket, IngestSummary};
use super::sketch::Sketch;
use super::window::{
    Accumulate, Bucket, HALF_HOUR_SECS, HALF_HOURS_KEPT, History, HostImportSum, MINUTE_SECS,
    MINUTES_KEPT,
};
use super::{
    IngestKey, Key, Layer, OUTCOMES, ScriptMetrics, ScriptSeries, ScriptSummary, Step, Window,
    point, rfc3339, sort_ingest, sort_scripts,
};

pub const TABLE: &str = "happyview_script_metrics";

const MINUTE: &str = "minute";
const HOUR: &str = "hour";
const HOUR_SECS: u64 = 3_600;

/// How often closed minutes are written.
pub const WRITE_INTERVAL: Duration = Duration::from_secs(60);

/// How often retention and the fold run.
pub const MAINTAIN_EVERY_SECS: u64 = HOUR_SECS;

/// Minute rows older than this are folded into hour rows. Two days, so the
/// last day can always be read back by the minute at boot.
pub const FOLD_AFTER_SECS: u64 = 48 * HOUR_SECS;

fn align(at: u64, width: u64) -> u64 {
    at - at % width
}

fn parse_start(raw: &str) -> Option<u64> {
    let at = chrono::DateTime::parse_from_rfc3339(raw).ok()?;
    u64::try_from(at.timestamp()).ok()
}

/// Names read back from a row: host imports, stages and ingest actions are
/// `&'static str` in memory. They come from a small fixed set, so each is
/// leaked once; past a bound, a name is read as `other` rather than leaked.
fn intern(name: &str) -> &'static str {
    const MAX_NAMES: usize = 4_096;
    static NAMES: LazyLock<Mutex<HashSet<&'static str>>> = LazyLock::new(Mutex::default);
    let mut names = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(held) = names.get(name) {
        return held;
    }
    if names.len() >= MAX_NAMES {
        return "other";
    }
    let leaked: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.insert(leaked);
    leaked
}

/// How a key is spelled in a row's `kind`, `key` and `variant`.
trait RowKey: Sized + Clone + Eq + Hash {
    const KIND: &'static str;
    fn encode(&self) -> (String, String);
    fn decode(key: &str, variant: &str) -> Option<Self>;
}

impl RowKey for Key {
    const KIND: &'static str = "script";

    fn encode(&self) -> (String, String) {
        (
            self.trigger.clone(),
            format!("{}/{}", self.kind.as_str(), self.layer.as_str()),
        )
    }

    fn decode(key: &str, variant: &str) -> Option<Self> {
        let (kind, layer) = variant.split_once('/')?;
        Some(Self {
            trigger: key.to_string(),
            kind: *ScriptKind::ALL.iter().find(|k| k.as_str() == kind)?,
            layer: match layer {
                "project" => Layer::Project,
                "dashboard" => Layer::Dashboard,
                _ => return None,
            },
        })
    }
}

impl RowKey for IngestKey {
    const KIND: &'static str = "ingest";

    fn encode(&self) -> (String, String) {
        (self.collection.clone(), self.action.to_string())
    }

    fn decode(key: &str, variant: &str) -> Option<Self> {
        Some(Self {
            collection: key.to_string(),
            action: intern(variant),
        })
    }
}

/// A bucket as a row's `counters` and `sketches`. Every stored field is
/// named and defaults when missing, so a row written before a field existed
/// still reads, and a count by an unknown name is dropped rather than
/// refusing the row.
trait Stored: Accumulate {
    fn encode(&self) -> (String, String);
    fn decode(counters: &str, sketches: &str) -> Result<Self, serde_json::Error>;
    fn is_empty(&self) -> bool;
}

fn to_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string())
}

fn named_counts(names: &[&'static str], counts: &[u64]) -> BTreeMap<String, u64> {
    names
        .iter()
        .zip(counts)
        .filter(|(_, n)| **n > 0)
        .map(|(name, n)| (name.to_string(), *n))
        .collect()
}

fn indexed_counts<const N: usize>(names: &[&str; N], counts: &BTreeMap<String, u64>) -> [u64; N] {
    let mut out = [0; N];
    for (name, n) in counts {
        if let Some(i) = names.iter().position(|known| known == name) {
            out[i] = *n;
        }
    }
    out
}

#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
#[serde(default)]
struct ScriptCounters {
    runs: u64,
    outcomes: BTreeMap<String, u64>,
    wall_us: u64,
    prepare_us: u64,
    instantiate_us: u64,
    link_us: u64,
    allocate_us: u64,
    instantiated: u64,
    teardown_us: u64,
    torn_down: u64,
    guest_us: u64,
    host_us: u64,
    called: u64,
    settle_us: u64,
    settled: u64,
    host_imports: Vec<ImportSum>,
    memory_peak_max: u64,
    memory_peak_sum: u64,
    memory_measured: u64,
    fuel_sum: u64,
    fuel_measured: u64,
    cache_hits: u64,
    cache_misses: u64,
}

#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
#[serde(default)]
struct ImportSum {
    name: String,
    us: u64,
    calls: u64,
}

#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
#[serde(default)]
struct ScriptSketches {
    wall: Sketch,
    guest: Sketch,
}

impl Stored for Bucket {
    fn encode(&self) -> (String, String) {
        let counters = ScriptCounters {
            runs: self.runs,
            outcomes: named_counts(&OUTCOMES, &self.outcomes),
            wall_us: self.wall_us,
            prepare_us: self.prepare_us,
            instantiate_us: self.instantiate_us,
            link_us: self.link_us,
            allocate_us: self.allocate_us,
            instantiated: self.instantiated,
            teardown_us: self.teardown_us,
            torn_down: self.torn_down,
            guest_us: self.guest_us,
            host_us: self.host_us,
            called: self.called,
            settle_us: self.settle_us,
            settled: self.settled,
            host_imports: self
                .host_imports
                .iter()
                .map(|i| ImportSum {
                    name: i.name.to_string(),
                    us: i.us,
                    calls: i.calls,
                })
                .collect(),
            memory_peak_max: self.memory_peak_max,
            memory_peak_sum: self.memory_peak_sum,
            memory_measured: self.memory_measured,
            fuel_sum: self.fuel_sum,
            fuel_measured: self.fuel_measured,
            cache_hits: self.cache_hits,
            cache_misses: self.cache_misses,
        };
        let sketches = ScriptSketches {
            wall: self.wall.clone(),
            guest: self.guest.clone(),
        };
        (to_json(&counters), to_json(&sketches))
    }

    fn decode(counters: &str, sketches: &str) -> Result<Self, serde_json::Error> {
        let c: ScriptCounters = serde_json::from_str(counters)?;
        let s: ScriptSketches = serde_json::from_str(sketches)?;
        Ok(Self {
            runs: c.runs,
            outcomes: indexed_counts(&OUTCOMES, &c.outcomes),
            wall_us: c.wall_us,
            prepare_us: c.prepare_us,
            instantiate_us: c.instantiate_us,
            link_us: c.link_us,
            allocate_us: c.allocate_us,
            instantiated: c.instantiated,
            teardown_us: c.teardown_us,
            torn_down: c.torn_down,
            guest_us: c.guest_us,
            host_us: c.host_us,
            called: c.called,
            settle_us: c.settle_us,
            settled: c.settled,
            host_imports: c
                .host_imports
                .into_iter()
                .map(|i| HostImportSum {
                    name: intern(&i.name),
                    us: i.us,
                    calls: i.calls,
                })
                .collect(),
            memory_peak_max: c.memory_peak_max,
            memory_peak_sum: c.memory_peak_sum,
            memory_measured: c.memory_measured,
            fuel_sum: c.fuel_sum,
            fuel_measured: c.fuel_measured,
            cache_hits: c.cache_hits,
            cache_misses: c.cache_misses,
            wall: s.wall,
            guest: s.guest,
        })
    }

    fn is_empty(&self) -> bool {
        self.runs == 0
    }
}

#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
#[serde(default)]
struct IngestCounters {
    records: u64,
    decisions: BTreeMap<String, u64>,
    failed: u64,
    timeouts: u64,
    dead_lettered: u64,
    record_us: u64,
    script_us: u64,
    scripted: u64,
    stages: Vec<StageRow>,
}

#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
#[serde(default)]
struct StageRow {
    name: String,
    n: u64,
    us: u64,
}

#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
#[serde(default)]
struct IngestSketches {
    record: Sketch,
}

impl Stored for IngestBucket {
    fn encode(&self) -> (String, String) {
        let counters = IngestCounters {
            records: self.records,
            decisions: named_counts(&DECISIONS, &self.decisions),
            failed: self.failed,
            timeouts: self.timeouts,
            dead_lettered: self.dead_lettered,
            record_us: self.record_us,
            script_us: self.script_us,
            scripted: self.scripted,
            stages: self
                .stages
                .sums()
                .iter()
                .map(|s| StageRow {
                    name: s.name.to_string(),
                    n: s.n,
                    us: s.us,
                })
                .collect(),
        };
        let sketches = IngestSketches {
            record: self.record.clone(),
        };
        (to_json(&counters), to_json(&sketches))
    }

    fn decode(counters: &str, sketches: &str) -> Result<Self, serde_json::Error> {
        let c: IngestCounters = serde_json::from_str(counters)?;
        let s: IngestSketches = serde_json::from_str(sketches)?;
        let mut bucket = Self {
            records: c.records,
            decisions: indexed_counts(&DECISIONS, &c.decisions),
            failed: c.failed,
            timeouts: c.timeouts,
            dead_lettered: c.dead_lettered,
            record_us: c.record_us,
            record: s.record,
            script_us: c.script_us,
            scripted: c.scripted,
            stages: Default::default(),
        };
        for stage in c.stages {
            bucket.stages.add_us(intern(&stage.name), stage.us, stage.n);
        }
        Ok(bucket)
    }

    fn is_empty(&self) -> bool {
        self.records == 0
    }
}

/// One row on its way into the table.
struct RowOut {
    kind: &'static str,
    key: String,
    variant: String,
    start: u64,
    counters: String,
    sketches: String,
}

/// One row read back.
struct RowIn {
    key: String,
    variant: String,
    start: u64,
    counters: String,
    sketches: String,
}

fn closed_rows<K: RowKey, B: Stored>(
    entries: Vec<(K, Arc<Mutex<History<B>>>)>,
    from: u64,
    open: u64,
    out: &mut Vec<RowOut>,
) {
    for (key, history) in entries {
        let history = history.lock().unwrap_or_else(|e| e.into_inner());
        for start in (from..open).step_by(MINUTE_SECS as usize) {
            let Some(bucket) = history.minutes.get(start) else {
                continue;
            };
            if bucket.is_empty() {
                continue;
            }
            let (key, variant) = key.encode();
            let (counters, sketches) = bucket.encode();
            out.push(RowOut {
                kind: K::KIND,
                key,
                variant,
                start,
                counters,
                sketches,
            });
        }
    }
}

fn upsert_sql(backend: DatabaseBackend) -> String {
    adapt_sql(
        "INSERT INTO happyview_script_metrics \
         (kind, key, variant, resolution, bucket_start, counters, sketches) \
         VALUES (?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (kind, key, variant, resolution, bucket_start) \
         DO UPDATE SET counters = excluded.counters, sketches = excluded.sketches",
        backend,
    )
}

/// Write every minute that has closed since the watermark, and move the
/// watermark to the minute still open. Rewriting a minute replaces its row,
/// so a write that fails leaves the watermark where it was and the next
/// tick writes the same minutes again. While metrics are off nothing is
/// written and the watermark stays, so the minutes recorded before the
/// switch went off are written once it is back on, while memory still holds
/// them.
pub async fn write_closed(
    metrics: &ScriptMetrics,
    pool: &AnyPool,
    backend: DatabaseBackend,
    now: u64,
) -> Result<u64, sqlx::Error> {
    if !metrics.enabled() {
        return Ok(0);
    }
    let open = align(now, MINUTE_SECS);
    // The oldest minute the ring can still hold.
    let held = open.saturating_sub((MINUTES_KEPT as u64 - 1) * MINUTE_SECS);
    let from = metrics.persisted_to().max(held);
    if from >= open {
        return Ok(0);
    }
    let mut rows = Vec::new();
    closed_rows(metrics.entries(), from, open, &mut rows);
    closed_rows(metrics.ingest_entries(), from, open, &mut rows);
    if !rows.is_empty() {
        let sql = upsert_sql(backend);
        let mut tx = pool.begin().await?;
        for row in &rows {
            crate::db::query(&sql)
                .bind(row.kind)
                .bind(&row.key)
                .bind(&row.variant)
                .bind(MINUTE)
                .bind(rfc3339(row.start))
                .bind(&row.counters)
                .bind(&row.sketches)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
    }
    metrics.persisted_to.fetch_max(open, Ordering::Relaxed);
    Ok(rows.len() as u64)
}

/// Delete every row starting more than `days` before `now`. `0` deletes
/// nothing.
pub async fn sweep(
    pool: &AnyPool,
    backend: DatabaseBackend,
    now: u64,
    days: u32,
) -> Result<u64, sqlx::Error> {
    if days == 0 {
        return Ok(0);
    }
    let cutoff = rfc3339(now.saturating_sub(u64::from(days) * 86_400));
    let result = crate::db::query(&adapt_sql(
        "DELETE FROM happyview_script_metrics WHERE bucket_start < ?",
        backend,
    ))
    .bind(cutoff)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

fn merge_rows<B: Stored>(rows: &[(String, String)]) -> B {
    let mut out = B::default();
    for (counters, sketches) in rows {
        match B::decode(counters, sketches) {
            Ok(bucket) => out.merge_from(&bucket),
            Err(e) => {
                tracing::warn!(error = %e, "dropping a script metrics row that does not read")
            }
        }
    }
    out
}

/// Fold minute rows older than [`FOLD_AFTER_SECS`] into hour rows, one hour
/// at a time, each in its own transaction: the hour row is written and its
/// minutes deleted together, so a read never sees both or neither. An hour
/// row already there is merged in. Returns the minute rows folded.
pub async fn fold(pool: &AnyPool, backend: DatabaseBackend, now: u64) -> Result<u64, sqlx::Error> {
    let cutoff = rfc3339(align(now.saturating_sub(FOLD_AFTER_SECS), HOUR_SECS));
    let oldest_sql = adapt_sql(
        "SELECT MIN(bucket_start) FROM happyview_script_metrics \
         WHERE resolution = ? AND bucket_start < ?",
        backend,
    );
    let rows_sql = adapt_sql(
        "SELECT kind, key, variant, counters, sketches FROM happyview_script_metrics \
         WHERE resolution = ? AND bucket_start >= ? AND bucket_start < ?",
        backend,
    );
    let hour_sql = adapt_sql(
        "SELECT counters, sketches FROM happyview_script_metrics \
         WHERE kind = ? AND key = ? AND variant = ? AND resolution = ? AND bucket_start = ?",
        backend,
    );
    let delete_sql = adapt_sql(
        "DELETE FROM happyview_script_metrics \
         WHERE resolution = ? AND bucket_start >= ? AND bucket_start < ?",
        backend,
    );
    let upsert = upsert_sql(backend);
    let mut folded = 0;
    loop {
        let oldest: Option<String> = crate::db::query_scalar(&oldest_sql)
            .bind(MINUTE)
            .bind(&cutoff)
            .fetch_one(pool)
            .await?;
        let Some(oldest) = oldest else {
            break;
        };
        let (from, to) = match parse_start(&oldest) {
            Some(start) => {
                let hour = align(start, HOUR_SECS);
                (rfc3339(hour), rfc3339(hour + HOUR_SECS))
            }
            None => {
                // Not a time this table writes. Dropped, or it would be
                // found first again on every pass.
                tracing::warn!(bucket_start = %oldest, "dropping script metrics rows with an unreadable bucket_start");
                crate::db::query(&adapt_sql(
                    "DELETE FROM happyview_script_metrics WHERE resolution = ? AND bucket_start = ?",
                    backend,
                ))
                .bind(MINUTE)
                .bind(&oldest)
                .execute(pool)
                .await?;
                continue;
            }
        };
        let mut tx = pool.begin().await?;
        let rows: Vec<(String, String, String, String, String)> = crate::db::query_as(&rows_sql)
            .bind(MINUTE)
            .bind(&from)
            .bind(&to)
            .fetch_all(&mut *tx)
            .await?;
        folded += rows.len() as u64;
        let mut groups: HashMap<(String, String, String), Vec<(String, String)>> = HashMap::new();
        for (kind, key, variant, counters, sketches) in rows {
            groups
                .entry((kind, key, variant))
                .or_default()
                .push((counters, sketches));
        }
        for ((kind, key, variant), mut rows) in groups {
            let held: Option<(String, String)> = crate::db::query_as(&hour_sql)
                .bind(&kind)
                .bind(&key)
                .bind(&variant)
                .bind(HOUR)
                .bind(&from)
                .fetch_optional(&mut *tx)
                .await?;
            rows.extend(held);
            let encoded = match kind.as_str() {
                k if k == Key::KIND => merge_rows::<Bucket>(&rows).encode(),
                k if k == IngestKey::KIND => merge_rows::<IngestBucket>(&rows).encode(),
                other => {
                    tracing::warn!(
                        kind = other,
                        "dropping script metrics rows of an unknown kind"
                    );
                    continue;
                }
            };
            crate::db::query(&upsert)
                .bind(&kind)
                .bind(&key)
                .bind(&variant)
                .bind(HOUR)
                .bind(&from)
                .bind(&encoded.0)
                .bind(&encoded.1)
                .execute(&mut *tx)
                .await?;
        }
        crate::db::query(&delete_sql)
            .bind(MINUTE)
            .bind(&from)
            .bind(&to)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    Ok(folded)
}

/// What one tick did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TickOutcome {
    pub written: u64,
    pub swept: u64,
    pub folded: u64,
}

/// The writer's state between ticks.
#[derive(Debug, Default)]
pub struct Persister {
    maintained_at: Option<u64>,
}

impl Persister {
    /// Write the minutes closed by `now`, and once an hour sweep and fold.
    /// Retention is re-read every hour, so a change to it lands without a
    /// restart. The sweep and fold run while metrics are off too: they are
    /// about what is already stored.
    pub async fn tick(
        &mut self,
        metrics: &ScriptMetrics,
        pool: &AnyPool,
        backend: DatabaseBackend,
        now: u64,
    ) -> TickOutcome {
        let mut outcome = TickOutcome::default();
        match write_closed(metrics, pool, backend, now).await {
            Ok(n) => outcome.written = n,
            Err(e) => tracing::warn!(error = %e, "failed to write script metrics"),
        }
        if self
            .maintained_at
            .is_some_and(|at| now < at + MAINTAIN_EVERY_SECS)
        {
            return outcome;
        }
        self.maintained_at = Some(now);
        let days = super::retention::retention_days(pool, backend).await;
        match sweep(pool, backend, now, days).await {
            Ok(n) => outcome.swept = n,
            Err(e) => tracing::warn!(error = %e, "failed to sweep script metrics"),
        }
        match fold(pool, backend, now).await {
            Ok(n) => outcome.folded = n,
            Err(e) => tracing::warn!(error = %e, "failed to fold script metrics"),
        }
        outcome
    }
}

/// Write and maintain the table for the life of the process.
pub async fn run(metrics: Arc<ScriptMetrics>, pool: AnyPool, backend: DatabaseBackend) {
    let mut persister = Persister::default();
    loop {
        tokio::time::sleep(WRITE_INTERVAL).await;
        persister
            .tick(&metrics, &pool, backend, super::unix_now())
            .await;
    }
}

fn restore<K: RowKey, B: Stored>(
    insert: impl Fn(K, u64) -> Option<Arc<Mutex<History<B>>>>,
    row: RowIn,
    now: u64,
    minutes_from: u64,
) -> bool {
    let Some(key) = K::decode(&row.key, &row.variant) else {
        return false;
    };
    let bucket = match B::decode(&row.counters, &row.sketches) {
        Ok(bucket) => bucket,
        Err(e) => {
            tracing::warn!(error = %e, "skipping a script metrics row that does not read");
            return false;
        }
    };
    let Some(history) = insert(key, now) else {
        return false;
    };
    let mut history = history.lock().unwrap_or_else(|e| e.into_inner());
    if row.start >= minutes_from {
        history.minutes.merge_bucket(row.start, &bucket);
    }
    history.half_hours.merge_bucket(row.start, &bucket);
    true
}

/// Read the last day's minute rows back into the rings: the last hour into
/// the minute rings, the whole day merged into the half-hour rings. Keys are
/// held up to [`super::MAX_KEYS`] as when they are recorded, a key past it
/// is not counted as overflow. Moves the watermark to the minute open at
/// `now`, since everything before it that memory holds is now the table's.
/// Returns the rows restored.
pub async fn reload(
    metrics: &ScriptMetrics,
    pool: &AnyPool,
    backend: DatabaseBackend,
    now: u64,
) -> Result<u64, sqlx::Error> {
    let open = align(now, MINUTE_SECS);
    let minutes_from = open.saturating_sub((MINUTES_KEPT as u64 - 1) * MINUTE_SECS);
    let day_from =
        align(now, HALF_HOUR_SECS).saturating_sub((HALF_HOURS_KEPT as u64 - 1) * HALF_HOUR_SECS);
    let rows: Vec<(String, String, String, String, String, String)> =
        crate::db::query_as(&adapt_sql(
            "SELECT kind, key, variant, bucket_start, counters, sketches \
             FROM happyview_script_metrics \
             WHERE resolution = ? AND bucket_start >= ? AND bucket_start < ?",
            backend,
        ))
        .bind(MINUTE)
        .bind(rfc3339(day_from))
        .bind(rfc3339(open))
        .fetch_all(pool)
        .await?;
    let mut restored = 0;
    for (kind, key, variant, start, counters, sketches) in rows {
        let Some(start) = parse_start(&start) else {
            continue;
        };
        let row = RowIn {
            key,
            variant,
            start,
            counters,
            sketches,
        };
        let done = match kind.as_str() {
            k if k == Key::KIND => restore::<Key, Bucket>(
                |key, at| metrics.insert(key, at, false),
                row,
                now,
                minutes_from,
            ),
            k if k == IngestKey::KIND => restore::<IngestKey, IngestBucket>(
                |key, at| metrics.insert_ingest(key, at, false),
                row,
                now,
                minutes_from,
            ),
            _ => false,
        };
        restored += u64::from(done);
    }
    metrics.persisted_to.fetch_max(open, Ordering::Relaxed);
    Ok(restored)
}

/// Every bucket of kind `K` from `from` to `now`: the table's rows, and the
/// minutes memory holds that the table does not yet. `only` narrows to one
/// row `key`.
async fn collect<K: RowKey, B: Stored>(
    entries: Vec<(K, Arc<Mutex<History<B>>>)>,
    persisted_to: u64,
    pool: &AnyPool,
    backend: DatabaseBackend,
    only: Option<&str>,
    from: u64,
    now: u64,
) -> Result<Vec<(K, u64, B)>, sqlx::Error> {
    let sql = if only.is_some() {
        "SELECT key, variant, bucket_start, counters, sketches FROM happyview_script_metrics \
         WHERE kind = ? AND key = ? AND bucket_start >= ?"
    } else {
        "SELECT key, variant, bucket_start, counters, sketches FROM happyview_script_metrics \
         WHERE kind = ? AND bucket_start >= ?"
    };
    let mut query = crate::db::query_as(&adapt_sql(sql, backend)).bind(K::KIND);
    if let Some(key) = only {
        query = query.bind(key);
    }
    let rows: Vec<(String, String, String, String, String)> =
        query.bind(rfc3339(from)).fetch_all(pool).await?;
    let mut out = Vec::with_capacity(rows.len());
    for (key, variant, start, counters, sketches) in rows {
        let (Some(key), Some(start)) = (K::decode(&key, &variant), parse_start(&start)) else {
            continue;
        };
        if start > now {
            continue;
        }
        match B::decode(&counters, &sketches) {
            Ok(bucket) => out.push((key, start, bucket)),
            Err(e) => {
                tracing::warn!(error = %e, "skipping a script metrics row that does not read")
            }
        }
    }
    let open = align(now, MINUTE_SECS);
    let held = open.saturating_sub((MINUTES_KEPT as u64 - 1) * MINUTE_SECS);
    let unwritten = align(from.max(persisted_to).max(held), MINUTE_SECS);
    for (key, history) in entries {
        if only.is_some_and(|only| key.encode().0 != only) {
            continue;
        }
        let history = history.lock().unwrap_or_else(|e| e.into_inner());
        for start in (unwritten..=open).step_by(MINUTE_SECS as usize) {
            if let Some(bucket) = history.minutes.get(start)
                && !bucket.is_empty()
            {
                out.push((key.clone(), start, bucket.clone()));
            }
        }
    }
    Ok(out)
}

/// The start of the first of the `width`-second steps a window of `secs`
/// ending at `now` holds. As a ring does, the window holds `secs / width`
/// steps, the newest the one still open.
fn first_step(now: u64, secs: u64, width: u64) -> u64 {
    align(now, width).saturating_sub((secs / width).saturating_sub(1) * width)
}

/// The first hour a stored window ending at `now` reads.
fn window_from(now: u64, window: Window) -> u64 {
    first_step(now, window.secs(), HOUR_SECS)
}

/// As [`ScriptMetrics::scripts_at`], for a window read from the table.
pub async fn scripts(
    metrics: &ScriptMetrics,
    pool: &AnyPool,
    backend: DatabaseBackend,
    now: u64,
    window: Window,
) -> Result<Vec<ScriptSummary>, sqlx::Error> {
    let buckets = collect::<Key, Bucket>(
        metrics.entries(),
        metrics.persisted_to(),
        pool,
        backend,
        None,
        window_from(now, window),
        now,
    )
    .await?;
    let mut merged: HashMap<Key, Bucket> = HashMap::new();
    for (key, _, bucket) in buckets {
        merged.entry(key).or_default().merge(&bucket);
    }
    let mut out: Vec<ScriptSummary> = merged
        .into_iter()
        .filter(|(_, bucket)| bucket.runs > 0)
        .map(|(key, bucket)| ScriptSummary {
            trigger: key.trigger,
            kind: key.kind.as_str(),
            layer: key.layer.as_str(),
            stats: bucket.stats(),
        })
        .collect();
    sort_scripts(&mut out);
    Ok(out)
}

/// As [`ScriptMetrics::series_at`], for a window read from the table, one
/// point per `step` of an hour or a day. Days are UTC days.
pub async fn series(
    metrics: &ScriptMetrics,
    pool: &AnyPool,
    backend: DatabaseBackend,
    now: u64,
    trigger: &str,
    window: Window,
    step: Step,
) -> Result<Option<ScriptSeries>, sqlx::Error> {
    let width = step.secs();
    let first = first_step(now, window.secs(), width);
    let last = align(now, width);
    let buckets = collect::<Key, Bucket>(
        metrics.entries(),
        metrics.persisted_to(),
        pool,
        backend,
        Some(trigger),
        first,
        now,
    )
    .await?;
    let mut per_key: BTreeMap<&'static str, (Key, Bucket)> = BTreeMap::new();
    let mut steps: BTreeMap<u64, Bucket> = BTreeMap::new();
    let mut total = Bucket::default();
    for (key, start, bucket) in &buckets {
        per_key
            .entry(key.layer.as_str())
            .or_insert_with(|| (key.clone(), Bucket::default()))
            .1
            .merge(bucket);
        steps.entry(align(*start, width)).or_default().merge(bucket);
        total.merge(bucket);
    }
    if total.runs == 0 {
        return Ok(None);
    }
    let empty = Bucket::default();
    let points = (first..=last)
        .step_by(width as usize)
        .map(|start| point(start, steps.get(&start).unwrap_or(&empty)))
        .collect();
    let kind = per_key
        .values()
        .next()
        .map(|(key, _)| key.kind.as_str())
        .unwrap_or_default();
    Ok(Some(ScriptSeries {
        trigger: trigger.to_string(),
        kind,
        layers: per_key
            .iter()
            .filter(|(_, (_, bucket))| bucket.runs > 0)
            .map(|(layer, _)| *layer)
            .collect(),
        stats: total.stats(),
        points,
    }))
}

/// As [`ScriptMetrics::ingest_at`], for a window read from the table.
pub async fn ingest(
    metrics: &ScriptMetrics,
    pool: &AnyPool,
    backend: DatabaseBackend,
    now: u64,
    window: Window,
) -> Result<Vec<IngestSummary>, sqlx::Error> {
    let buckets = collect::<IngestKey, IngestBucket>(
        metrics.ingest_entries(),
        metrics.persisted_to(),
        pool,
        backend,
        None,
        window_from(now, window),
        now,
    )
    .await?;
    let mut merged: HashMap<IngestKey, (IngestBucket, u64)> = HashMap::new();
    for (key, start, bucket) in buckets {
        let (held, oldest) = merged
            .entry(key)
            .or_insert_with(|| (IngestBucket::default(), start));
        held.merge_from(&bucket);
        *oldest = (*oldest).min(start);
    }
    let mut out: Vec<IngestSummary> = merged
        .into_iter()
        .filter(|(_, (bucket, _))| bucket.records > 0)
        .map(|(key, (bucket, oldest))| {
            bucket.summary(&key.collection, key.action, now.saturating_sub(oldest) + 1)
        })
        .collect();
    sort_ingest(&mut out);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::ingest::{RecordDecision, RecordSample};
    use super::super::run::RunTimings;
    use super::super::{Outcome, RunSample};
    use super::*;

    /// A day boundary, so hours and half hours line up with it.
    const T0: u64 = 1_793_318_400;
    const SQLITE: DatabaseBackend = DatabaseBackend::Sqlite;

    fn sample(wall_ms: u64, outcome: Outcome) -> RunSample {
        RunSample {
            outcome,
            wall: Duration::from_millis(wall_ms),
            prepare: Duration::from_micros(300),
            timings: RunTimings {
                instantiate: Some(Duration::from_millis(1)),
                guest_call: Some(Duration::from_millis(wall_ms / 2)),
                host_total: Duration::from_micros(100),
                host_by_import: vec![("host_db_query", Duration::from_micros(100), 1)],
                memory_peak_bytes: Some(1 << 20),
                module_cache_hit: Some(true),
                ..Default::default()
            },
        }
    }

    fn record(ms: u64) -> RecordSample {
        RecordSample {
            decision: RecordDecision::Replace,
            record: Duration::from_millis(ms),
            script: Some(Duration::from_millis(ms / 2)),
            stages: vec![
                ("resolve", Duration::from_micros(20)),
                ("write", Duration::from_millis(1)),
            ],
            failed: false,
            timed_out: false,
            dead_lettered: false,
        }
    }

    fn run(m: &ScriptMetrics, at: u64, wall_ms: u64) {
        m.record_at(
            at,
            "xrpc.query:a.b.c",
            ScriptKind::XrpcQuery,
            Layer::Dashboard,
            &sample(wall_ms, Outcome::Ok),
        );
    }

    async fn count(pool: &AnyPool, resolution: &str) -> i64 {
        crate::db::query_scalar(
            "SELECT COUNT(*) FROM happyview_script_metrics WHERE resolution = ?",
        )
        .bind(resolution)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[test]
    fn a_bucket_reads_back_as_it_was_written() {
        let mut bucket = Bucket::default();
        bucket.add(&sample(10, Outcome::Ok));
        bucket.add(&sample(30, Outcome::Timeout));
        let (counters, sketches) = bucket.encode();
        let back = Bucket::decode(&counters, &sketches).unwrap();
        assert_eq!(back.stats(), bucket.stats());
        assert_eq!(back.wall, bucket.wall);
        assert_eq!(back.guest, bucket.guest);
        assert_eq!(back.encode(), (counters, sketches));

        let mut ingest = IngestBucket::default();
        ingest.add_sample(&record(8));
        ingest.add_sample(&record(4));
        let (counters, sketches) = ingest.encode();
        let back = IngestBucket::decode(&counters, &sketches).unwrap();
        assert_eq!(
            back.summary("c", "create", 60),
            ingest.summary("c", "create", 60)
        );
        assert_eq!(back.record, ingest.record);
    }

    #[test]
    fn a_row_missing_newer_fields_still_reads() {
        // As a row written before `settle_us`, `cache_hits` and the guest
        // sketch existed would be, with an outcome this build does not know.
        let counters = r#"{"runs":2,"outcomes":{"ok":1,"vanished":1},"wall_us":5000}"#;
        let sketches = r#"{"wall":{"buckets":{"400":2},"count":2,"min":2500,"max":2500}}"#;
        let bucket = Bucket::decode(counters, sketches).unwrap();
        assert_eq!(bucket.runs, 2);
        assert_eq!(bucket.count(Outcome::Ok), 1);
        assert_eq!(bucket.settle_us, 0);
        assert_eq!(bucket.cache_hits, 0);
        assert_eq!(bucket.guest.count(), 0);
        assert_eq!(bucket.wall.max(), Some(2_500));
        let stats = bucket.stats();
        assert_eq!(stats.wall_ms.mean, Some(2.5));

        let ingest = IngestBucket::decode(r#"{"records":3}"#, "{}").unwrap();
        assert_eq!(ingest.records, 3);
        assert_eq!(ingest.record.count(), 0);
        assert!(Bucket::decode("not json", "{}").is_err());
    }

    #[test]
    fn keys_spell_and_read_back() {
        let key = Key {
            trigger: "record.index:a.b/c".into(),
            kind: ScriptKind::RecordEvent,
            layer: Layer::Project,
        };
        let (k, v) = key.encode();
        assert_eq!(v, "record_event/project");
        assert_eq!(Key::decode(&k, &v), Some(key));
        assert_eq!(Key::decode("t", "record_event/elsewhere"), None);
        assert_eq!(Key::decode("t", "nonsense"), None);
        let ingest = IngestKey {
            collection: "a.b.c".into(),
            action: "create",
        };
        let (k, v) = ingest.encode();
        assert_eq!(IngestKey::decode(&k, &v), Some(ingest));
    }

    #[tokio::test]
    async fn a_minute_is_written_once_and_only_once_it_closes() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let m = ScriptMetrics::new();
        run(&m, T0 + 5, 10);
        m.record_ingest_at((T0 + 6) * 1_000, "a.b.c", "create", None, &record(4));
        // Still open: nothing to write.
        assert_eq!(write_closed(&m, &pool, SQLITE, T0 + 30).await.unwrap(), 0);
        assert_eq!(count(&pool, MINUTE).await, 0);
        // Closed: one script row and one ingest row.
        assert_eq!(write_closed(&m, &pool, SQLITE, T0 + 61).await.unwrap(), 2);
        assert_eq!(write_closed(&m, &pool, SQLITE, T0 + 70).await.unwrap(), 0);
        run(&m, T0 + 75, 10);
        // Idle minutes write nothing, and an earlier minute is not rewritten.
        assert_eq!(
            write_closed(&m, &pool, SQLITE, T0 + 5 * 60).await.unwrap(),
            1
        );
        assert_eq!(count(&pool, MINUTE).await, 3);
    }

    #[tokio::test]
    async fn nothing_is_written_while_metrics_are_off() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let m = ScriptMetrics::new();
        run(&m, T0, 10);
        m.set_enabled(false);
        assert_eq!(write_closed(&m, &pool, SQLITE, T0 + 120).await.unwrap(), 0);
        m.set_enabled(true);
        // What was recorded before the switch went off is still written.
        assert_eq!(write_closed(&m, &pool, SQLITE, T0 + 180).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn folding_minutes_into_an_hour_equals_merging_them() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let m = ScriptMetrics::new();
        let mut direct = Bucket::default();
        for minute in 0..50 {
            let wall = 5 + minute * 3;
            run(&m, T0 + minute * 60 + 10, wall);
            direct.add(&sample(wall, Outcome::Ok));
        }
        assert_eq!(
            write_closed(&m, &pool, SQLITE, T0 + 50 * 60).await.unwrap(),
            50
        );
        // Not yet two days old.
        assert_eq!(fold(&pool, SQLITE, T0 + 47 * 3_600).await.unwrap(), 0);
        assert_eq!(
            fold(&pool, SQLITE, T0 + FOLD_AFTER_SECS + 3_600)
                .await
                .unwrap(),
            50
        );
        assert_eq!(count(&pool, MINUTE).await, 0);
        assert_eq!(count(&pool, HOUR).await, 1);
        let (counters, sketches): (String, String) = crate::db::query_as(
            "SELECT counters, sketches FROM happyview_script_metrics WHERE resolution = 'hour'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let hour = Bucket::decode(&counters, &sketches).unwrap();
        assert_eq!(hour.stats(), direct.stats());
        assert_eq!(hour.wall, direct.wall);
    }

    #[tokio::test]
    async fn retention_deletes_rows_before_the_cutoff() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let m = ScriptMetrics::new();
        run(&m, T0, 10);
        run(&m, T0 + 600, 10);
        write_closed(&m, &pool, SQLITE, T0 + 3_000).await.unwrap();
        let days = 3;
        let cutoff = T0 + 300 + u64::from(days) * 86_400;
        assert_eq!(sweep(&pool, SQLITE, cutoff, 0).await.unwrap(), 0);
        assert_eq!(sweep(&pool, SQLITE, cutoff, days).await.unwrap(), 1);
        let left: String =
            crate::db::query_scalar("SELECT bucket_start FROM happyview_script_metrics")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(left, rfc3339(T0 + 600));
    }

    #[tokio::test]
    async fn the_tick_maintains_once_an_hour() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let m = ScriptMetrics::new();
        let mut p = Persister::default();
        run(&m, T0, 10);
        let first = p.tick(&m, &pool, SQLITE, T0 + 60).await;
        assert_eq!(first.written, 1);
        // Three days on, the row is past the fold but kept by retention.
        let later = T0 + 3 * 86_400;
        assert_eq!(p.tick(&m, &pool, SQLITE, later).await.folded, 1);
        assert_eq!(count(&pool, HOUR).await, 1);
        crate::admin::settings::put_setting(&pool, super::super::retention::KEY, "2", SQLITE)
            .await
            .unwrap();
        // Within the hour, nothing is maintained.
        assert_eq!(p.tick(&m, &pool, SQLITE, later + 60).await.swept, 0);
        assert_eq!(p.tick(&m, &pool, SQLITE, later + 3_600).await.swept, 1);
        assert_eq!(count(&pool, HOUR).await, 0);
    }

    #[tokio::test]
    async fn a_restart_reads_the_last_day_back() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let before = ScriptMetrics::new();
        let now = T0 + 86_400;
        // Twenty hours ago, and ten minutes ago.
        run(&before, now - 20 * 3_600, 10);
        run(&before, now - 600, 20);
        run(&before, now - 590, 20);
        before.record_ingest_at((now - 300) * 1_000, "a.b.c", "create", None, &record(4));
        // The first row has left memory's minute ring, so it is written by
        // an earlier tick, as it would have been.
        write_closed(&before, &pool, SQLITE, now - 20 * 3_600 + 60)
            .await
            .unwrap();
        write_closed(&before, &pool, SQLITE, now).await.unwrap();

        let after = ScriptMetrics::new();
        assert_eq!(reload(&after, &pool, SQLITE, now + 5).await.unwrap(), 3);
        let hour = after.scripts_at(now + 5, Window::Hour);
        assert_eq!(hour.len(), 1);
        assert_eq!(hour[0].stats.runs, 2);
        assert_eq!(after.scripts_at(now + 5, Window::Day)[0].stats.runs, 3);
        assert_eq!(
            after.scripts_at(now + 5, Window::Hour),
            before.scripts_at(now + 5, Window::Hour)
        );
        assert_eq!(after.ingest_at(now + 5, Window::Hour)[0].records, 1);
        // Already in the table, so not written again.
        assert_eq!(
            write_closed(&after, &pool, SQLITE, now + 120)
                .await
                .unwrap(),
            0
        );
        // And not counted twice by a stored window.
        let week = scripts(&after, &pool, SQLITE, now + 5, Window::Week)
            .await
            .unwrap();
        assert_eq!(week[0].stats.runs, 3);
    }

    #[tokio::test]
    async fn a_stored_window_adds_what_memory_has_not_written() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let m = ScriptMetrics::new();
        // Two in the morning, so the last minutes fall in the day still open.
        let now = T0 + 5 * 86_400 + 7_200;
        run(&m, now - 4 * 86_400, 10);
        write_closed(&m, &pool, SQLITE, now - 4 * 86_400 + 60)
            .await
            .unwrap();
        fold(&pool, SQLITE, now).await.unwrap();
        run(&m, now - 120, 10);
        write_closed(&m, &pool, SQLITE, now - 60).await.unwrap();
        // Still open, so only in memory.
        run(&m, now - 5, 10);
        let week = scripts(&m, &pool, SQLITE, now, Window::Week).await.unwrap();
        assert_eq!(week[0].stats.runs, 3);
        let read = series(
            &m,
            &pool,
            SQLITE,
            now,
            "xrpc.query:a.b.c",
            Window::Week,
            Step::Day,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(read.points.len(), 7);
        assert_eq!(read.points.iter().map(|p| p.runs).sum::<u64>(), 3);
        assert_eq!(read.points.last().unwrap().runs, 2);
        assert_eq!(read.layers, vec!["dashboard"]);
        assert!(
            series(&m, &pool, SQLITE, now, "nope", Window::Week, Step::Hour)
                .await
                .unwrap()
                .is_none()
        );
        m.record_ingest_at((now - 5) * 1_000, "a.b.c", "create", None, &record(4));
        let rows = ingest(&m, &pool, SQLITE, now, Window::Month).await.unwrap();
        assert_eq!(rows[0].records, 1);
    }
}
