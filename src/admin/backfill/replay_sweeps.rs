//! Rows that make a replay job resumable: one per planSnapshot sweep
//! (`happyview_backfill_replay_sweeps`), and the blocks a retry job refetches
//! (`happyview_backfill_replay_retries`). Every write here is load-bearing for
//! resume, so errors propagate instead of being logged and dropped.

use crate::AppState;
use crate::db::adapt_sql;

pub(super) const MAX_PLAN_COLLECTIONS: usize = 100;
pub(super) const MAX_PLAN_DIDS: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SweepKind {
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
pub(super) struct Sweep {
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
pub(super) fn initial_sweeps(
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

pub(super) async fn load(state: &AppState, job_id: &str) -> Result<Vec<Sweep>, sqlx::Error> {
    let rows: Vec<SweepRow> = crate::db::query_as(&adapt_sql(
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

pub(super) async fn insert(
    state: &AppState,
    job_id: &str,
    sweeps: &[Sweep],
) -> Result<(), sqlx::Error> {
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
        crate::db::retry_on_busy(|| {
            crate::db::query(&sql)
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
pub(super) async fn save(state: &AppState, job_id: &str, sweep: &Sweep) -> Result<(), sqlx::Error> {
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
    crate::db::retry_on_busy(|| async {
        crate::db::query(&progress)
            .bind(sweep.cursor_seq)
            .bind(sweep.tip_seq)
            .bind(i32::from(sweep.done))
            .bind(job_id)
            .bind(sweep.sweep)
            .execute(&state.backfill_db)
            .await?;
        crate::db::query(&mirror)
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
pub(super) async fn set_dids(
    state: &AppState,
    job_id: &str,
    sweep: &Sweep,
) -> Result<(), sqlx::Error> {
    let dids = sweep
        .dids
        .as_ref()
        .map(|d| serde_json::to_string(d).expect("strings serialize"));
    crate::db::retry_on_busy(|| {
        crate::db::query(&adapt_sql(
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
pub(super) async fn clear(state: &AppState, job_id: &str) -> Result<(), sqlx::Error> {
    crate::db::retry_on_busy(|| {
        crate::db::query(&adapt_sql(
            "DELETE FROM happyview_backfill_replay_sweeps WHERE job_id = ?",
            state.db_backend,
        ))
        .bind(job_id)
        .execute(&state.backfill_db)
    })
    .await?;
    Ok(())
}

pub(super) async fn set_source(
    state: &AppState,
    job_id: &str,
    source: &str,
) -> Result<(), sqlx::Error> {
    crate::db::retry_on_busy(|| {
        crate::db::query(&adapt_sql(
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
pub(super) async fn add_records(state: &AppState, job_id: &str, n: i64) -> Result<(), sqlx::Error> {
    let sql = adapt_sql(
        &format!(
            "UPDATE happyview_backfill_jobs SET {} WHERE id = ?",
            happyview_source_jetstream::backfill::saturating_counter(
                "total_records",
                "?",
                state.db_backend
            )
        ),
        state.db_backend,
    );
    crate::db::retry_on_busy(|| {
        crate::db::query(&sql)
            .bind(n)
            .bind(job_id)
            .execute(&state.backfill_db)
    })
    .await?;
    Ok(())
}

/// The job's `total_records`, across all its runs.
pub(super) async fn records(state: &AppState, job_id: &str) -> Result<i32, sqlx::Error> {
    let (n,): (Option<i32>,) = crate::db::query_as(&adapt_sql(
        "SELECT total_records FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    ))
    .bind(job_id)
    .fetch_one(&state.backfill_db)
    .await?;
    Ok(n.unwrap_or(0))
}

/// Add to the job's replay counters; returns the new totals.
pub(super) async fn add_counters(
    state: &AppState,
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
    crate::db::retry_on_busy(|| async {
        crate::db::query(&update)
            .bind(bytes)
            .bind(blocks_failed)
            .bind(job_id)
            .execute(&state.backfill_db)
            .await?;
        crate::db::query_as::<(i64, i32)>(&read)
            .bind(job_id)
            .fetch_one(&state.backfill_db)
            .await
    })
    .await
}

/// The job's replay counters, read without writing.
pub(super) async fn counters(state: &AppState, job_id: &str) -> Result<(i64, i32), sqlx::Error> {
    crate::db::query_as::<(i64, i32)>(&adapt_sql(
        "SELECT replay_bytes, replay_blocks_failed FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    ))
    .bind(job_id)
    .fetch_one(&state.backfill_db)
    .await
}

/// `replay_after_seq`: where a gap replay starts; 0 for a whole-archive job.
pub(super) async fn after_seq(state: &AppState, job_id: &str) -> Result<i64, sqlx::Error> {
    let row: Option<(Option<i64>,)> = crate::db::query_as(&adapt_sql(
        "SELECT replay_after_seq FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    ))
    .bind(job_id)
    .fetch_optional(&state.backfill_db)
    .await?;
    Ok(row.and_then(|(seq,)| seq).unwrap_or(0))
}

/// The DIDs an account job targets (its queue rows).
pub(super) async fn job_dids(state: &AppState, job_id: &str) -> Result<Vec<String>, sqlx::Error> {
    Ok(crate::db::query_as::<(String,)>(&adapt_sql(
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

/// Up to `limit` DIDs, by `happyview_dids.id` after `after_id`, that hold
/// network records in `collections`: whose account markers a network job
/// replays.
pub(super) async fn next_held_dids(
    state: &AppState,
    collections: &[String],
    after_id: i64,
    limit: i64,
) -> Result<Vec<(i64, String)>, sqlx::Error> {
    let placeholders = vec!["?"; collections.len()].join(", ");
    let sql = adapt_sql(
        &format!(
            "SELECT d.id, d.did FROM happyview_dids d WHERE d.id > ? AND EXISTS ( \
               SELECT 1 FROM happyview_records_v2 r \
               WHERE r.did_id = d.id AND r.indexed_at IS NOT NULL AND r.collection_id IN ( \
                 SELECT id FROM happyview_collections WHERE nsid IN ({placeholders}))) \
             ORDER BY d.id LIMIT ?"
        ),
        state.db_backend,
    );
    let mut query = crate::db::query_as::<(i64, String)>(&sql).bind(after_id);
    for collection in collections {
        query = query.bind(collection.as_str());
    }
    query.bind(limit).fetch_all(&state.backfill_db).await
}

pub(super) async fn retries(
    state: &AppState,
    job_id: &str,
) -> Result<Vec<(String, i32)>, sqlx::Error> {
    crate::db::query_as::<(String, i32)>(&adapt_sql(
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
pub(super) async fn insert_retries(
    state: &AppState,
    job_id: &str,
    blocks: &[(String, i32)],
) -> Result<(), sqlx::Error> {
    let sql = adapt_sql(
        "INSERT INTO happyview_backfill_replay_retries (job_id, segment, block_index, done) VALUES (?, ?, ?, 0)",
        state.db_backend,
    );
    for (segment, block) in blocks {
        crate::db::retry_on_busy(|| {
            crate::db::query(&sql)
                .bind(job_id)
                .bind(segment.as_str())
                .bind(*block)
                .execute(&state.backfill_db)
        })
        .await?;
    }
    Ok(())
}

pub(super) async fn has_retries(state: &AppState, job_id: &str) -> Result<bool, sqlx::Error> {
    let n: i64 = crate::db::query_scalar(&adapt_sql(
        "SELECT COUNT(*) FROM happyview_backfill_replay_retries WHERE job_id = ?",
        state.db_backend,
    ))
    .bind(job_id)
    .fetch_one(&state.backfill_db)
    .await?;
    Ok(n > 0)
}

pub(super) async fn retry_done(
    state: &AppState,
    job_id: &str,
    segment: &str,
    block: i32,
) -> Result<(), sqlx::Error> {
    crate::db::retry_on_busy(|| {
        crate::db::query(&adapt_sql(
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

    #[tokio::test]
    async fn sweeps_round_trip_on_both_backends() {
        let mut states = vec![crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        )];
        if let Some(state) = crate::test_support::test_state_from_env().await {
            states.push(state);
        }
        for state in states {
            let job = format!("sweeps-{}", uuid::Uuid::new_v4());
            crate::db::query(&adapt_sql(
                "INSERT INTO happyview_backfill_jobs (id, scope, status, stage, created_at) \
                 VALUES (?, 'network', 'running', 'pending', '2026-10-12T00:00:00+00:00')",
                state.db_backend,
            ))
            .bind(&job)
            .execute(&state.db)
            .await
            .unwrap();
            let mut sweeps = initial_sweeps(&names(2), Some(&["did:plc:x".to_string()]), 5);
            insert(&state, &job, &sweeps).await.unwrap();
            sweeps[0].cursor_seq = 40_000_000_000;
            sweeps[0].tip_seq = Some(50_000_000_000);
            sweeps[0].done = true;
            save(&state, &job, &sweeps[0]).await.unwrap();
            assert_eq!(load(&state, &job).await.unwrap(), sweeps);
            assert_eq!(
                add_counters(&state, &job, 1 << 40, 2).await.unwrap(),
                (1 << 40, 2)
            );
            assert_eq!(
                add_counters(&state, &job, 10, 1).await.unwrap(),
                ((1 << 40) + 10, 3)
            );
            assert_eq!(
                counters(&state, &job).await.unwrap(),
                ((1 << 40) + 10, 3),
                "a read changes nothing"
            );
            add_records(&state, &job, 4).await.unwrap();
            add_records(&state, &job, 4).await.unwrap();
            assert_eq!(records(&state, &job).await.unwrap(), 8);
        }
    }

    #[tokio::test]
    async fn an_unreadable_sweep_row_is_an_error_not_an_empty_list() {
        let mut states = vec![crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        )];
        if let Some(state) = crate::test_support::test_state_from_env().await {
            states.push(state);
        }
        let cases = [
            ("commits", "not json", None),
            ("commits", "[]", None),
            ("markers", "[]", Some("{oops")),
            ("markers", "[]", None),
            ("bogus", "[\"app.test.c\"]", None),
        ];
        for state in states {
            for (kind, collections, dids) in cases {
                let job = format!("corrupt-{}", uuid::Uuid::new_v4());
                crate::db::query(&adapt_sql(
                    "INSERT INTO happyview_backfill_jobs (id, scope, status, stage, created_at) \
                     VALUES (?, 'network', 'running', 'pending', '2026-10-12T00:00:00+00:00')",
                    state.db_backend,
                ))
                .bind(&job)
                .execute(&state.db)
                .await
                .unwrap();
                crate::db::query(&adapt_sql(
                    "INSERT INTO happyview_backfill_replay_sweeps \
                     (job_id, sweep, kind, collections, dids, after_seq, cursor_seq) \
                     VALUES (?, 0, ?, ?, ?, 0, 0)",
                    state.db_backend,
                ))
                .bind(&job)
                .bind(kind)
                .bind(collections)
                .bind(dids)
                .execute(&state.db)
                .await
                .unwrap();
                let error = load(&state, &job).await.expect_err(kind).to_string();
                assert!(error.contains("replay sweep 0"), "{error}");
            }
        }
    }
}
