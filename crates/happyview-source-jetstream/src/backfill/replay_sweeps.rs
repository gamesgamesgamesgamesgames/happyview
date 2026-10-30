//! Rows that make a replay job resumable: one per planSnapshot sweep
//! (`happyview_backfill_replay_sweeps`), and the blocks a retry job refetches
//! (`happyview_backfill_replay_retries`). Every write here is load-bearing for
//! resume, so errors propagate instead of being logged and dropped.

use happyview_provider::db::adapt_sql;

use super::JobState;

pub(crate) const MAX_PLAN_COLLECTIONS: usize = 100;
pub(crate) const MAX_PLAN_DIDS: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SweepKind {
    Commits,
    Markers,
}

impl SweepKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Commits => "commits",
            Self::Markers => "markers",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sweep {
    pub sweep: i32,
    pub kind: SweepKind,
    pub collections: Vec<String>,
    pub dids: Option<Vec<String>>,
    pub after_seq: i64,
    pub cursor_seq: i64,
    pub tip_seq: Option<i64>,
    pub last_did_id: Option<i64>,
    pub done: bool,
}

/// The commit sweeps of a job: its collections in chunks of 100, each with the
/// job's DIDs when it targets accounts, then (for an account job) its marker
/// sweeps, the DIDs in chunks of 10,000. A network job's marker sweeps are
/// added later, from the DIDs the commit sweeps left holding records.
pub fn initial_sweeps(
    collections: &[String],
    dids: Option<&[String]>,
    after_seq: i64,
) -> Vec<Sweep> {
    let mut sweeps = Vec::new();
    for chunk in collections.chunks(MAX_PLAN_COLLECTIONS) {
        sweeps.push(Sweep {
            sweep: sweeps.len() as i32,
            kind: SweepKind::Commits,
            collections: chunk.to_vec(),
            dids: dids.map(<[String]>::to_vec),
            after_seq,
            cursor_seq: after_seq,
            tip_seq: None,
            last_did_id: None,
            done: false,
        });
    }
    for chunk in dids.unwrap_or_default().chunks(MAX_PLAN_DIDS) {
        sweeps.push(Sweep {
            sweep: sweeps.len() as i32,
            kind: SweepKind::Markers,
            collections: Vec::new(),
            dids: Some(chunk.to_vec()),
            after_seq,
            cursor_seq: after_seq,
            tip_seq: None,
            last_did_id: None,
            done: false,
        });
    }
    sweeps
}

type SweepRow = (
    i32,
    String,
    String,
    Option<String>,
    i64,
    i64,
    Option<i64>,
    Option<i64>,
    i32,
);

pub async fn load(state: &JobState, job_id: &str) -> Result<Vec<Sweep>, sqlx::Error> {
    let rows: Vec<SweepRow> = happyview_provider::db::query_as(&adapt_sql(
        "SELECT sweep, kind, collections, dids, after_seq, cursor_seq, tip_seq, last_did_id, done \
         FROM happyview_backfill_replay_sweeps WHERE job_id = ? ORDER BY sweep",
        state.db_backend,
    ))
    .bind(job_id)
    .fetch_all(&state.backfill_db)
    .await?;
    rows.into_iter()
        .map(
            |(
                sweep,
                kind,
                collections,
                dids,
                after_seq,
                cursor_seq,
                tip_seq,
                last_did_id,
                done,
            )| {
                // A row that does not parse must stop the job: read as empty,
                // `collections` would plan every commit in the archive and
                // `dids` would widen an account sweep to the network (P1).
                let corrupt = |what: &str, e: String| {
                    sqlx::Error::Decode(
                        format!("replay sweep {sweep} of job {job_id} has unreadable {what}: {e}")
                            .into(),
                    )
                };
                let kind = match kind.as_str() {
                    "commits" => SweepKind::Commits,
                    "markers" => SweepKind::Markers,
                    other => return Err(corrupt("kind", format!("{other:?}"))),
                };
                let collections: Vec<String> = serde_json::from_str(&collections)
                    .map_err(|e| corrupt("collections", e.to_string()))?;
                let dids: Option<Vec<String>> = dids
                    .map(|d| serde_json::from_str(&d))
                    .transpose()
                    .map_err(|e| corrupt("dids", e.to_string()))?;
                if kind == SweepKind::Commits && collections.is_empty() {
                    return Err(corrupt("collections", "a commit sweep has none".into()));
                }
                if kind == SweepKind::Markers && dids.as_ref().is_none_or(Vec::is_empty) {
                    return Err(corrupt("dids", "a marker sweep has none".into()));
                }
                Ok(Sweep {
                    sweep,
                    kind,
                    collections,
                    dids,
                    after_seq,
                    cursor_seq,
                    tip_seq,
                    last_did_id,
                    done: done != 0,
                })
            },
        )
        .collect()
}

pub async fn insert(state: &JobState, job_id: &str, sweeps: &[Sweep]) -> Result<(), sqlx::Error> {
    let sql = adapt_sql(
        "INSERT INTO happyview_backfill_replay_sweeps \
         (job_id, sweep, kind, collections, dids, after_seq, cursor_seq, tip_seq, last_did_id, done) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 0)",
        state.db_backend,
    );
    for sweep in sweeps {
        let collections = serde_json::to_string(&sweep.collections).expect("strings serialize");
        let dids = sweep
            .dids
            .as_ref()
            .map(|d| serde_json::to_string(d).expect("strings serialize"));
        happyview_provider::db::retry_on_busy(|| {
            happyview_provider::db::query(&sql)
                .bind(job_id)
                .bind(sweep.sweep)
                .bind(sweep.kind.as_str())
                .bind(&collections)
                .bind(&dids)
                .bind(sweep.after_seq)
                .bind(sweep.cursor_seq)
                .bind(sweep.tip_seq)
                .bind(sweep.last_did_id)
                .execute(&state.backfill_db)
        })
        .await?;
    }
    Ok(())
}

/// Save a sweep's position, and mirror it on the job for the dashboard.
pub async fn save(state: &JobState, job_id: &str, sweep: &Sweep) -> Result<(), sqlx::Error> {
    let backend = state.db_backend;
    let progress = adapt_sql(
        "UPDATE happyview_backfill_replay_sweeps SET cursor_seq = ?, tip_seq = ?, done = ? \
         WHERE job_id = ? AND sweep = ?",
        backend,
    );
    let mirror = adapt_sql(
        "UPDATE happyview_backfill_jobs SET replay_through_seq = ?, replay_tip_seq = ? WHERE id = ?",
        backend,
    );
    happyview_provider::db::retry_on_busy(|| async {
        happyview_provider::db::query(&progress)
            .bind(sweep.cursor_seq)
            .bind(sweep.tip_seq)
            .bind(i32::from(sweep.done))
            .bind(job_id)
            .bind(sweep.sweep)
            .execute(&state.backfill_db)
            .await?;
        happyview_provider::db::query(&mirror)
            .bind(sweep.cursor_seq)
            .bind(sweep.tip_seq)
            .bind(job_id)
            .execute(&state.backfill_db)
            .await
    })
    .await?;
    Ok(())
}

/// Replace the DIDs of a marker sweep that has not planned yet (a retry
/// job adds the DIDs each refetched block wrote).
pub(crate) async fn set_dids(
    state: &JobState,
    job_id: &str,
    sweep: &Sweep,
) -> Result<(), sqlx::Error> {
    let dids = sweep
        .dids
        .as_ref()
        .map(|d| serde_json::to_string(d).expect("strings serialize"));
    happyview_provider::db::retry_on_busy(|| {
        happyview_provider::db::query(&adapt_sql(
            "UPDATE happyview_backfill_replay_sweeps SET dids = ? WHERE job_id = ? AND sweep = ?",
            state.db_backend,
        ))
        .bind(&dids)
        .bind(job_id)
        .bind(sweep.sweep)
        .execute(&state.backfill_db)
    })
    .await?;
    Ok(())
}

/// Forget a job's sweeps (an `auto` job that fell back to crawling).
pub(crate) async fn clear(state: &JobState, job_id: &str) -> Result<(), sqlx::Error> {
    happyview_provider::db::retry_on_busy(|| {
        happyview_provider::db::query(&adapt_sql(
            "DELETE FROM happyview_backfill_replay_sweeps WHERE job_id = ?",
            state.db_backend,
        ))
        .bind(job_id)
        .execute(&state.backfill_db)
    })
    .await?;
    Ok(())
}

pub(crate) async fn set_source(
    state: &JobState,
    job_id: &str,
    source: &str,
) -> Result<(), sqlx::Error> {
    happyview_provider::db::retry_on_busy(|| {
        happyview_provider::db::query(&adapt_sql(
            "UPDATE happyview_backfill_jobs SET source = ? WHERE id = ?",
            state.db_backend,
        ))
        .bind(source)
        .bind(job_id)
        .execute(&state.backfill_db)
    })
    .await?;
    Ok(())
}

/// Add `n` replayed records to the job's `total_records`, which spans every
/// run of the job (a resumed run starts a fresh fold).
pub async fn add_records(state: &JobState, job_id: &str, n: i64) -> Result<(), sqlx::Error> {
    let sql = adapt_sql(
        &format!(
            "UPDATE happyview_backfill_jobs SET {} WHERE id = ?",
            super::saturating_counter("total_records", "?", state.db_backend)
        ),
        state.db_backend,
    );
    happyview_provider::db::retry_on_busy(|| {
        happyview_provider::db::query(&sql)
            .bind(n)
            .bind(job_id)
            .execute(&state.backfill_db)
    })
    .await?;
    Ok(())
}

/// The job's `total_records`, across all its runs.
pub async fn records(state: &JobState, job_id: &str) -> Result<i32, sqlx::Error> {
    let (n,): (Option<i32>,) = happyview_provider::db::query_as(&adapt_sql(
        "SELECT total_records FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    ))
    .bind(job_id)
    .fetch_one(&state.backfill_db)
    .await?;
    Ok(n.unwrap_or(0))
}

/// Add to the job's replay counters; returns the new totals.
pub async fn add_counters(
    state: &JobState,
    job_id: &str,
    bytes: i64,
    blocks_failed: i32,
) -> Result<(i64, i32), sqlx::Error> {
    let backend = state.db_backend;
    let update = adapt_sql(
        "UPDATE happyview_backfill_jobs SET replay_bytes = replay_bytes + ?, \
         replay_blocks_failed = replay_blocks_failed + ? WHERE id = ?",
        backend,
    );
    let read = adapt_sql(
        "SELECT replay_bytes, replay_blocks_failed FROM happyview_backfill_jobs WHERE id = ?",
        backend,
    );
    happyview_provider::db::retry_on_busy(|| async {
        happyview_provider::db::query(&update)
            .bind(bytes)
            .bind(blocks_failed)
            .bind(job_id)
            .execute(&state.backfill_db)
            .await?;
        happyview_provider::db::query_as::<(i64, i32)>(&read)
            .bind(job_id)
            .fetch_one(&state.backfill_db)
            .await
    })
    .await
}

/// The job's replay counters, read without writing.
pub async fn counters(state: &JobState, job_id: &str) -> Result<(i64, i32), sqlx::Error> {
    happyview_provider::db::query_as::<(i64, i32)>(&adapt_sql(
        "SELECT replay_bytes, replay_blocks_failed FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    ))
    .bind(job_id)
    .fetch_one(&state.backfill_db)
    .await
}

/// `replay_after_seq`: where a gap replay starts; 0 for a whole-archive job.
pub(crate) async fn after_seq(state: &JobState, job_id: &str) -> Result<i64, sqlx::Error> {
    let row: Option<(Option<i64>,)> = happyview_provider::db::query_as(&adapt_sql(
        "SELECT replay_after_seq FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    ))
    .bind(job_id)
    .fetch_optional(&state.backfill_db)
    .await?;
    Ok(row.and_then(|(seq,)| seq).unwrap_or(0))
}

/// The DIDs an account job targets (its queue rows).
pub(crate) async fn job_dids(state: &JobState, job_id: &str) -> Result<Vec<String>, sqlx::Error> {
    Ok(happyview_provider::db::query_as::<(String,)>(&adapt_sql(
        "SELECT DISTINCT did FROM happyview_backfill_queue WHERE job_id = ? ORDER BY did",
        state.db_backend,
    ))
    .bind(job_id)
    .fetch_all(&state.backfill_db)
    .await?
    .into_iter()
    .map(|(did,)| did)
    .collect())
}

pub(crate) async fn retries(
    state: &JobState,
    job_id: &str,
) -> Result<Vec<(String, i32)>, sqlx::Error> {
    happyview_provider::db::query_as::<(String, i32)>(&adapt_sql(
        "SELECT segment, block_index FROM happyview_backfill_replay_retries \
         WHERE job_id = ? AND done = 0 ORDER BY segment, block_index",
        state.db_backend,
    ))
    .bind(job_id)
    .fetch_all(&state.backfill_db)
    .await
}

/// Record the blocks a retry job refetches (`block_index` -1: a whole
/// segment), before the job is spawned.
pub(crate) async fn insert_retries(
    state: &JobState,
    job_id: &str,
    blocks: &[(String, i32)],
) -> Result<(), sqlx::Error> {
    let sql = adapt_sql(
        "INSERT INTO happyview_backfill_replay_retries (job_id, segment, block_index, done) VALUES (?, ?, ?, 0)",
        state.db_backend,
    );
    for (segment, block) in blocks {
        happyview_provider::db::retry_on_busy(|| {
            happyview_provider::db::query(&sql)
                .bind(job_id)
                .bind(segment.as_str())
                .bind(*block)
                .execute(&state.backfill_db)
        })
        .await?;
    }
    Ok(())
}

pub(crate) async fn has_retries(state: &JobState, job_id: &str) -> Result<bool, sqlx::Error> {
    let n: i64 = happyview_provider::db::query_scalar(&adapt_sql(
        "SELECT COUNT(*) FROM happyview_backfill_replay_retries WHERE job_id = ?",
        state.db_backend,
    ))
    .bind(job_id)
    .fetch_one(&state.backfill_db)
    .await?;
    Ok(n > 0)
}

pub(crate) async fn retry_done(
    state: &JobState,
    job_id: &str,
    segment: &str,
    block: i32,
) -> Result<(), sqlx::Error> {
    happyview_provider::db::retry_on_busy(|| {
        happyview_provider::db::query(&adapt_sql(
            "UPDATE happyview_backfill_replay_retries SET done = 1 \
             WHERE job_id = ? AND segment = ? AND block_index = ?",
            state.db_backend,
        ))
        .bind(job_id)
        .bind(segment)
        .bind(block)
        .execute(&state.backfill_db)
    })
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("app.test.c{i:03}")).collect()
    }

    #[test]
    fn a_network_job_chunks_collections_and_adds_no_marker_sweeps_yet() {
        let sweeps = initial_sweeps(&names(250), None, 0);
        assert_eq!(sweeps.len(), 3);
        assert_eq!(
            sweeps
                .iter()
                .map(|s| s.collections.len())
                .collect::<Vec<_>>(),
            vec![100, 100, 50]
        );
        assert!(
            sweeps
                .iter()
                .all(|s| s.kind == SweepKind::Commits && s.dids.is_none())
        );
        assert_eq!(
            sweeps.iter().map(|s| s.sweep).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn an_account_job_sweeps_its_dids_for_markers() {
        let dids: Vec<String> = (0..3).map(|i| format!("did:plc:{i}")).collect();
        let sweeps = initial_sweeps(&names(2), Some(&dids), 77);
        assert_eq!(sweeps.len(), 2);
        assert_eq!(sweeps[0].kind, SweepKind::Commits);
        assert_eq!(sweeps[0].dids.as_deref(), Some(dids.as_slice()));
        assert_eq!(sweeps[1].kind, SweepKind::Markers);
        assert!(sweeps[1].collections.is_empty());
        assert!(
            sweeps
                .iter()
                .all(|s| s.after_seq == 77 && s.cursor_seq == 77)
        );
    }
}
