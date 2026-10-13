//! Periodic SQLite upkeep.
//!
//! A WAL only rewinds when a checkpoint has copied every frame back *and* no
//! reader is still using the log. With dozens of pooled connections some
//! reader is nearly always active, so SQLite's own PASSIVE autocheckpoints
//! never get that moment and the log grows until shutdown. A TRUNCATE
//! checkpoint waits for readers to move off the log and then empties it. This
//! task runs one on a timer, and a daily `PRAGMA optimize` keeps planner
//! statistics fresh.

use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use sqlx::{AnyConnection, AnyPool};

use crate::db::DatabaseBackend;

/// Seconds between TRUNCATE checkpoints unless
/// `SQLITE_CHECKPOINT_INTERVAL_SECS` says otherwise.
pub const DEFAULT_CHECKPOINT_INTERVAL_SECS: u64 = 60;

/// Consecutive busy checkpoints after which the task warns, once per streak.
pub const BUSY_WARN_AFTER: u32 = 10;

/// How long a checkpoint waits for readers before reporting busy. A pending
/// TRUNCATE holds off new writers, so this stays well below the pool's 5 s
/// busy timeout: a busy checkpoint costs nothing, a stalled writer costs
/// ingest.
pub const CHECKPOINT_BUSY_TIMEOUT_MS: u64 = 1000;

/// Quick retries for a checkpoint that came back busy without waiting.
/// SQLite never waits on the checkpoint lock, so a TRUNCATE that collides
/// with a writer's own PASSIVE autocheckpoint reports busy at once. That
/// collision clears in milliseconds, and without a retry it costs a whole
/// interval of WAL growth.
pub const LOCK_COLLISION_RETRIES: u32 = 5;

/// Pause between those retries.
pub const LOCK_COLLISION_RETRY_DELAY: Duration = Duration::from_millis(20);

/// How often `PRAGMA optimize` runs after the one at startup.
pub const OPTIMIZE_INTERVAL: Duration = Duration::from_secs(86_400);

/// What one TRUNCATE checkpoint reported.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CheckpointOutcome {
    pub at: String,
    /// The checkpoint could not finish because a reader or writer held on.
    pub busy: bool,
    /// Frames in the WAL when the checkpoint ran.
    pub wal_frames: i64,
    /// Frames copied back into the database file.
    pub checkpointed_frames: i64,
}

/// The latest outcome, for the admin database page. Process-wide because the
/// process has exactly one SQLite database.
static LAST_CHECKPOINT: Mutex<Option<CheckpointOutcome>> = Mutex::new(None);

pub fn last_checkpoint() -> Option<CheckpointOutcome> {
    LAST_CHECKPOINT.lock().ok().and_then(|last| last.clone())
}

/// Parse `SQLITE_CHECKPOINT_INTERVAL_SECS`. `0` disables the checkpoint task;
/// anything unparseable falls back to the default.
pub fn parse_checkpoint_interval_secs(raw: Option<&str>) -> u64 {
    raw.and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_CHECKPOINT_INTERVAL_SECS)
}

pub fn checkpoint_interval_secs() -> u64 {
    parse_checkpoint_interval_secs(
        std::env::var("SQLITE_CHECKPOINT_INTERVAL_SECS")
            .ok()
            .as_deref(),
    )
}

/// Run one `PRAGMA wal_checkpoint(TRUNCATE)` on a pooled connection.
///
/// The connection's busy timeout is shortened for the checkpoint and put back
/// before it returns to the pool. If putting it back fails, the connection is
/// closed instead: one left at the short timeout would fail ordinary writes
/// under contention. The same holds if the future is dropped mid-checkpoint.
pub async fn checkpoint_truncate(pool: &AnyPool) -> Result<CheckpointOutcome, sqlx::Error> {
    let mut guard = ShortTimeoutGuard {
        conn: pool.acquire().await?,
        armed: true,
    };
    let conn = &mut guard.conn;
    crate::db::query(&pragma_sql(&format!(
        "PRAGMA busy_timeout = {CHECKPOINT_BUSY_TIMEOUT_MS}"
    )))
    .execute(&mut **conn)
    .await?;
    let checkpoint =
        crate::db::query_as::<(i64, i64, i64)>(&pragma_sql("PRAGMA wal_checkpoint(TRUNCATE)"))
            .fetch_one(&mut **conn)
            .await;
    let restored = crate::db::query(&pragma_sql(&format!(
        "PRAGMA busy_timeout = {}",
        crate::db::SQLITE_BUSY_TIMEOUT_MS
    )))
    .execute(&mut **conn)
    .await;
    if restored.is_ok() {
        guard.armed = false;
    }
    let (busy, wal_frames, checkpointed_frames) = checkpoint?;
    restored?;
    let outcome = CheckpointOutcome {
        at: crate::db::now_rfc3339(),
        busy: busy != 0,
        wal_frames,
        checkpointed_frames,
    };
    if let Ok(mut last) = LAST_CHECKPOINT.lock() {
        *last = Some(outcome.clone());
    }
    Ok(outcome)
}

/// Closes the connection on drop while it may still carry the short busy
/// timeout, so a cancelled checkpoint never returns it to the pool that way.
struct ShortTimeoutGuard {
    conn: sqlx::pool::PoolConnection<sqlx::Any>,
    armed: bool,
}

impl Drop for ShortTimeoutGuard {
    fn drop(&mut self) {
        if self.armed {
            self.conn.close_on_drop();
        }
    }
}

fn pragma_sql(sql: &str) -> String {
    crate::db::adapt_sql(sql, DatabaseBackend::Sqlite)
}

/// Bytes of free pages one `incremental_vacuum` step hands back. Releasing
/// pages writes the WAL (about half their size, plus any page moved to fill
/// a gap), so a database that freed gigabytes at once would otherwise grow
/// the WAL by gigabytes before the next checkpoint could truncate it.
pub const RECLAIM_CHUNK_BYTES: i64 = 32 << 20;

/// What [`reclaim_free_pages`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reclaimed {
    /// Free pages returned to the filesystem.
    pub pages: i64,
    /// `incremental_vacuum` steps that returned some.
    pub chunks: u32,
    /// TRUNCATE checkpoints run between and after them.
    pub checkpoints: u32,
    /// Those checkpoints that a reader or writer kept from emptying the WAL.
    pub busy_checkpoints: u32,
    /// The most frames the WAL held at any of those checkpoints.
    pub max_wal_frames: i64,
}

/// Where [`reclaim`] runs: one connection (at boot, when nothing else uses
/// the database) or the pool of a running instance.
enum On<'a> {
    Conn(&'a mut AnyConnection),
    Pool(&'a AnyPool),
}

impl On<'_> {
    async fn scalar(&mut self, sql: &str) -> Result<i64, sqlx::Error> {
        let query = crate::db::query_scalar::<i64>(sql);
        match self {
            On::Conn(conn) => query.fetch_one(&mut **conn).await,
            On::Pool(pool) => query.fetch_one(*pool).await,
        }
    }

    async fn execute(&mut self, sql: &str) -> Result<(), sqlx::Error> {
        let query = crate::db::query(sql);
        match self {
            On::Conn(conn) => query.execute(&mut **conn).await.map(drop),
            On::Pool(pool) => query.execute(*pool).await.map(drop),
        }
    }

    /// `(busy, wal_frames)`: the frames a PASSIVE checkpoint finds in the
    /// WAL (a successful TRUNCATE reports none), then the TRUNCATE that
    /// empties it. On a pool the TRUNCATE has [`checkpoint_truncate`]'s short
    /// busy timeout, so writers are not held up for long.
    async fn checkpoint(&mut self) -> Result<(bool, i64), sqlx::Error> {
        let passive = pragma_sql("PRAGMA wal_checkpoint(PASSIVE)");
        let truncate = pragma_sql("PRAGMA wal_checkpoint(TRUNCATE)");
        match self {
            On::Conn(conn) => {
                let (_, frames, _) = crate::db::query_as::<(i64, i64, i64)>(&passive)
                    .fetch_one(&mut **conn)
                    .await?;
                let (busy, _, _) = crate::db::query_as::<(i64, i64, i64)>(&truncate)
                    .fetch_one(&mut **conn)
                    .await?;
                Ok((busy != 0, frames))
            }
            On::Pool(pool) => {
                let (_, frames, _) = crate::db::query_as::<(i64, i64, i64)>(&passive)
                    .fetch_one(*pool)
                    .await?;
                Ok((checkpoint_truncate(pool).await?.busy, frames))
            }
        }
    }
}

/// Return the database's free pages to the filesystem `chunk_bytes` at a
/// time, with a TRUNCATE checkpoint after each step, so the WAL never holds
/// more than about one step's worth. On a database without incremental
/// vacuum (`auto_vacuum` 0) the first step frees nothing and this stops.
pub async fn reclaim_free_pages_on(
    conn: &mut AnyConnection,
    chunk_bytes: i64,
) -> Result<Reclaimed, sqlx::Error> {
    reclaim(On::Conn(conn), chunk_bytes).await
}

/// [`reclaim_free_pages_on`] for a running instance, on its pool.
pub async fn reclaim_free_pages(
    pool: &AnyPool,
    chunk_bytes: i64,
) -> Result<Reclaimed, sqlx::Error> {
    reclaim(On::Pool(pool), chunk_bytes).await
}

async fn reclaim(mut on: On<'_>, chunk_bytes: i64) -> Result<Reclaimed, sqlx::Error> {
    let page_size = on.scalar("PRAGMA page_size").await?.max(512);
    let chunk_pages = (chunk_bytes / page_size).max(1);
    let mut done = Reclaimed::default();
    let mut free = on.scalar("PRAGMA freelist_count").await?;
    while free > 0 {
        on.execute(&format!("PRAGMA incremental_vacuum({chunk_pages})"))
            .await?;
        let left = on.scalar("PRAGMA freelist_count").await?;
        if left >= free {
            break;
        }
        done.pages += free - left;
        done.chunks += 1;
        free = left;
        let (busy, frames) = on.checkpoint().await?;
        done.checkpoints += 1;
        done.max_wal_frames = done.max_wal_frames.max(frames);
        if busy {
            done.busy_checkpoints += 1;
        }
    }
    // Every checkpoint busy means a long-lived reader kept the WAL from
    // rewinding, so it grew by every step: the growth the steps exist to
    // prevent, and worth saying so once.
    if done.checkpoints > 0 && done.busy_checkpoints == done.checkpoints {
        tracing::warn!(
            steps = done.chunks,
            wal_bytes = done.max_wal_frames * (page_size + WAL_FRAME_HEADER_BYTES),
            "every checkpoint between incremental vacuum steps was busy; the write-ahead log was not emptied"
        );
    }
    Ok(done)
}

/// Bytes of header before each page in a WAL frame.
const WAL_FRAME_HEADER_BYTES: i64 = 24;

/// Counts consecutive busy checkpoints so the task warns once per streak
/// rather than every minute.
#[derive(Debug, Default)]
pub struct BusyTracker {
    consecutive: u32,
    warned: bool,
}

impl BusyTracker {
    /// Record one outcome. Returns true exactly when the warning is due.
    pub fn observe(&mut self, busy: bool) -> bool {
        if !busy {
            self.consecutive = 0;
            self.warned = false;
            return false;
        }
        self.consecutive = self.consecutive.saturating_add(1);
        if self.consecutive >= BUSY_WARN_AFTER && !self.warned {
            self.warned = true;
            return true;
        }
        false
    }
}

/// Run `attempt`, retrying while it comes back busy faster than the busy
/// timeout could have elapsed, which means it lost the checkpoint lock rather
/// than waited on readers. A busy result after waiting on readers is not
/// retried: another wait would hold off writers again for nothing.
async fn retry_lock_collisions<F, Fut>(mut attempt: F) -> Result<CheckpointOutcome, sqlx::Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<CheckpointOutcome, sqlx::Error>>,
{
    let waited_on_readers = Duration::from_millis(CHECKPOINT_BUSY_TIMEOUT_MS / 2);
    let mut retries = 0;
    loop {
        let started = tokio::time::Instant::now();
        let outcome = attempt().await?;
        if !outcome.busy
            || started.elapsed() >= waited_on_readers
            || retries == LOCK_COLLISION_RETRIES
        {
            return Ok(outcome);
        }
        retries += 1;
        tokio::time::sleep(LOCK_COLLISION_RETRY_DELAY).await;
    }
}

async fn run_checkpoint(pool: &AnyPool, busy: &mut BusyTracker) {
    match retry_lock_collisions(|| checkpoint_truncate(pool)).await {
        Ok(outcome) => {
            if outcome.busy {
                tracing::debug!(
                    wal_frames = outcome.wal_frames,
                    checkpointed = outcome.checkpointed_frames,
                    "WAL checkpoint was busy; will retry"
                );
            }
            if busy.observe(outcome.busy) {
                tracing::warn!(
                    "the WAL checkpoint has been busy {BUSY_WARN_AFTER} times in a row; \
                     long-running readers are keeping the write-ahead log from shrinking"
                );
            }
        }
        Err(e) => tracing::warn!(error = %e, "WAL checkpoint failed"),
    }
}

/// Resolves on the interval's next tick, or never when there is no interval.
async fn next_tick(tick: Option<&mut tokio::time::Interval>) {
    match tick {
        Some(tick) => {
            tick.tick().await;
        }
        None => std::future::pending::<()>().await,
    }
}

/// The maintenance loop. A no-op on Postgres. `checkpoint_every` of `None`
/// disables checkpoints but keeps the daily optimize.
pub async fn run(pool: AnyPool, backend: DatabaseBackend, checkpoint_every: Option<Duration>) {
    if backend != DatabaseBackend::Sqlite {
        return;
    }
    let mut busy = BusyTracker::default();
    let mut checkpoint_tick = checkpoint_every.map(|every| {
        let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick
    });
    // Startup already ran `optimize` (see `db::connect`), so the first one
    // here is a day out.
    let mut optimize_tick = tokio::time::interval_at(
        tokio::time::Instant::now() + OPTIMIZE_INTERVAL,
        OPTIMIZE_INTERVAL,
    );
    optimize_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = next_tick(checkpoint_tick.as_mut()) => run_checkpoint(&pool, &mut busy).await,
            _ = optimize_tick.tick() => {
                if let Err(e) = crate::db::sqlite_optimize(&pool).await {
                    tracing::warn!(error = %e, "daily PRAGMA optimize failed");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use serial_test::serial;

    use super::*;

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn checkpoint_interval_defaults_to_sixty_seconds() {
        assert_eq!(parse_checkpoint_interval_secs(None), 60);
        assert_eq!(parse_checkpoint_interval_secs(Some("banana")), 60);
        assert_eq!(parse_checkpoint_interval_secs(Some("15")), 15);
        assert_eq!(parse_checkpoint_interval_secs(Some("0")), 0, "0 disables");
    }

    #[test]
    fn a_busy_streak_warns_once_and_a_success_resets_it() {
        let mut busy = BusyTracker::default();
        let warnings: Vec<bool> = (0..BUSY_WARN_AFTER + 3)
            .map(|_| busy.observe(true))
            .collect();
        assert_eq!(warnings.iter().filter(|w| **w).count(), 1);
        assert!(warnings[(BUSY_WARN_AFTER - 1) as usize]);

        assert!(!busy.observe(false));
        let again: Vec<bool> = (0..BUSY_WARN_AFTER).map(|_| busy.observe(true)).collect();
        assert_eq!(
            again.iter().filter(|w| **w).count(),
            1,
            "a new streak warns again"
        );
    }

    fn outcome(busy: bool) -> CheckpointOutcome {
        CheckpointOutcome {
            at: String::new(),
            busy,
            wal_frames: if busy { -1 } else { 0 },
            checkpointed_frames: if busy { -1 } else { 0 },
        }
    }

    /// Runs `retry_lock_collisions` over scripted attempts, each taking
    /// `took`, and returns the final outcome and how many
    /// attempts ran.
    async fn retry_script(script: Vec<bool>, took: Duration) -> (CheckpointOutcome, usize) {
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&attempts);
        let result = retry_lock_collisions(|| {
            let n = counter.fetch_add(1, Ordering::SeqCst);
            let busy = script.get(n).copied().unwrap_or(true);
            async move {
                tokio::time::sleep(took).await;
                Ok::<_, sqlx::Error>(outcome(busy))
            }
        })
        .await
        .expect("checkpoint");
        (result, attempts.load(Ordering::SeqCst))
    }

    #[tokio::test]
    async fn a_checkpoint_that_lost_the_lock_is_retried_until_it_runs() {
        let (result, attempts) = retry_script(vec![true, true, false], Duration::ZERO).await;
        assert!(!result.busy);
        assert_eq!(attempts, 3);
    }

    #[tokio::test]
    async fn lock_collision_retries_are_bounded() {
        let (result, attempts) = retry_script(vec![], Duration::ZERO).await;
        assert!(result.busy);
        assert_eq!(attempts, 1 + LOCK_COLLISION_RETRIES as usize);
    }

    #[tokio::test]
    async fn a_checkpoint_busy_after_waiting_on_readers_is_not_retried() {
        let waited = Duration::from_millis(CHECKPOINT_BUSY_TIMEOUT_MS);
        let (result, attempts) = retry_script(vec![true, false], waited).await;
        assert!(result.busy);
        assert_eq!(attempts, 1);
    }

    #[tokio::test]
    async fn a_checkpoint_returns_its_connection_with_the_normal_busy_timeout() {
        crate::db::install_drivers();
        let path = std::env::temp_dir().join(format!("hv-ckpt-{}.db", uuid::Uuid::new_v4()));
        let url = format!("sqlite://{}?mode=rwc", path.display());
        // One connection, so the one the checkpoint borrowed is the one read back.
        let pool = sqlx::pool::PoolOptions::<sqlx::Any>::new()
            .max_connections(1)
            .connect(&url)
            .await
            .expect("connect");
        crate::db::query("PRAGMA journal_mode = WAL")
            .execute(&pool)
            .await
            .expect("enable WAL");
        crate::db::query("CREATE TABLE t (id INTEGER PRIMARY KEY)")
            .execute(&pool)
            .await
            .expect("create table");
        crate::db::query("INSERT INTO t (id) VALUES (1)")
            .execute(&pool)
            .await
            .expect("insert row");

        let outcome = checkpoint_truncate(&pool).await.expect("checkpoint");
        assert!(!outcome.busy, "{outcome:?}");

        let (timeout,): (i64,) = crate::db::query_as("PRAGMA busy_timeout")
            .fetch_one(&pool)
            .await
            .expect("read busy_timeout");
        assert_eq!(timeout, crate::db::SQLITE_BUSY_TIMEOUT_MS as i64);

        drop(pool);
        remove_db_files(&path);
    }

    /// Writes 6000 rows of 4 KiB into a file-backed database while three
    /// staggered readers keep a read transaction open at every instant, and
    /// returns the largest the `-wal` file got and how many times it was seen
    /// to shrink sharply (a truncate).
    ///
    /// Overlapping readers are what stopped the WAL on the managed tenant
    /// from ever rewinding: SQLite's own PASSIVE checkpoints can copy frames
    /// back but cannot restart the log while a reader holds a snapshot in it.
    async fn wal_peak_under_load(checkpoint_every: Option<Duration>) -> (u64, u32) {
        let path = std::env::temp_dir().join(format!("hv-wal-{}.db", uuid::Uuid::new_v4()));
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = crate::db::connect(&url, DatabaseBackend::Sqlite).await;
        crate::db::query("CREATE TABLE wal_load (id INTEGER PRIMARY KEY, body BLOB NOT NULL)")
            .execute(&pool)
            .await
            .expect("create table");

        let maintenance = checkpoint_every
            .map(|every| tokio::spawn(run(pool.clone(), DatabaseBackend::Sqlite, Some(every))));

        let stop = Arc::new(AtomicBool::new(false));
        let mut readers = Vec::new();
        for i in 0..3u64 {
            let pool = pool.clone();
            let stop = Arc::clone(&stop);
            readers.push(tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(i * 10)).await;
                while !stop.load(Ordering::Relaxed) {
                    let mut tx = pool.begin().await.expect("begin read");
                    let _: (i64,) = crate::db::query_as("SELECT COUNT(*) FROM wal_load")
                        .fetch_one(&mut *tx)
                        .await
                        .expect("read");
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    tx.commit().await.expect("end read");
                }
            }));
        }

        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let body = vec![7u8; 4096];
        let mut peak = 0u64;
        let mut last = 0u64;
        let mut truncations = 0u32;
        for i in 0..6000i64 {
            crate::db::query("INSERT INTO wal_load (id, body) VALUES (?, ?)")
                .bind(i)
                .bind(body.clone())
                .execute(&pool)
                .await
                .expect("insert");
            if i % 10 == 0 {
                let size = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
                peak = peak.max(size);
                if last > MIB && size < last / 2 {
                    truncations += 1;
                }
                last = size;
            }
        }

        stop.store(true, Ordering::Relaxed);
        for reader in readers {
            reader.await.expect("reader task");
        }
        if let Some(handle) = maintenance {
            handle.abort();
        }
        drop(pool);
        remove_db_files(&path);
        (peak, truncations)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[serial]
    async fn the_checkpoint_task_bounds_the_wal_under_concurrent_readers() {
        let (unchecked, unchecked_truncations) = wal_peak_under_load(None).await;
        let (checked, checked_truncations) =
            wal_peak_under_load(Some(Duration::from_millis(50))).await;
        assert!(
            unchecked > 16 * MIB,
            "without the task the WAL should grow with the writes (peak {unchecked} bytes)"
        );
        assert_eq!(
            unchecked_truncations, 0,
            "overlapping readers should keep the WAL from ever rewinding on its own"
        );
        assert!(
            checked_truncations >= 1,
            "with the task the WAL should be truncated at least once (peak {checked} bytes)"
        );
        assert!(
            checked < unchecked / 2,
            "with the task the WAL should stay well under the unchecked peak \
             (checked {checked}, unchecked {unchecked} bytes)"
        );
    }

    #[tokio::test]
    async fn a_busy_checkpoint_still_restores_the_normal_busy_timeout() {
        crate::db::install_drivers();
        let path = std::env::temp_dir().join(format!("hv-busy-{}.db", uuid::Uuid::new_v4()));
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = sqlx::pool::PoolOptions::<sqlx::Any>::new()
            .max_connections(3)
            .connect(&url)
            .await
            .expect("connect");
        crate::db::query("PRAGMA journal_mode = WAL")
            .execute(&pool)
            .await
            .expect("enable WAL");
        crate::db::query("CREATE TABLE t (id INTEGER PRIMARY KEY)")
            .execute(&pool)
            .await
            .expect("create table");

        // A reader parked mid-snapshot, then a write behind it, leaves frames
        // the TRUNCATE cannot finish until the reader leaves.
        let mut reader = pool.begin().await.expect("begin read");
        let _: (i64,) = crate::db::query_as("SELECT COUNT(*) FROM t")
            .fetch_one(&mut *reader)
            .await
            .expect("read");
        crate::db::query("INSERT INTO t (id) VALUES (1)")
            .execute(&pool)
            .await
            .expect("insert row");

        let outcome = checkpoint_truncate(&pool).await.expect("checkpoint");
        assert!(outcome.busy, "{outcome:?}");

        reader.commit().await.expect("end read");
        let mut a = pool.acquire().await.expect("acquire a");
        let mut b = pool.acquire().await.expect("acquire b");
        let mut c = pool.acquire().await.expect("acquire c");
        for conn in [&mut a, &mut b, &mut c] {
            let (timeout,): (i64,) = crate::db::query_as("PRAGMA busy_timeout")
                .fetch_one(&mut **conn)
                .await
                .expect("read busy_timeout");
            assert_eq!(timeout, crate::db::SQLITE_BUSY_TIMEOUT_MS as i64);
        }

        drop((a, b, c));
        drop(pool);
        remove_db_files(&path);
    }

    /// A file database of its own, holding `mib` MiB of free pages from a
    /// dropped table, on a one-connection pool that never checkpoints by
    /// itself. Its page cache holds `CACHE_PAGES`: an `incremental_vacuum`
    /// step that dirties more pages than that spills them into the WAL,
    /// while a smaller one writes only the few pages it keeps (the freed
    /// ones are cut off the end of the file and never reach the WAL). A
    /// fixed cache makes that split the same on every run; the default one
    /// is shared with SQLite's process-wide memory accounting.
    async fn database_with_free_pages(auto_vacuum: &str, mib: usize) -> (AnyPool, PathBuf) {
        crate::db::install_drivers();
        let path = std::env::temp_dir().join(format!("hv-reclaim-{}.db", uuid::Uuid::new_v4()));
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = sqlx::pool::PoolOptions::<sqlx::Any>::new()
            .max_connections(1)
            .connect(&url)
            .await
            .expect("connect");
        for sql in [
            format!("PRAGMA auto_vacuum = {auto_vacuum}"),
            "PRAGMA journal_mode = WAL".to_string(),
            "PRAGMA wal_autocheckpoint = 0".to_string(),
            format!("PRAGMA cache_size = {CACHE_PAGES}"),
            "CREATE TABLE keep (id INTEGER PRIMARY KEY)".to_string(),
            "CREATE TABLE gone (id INTEGER PRIMARY KEY, body BLOB NOT NULL)".to_string(),
            format!(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {}) \
                 INSERT INTO gone (id, body) SELECT i, randomblob(4000) FROM n",
                mib * 256
            ),
            "PRAGMA wal_checkpoint(TRUNCATE)".to_string(),
            "DROP TABLE gone".to_string(),
            "PRAGMA wal_checkpoint(TRUNCATE)".to_string(),
        ] {
            crate::db::query(&sql).execute(&pool).await.expect(&sql);
        }
        (pool, path)
    }

    const CACHE_PAGES: i64 = 100;
    /// Smaller than the cache, so a step never spills.
    const CHUNK_PAGES: i64 = 64;

    async fn freelist(pool: &AnyPool) -> i64 {
        crate::db::query_scalar::<i64>("PRAGMA freelist_count")
            .fetch_one(pool)
            .await
            .expect("freelist_count")
    }

    #[tokio::test]
    async fn reclaiming_runs_in_chunks_and_checkpoints_between_them() {
        let (pool, path) = database_with_free_pages("INCREMENTAL", 8).await;
        let free = freelist(&pool).await;
        assert!(free >= 2000, "{free} free pages");

        let mut conn = pool.acquire().await.expect("acquire");
        let done = reclaim_free_pages_on(&mut conn, CHUNK_PAGES * 4096)
            .await
            .expect("reclaim");
        drop(conn);

        assert_eq!(done.pages, free);
        assert!(done.chunks >= (free / CHUNK_PAGES) as u32, "{done:?}");
        assert_eq!(
            done.checkpoints, done.chunks,
            "a checkpoint after every chunk"
        );
        assert!(
            done.max_wal_frames < CHUNK_PAGES,
            "the WAL held less than one step's pages: {done:?}"
        );
        assert_eq!(freelist(&pool).await, 0);
        assert_eq!(
            std::fs::metadata(format!("{}-wal", path.display()))
                .map(|m| m.len())
                .unwrap_or(0),
            0,
            "the last checkpoint emptied the WAL"
        );
        drop(pool);
        remove_db_files(&path);
    }

    #[tokio::test]
    async fn reclaiming_in_one_chunk_is_what_fills_the_wal() {
        let (pool, path) = database_with_free_pages("INCREMENTAL", 8).await;
        let mut conn = pool.acquire().await.expect("acquire");
        let done = reclaim_free_pages_on(&mut conn, i64::MAX / 2)
            .await
            .expect("reclaim");
        assert_eq!(done.chunks, 1);
        assert!(
            done.max_wal_frames >= done.pages / 2,
            "one step past the cache spills most of the freed pages into the WAL: {done:?}"
        );
        drop(conn);
        drop(pool);
        remove_db_files(&path);
    }

    #[tokio::test]
    async fn a_reader_that_keeps_every_checkpoint_busy_is_counted() {
        let (pool, path) = database_with_free_pages("INCREMENTAL", 1).await;
        let free = freelist(&pool).await;
        drop(pool);
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = sqlx::pool::PoolOptions::<sqlx::Any>::new()
            .max_connections(2)
            .connect(&url)
            .await
            .expect("connect");
        let mut reader = pool.begin().await.expect("begin read");
        let _: (i64,) = crate::db::query_as("SELECT COUNT(*) FROM keep")
            .fetch_one(&mut *reader)
            .await
            .expect("read");

        let done = reclaim_free_pages(&pool, (free + 1) / 2 * 4096)
            .await
            .expect("reclaim");
        assert_eq!(done.pages, free);
        assert!(done.checkpoints > 0, "{done:?}");
        assert_eq!(done.busy_checkpoints, done.checkpoints, "{done:?}");

        reader.rollback().await.expect("end read");
        drop(pool);
        remove_db_files(&path);
    }

    #[tokio::test]
    async fn reclaiming_without_incremental_vacuum_stops_at_once() {
        let (pool, path) = database_with_free_pages("NONE", 1).await;
        assert!(freelist(&pool).await > 0);
        let done = reclaim_free_pages(&pool, 256 * 4096)
            .await
            .expect("reclaim");
        assert_eq!(done, Reclaimed::default(), "nothing to do, and no loop");
        drop(pool);
        remove_db_files(&path);
    }

    fn remove_db_files(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }
}
