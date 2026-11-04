use std::time::Duration;

use serde_json::Value;
use uuid::Uuid;

use crate::AppState;
use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::error::AppError;

use super::Job;

const JOB_COLUMNS: &str = "id, job_type, status, input, progress, result, error, created_by, started_at, completed_at, created_at, CAST(inherit_auth AS INTEGER), api_client_id, dpop_key_id, CAST(attempt AS BIGINT)";

/// How long a claim holds a job without being renewed. A worker renews it
/// every [`RENEW_EVERY`] for as long as the job runs, so only a worker that
/// stopped (its process died, or it hangs) lets it run out.
pub const LEASE: Duration = Duration::from_secs(60);
/// How often a running job's worker renews its lease.
pub const RENEW_EVERY: Duration = Duration::from_secs(20);
/// How many times a job may lose its worker before it is failed rather than
/// queued again: a job that takes its process down with it would otherwise
/// do so forever. Counted in `lost_leases`, not `attempt`: a job paused and
/// resumed is claimed again without having lost anything.
pub const MAX_LOST_LEASES: i64 = 3;

/// The claim a worker holds on a job: who it is, and which run of the job
/// this is. Writes a worker makes about a job's outcome are fenced on both,
/// so a worker whose lease ran out, and whose job was queued again or
/// claimed by another worker, changes nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub owner: String,
    pub attempt: i64,
}

/// Milliseconds since the epoch: the unit of `lease_until`. Not an RFC 3339
/// string, whose fractional seconds vary in width and so do not sort.
fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn lease_until() -> i64 {
    now_millis() + LEASE.as_millis() as i64
}

type JobRow = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    String,
    i64,
    Option<String>,
    Option<String>,
    i64,
);

fn row_to_job(
    (
        id,
        job_type,
        status,
        input,
        progress,
        result,
        error,
        created_by,
        started_at,
        completed_at,
        created_at,
        inherit_auth,
        api_client_id,
        dpop_key_id,
        attempt,
    ): JobRow,
) -> Job {
    Job {
        id,
        job_type,
        status,
        input: serde_json::from_str(&input).unwrap_or(Value::Null),
        progress: serde_json::from_str(&progress).unwrap_or(Value::Null),
        result: result.and_then(|r| serde_json::from_str(&r).ok()),
        error,
        created_by,
        started_at,
        completed_at,
        created_at,
        inherit_auth: inherit_auth != 0,
        api_client_id,
        dpop_key_id,
        attempt,
    }
}

pub async fn create_job(
    state: &AppState,
    job_type: &str,
    input: &Value,
    created_by: &str,
    inherit_auth: bool,
    api_client_id: Option<&str>,
    dpop_key_id: Option<&str>,
) -> Result<String, AppError> {
    let id = Uuid::new_v4().to_string();
    let now = now_rfc3339();
    let input_str = serde_json::to_string(input)
        .map_err(|e| AppError::Internal(format!("failed to serialize job input: {e}")))?;

    let sql = adapt_sql(
        "INSERT INTO happyview_jobs (id, job_type, status, input, created_by, created_at, inherit_auth, api_client_id, dpop_key_id) VALUES (?, ?, 'pending', ?, ?, ?, ?, ?, ?)",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(&id)
        .bind(job_type)
        .bind(&input_str)
        .bind(created_by)
        .bind(&now)
        .bind(inherit_auth)
        .bind(api_client_id)
        .bind(dpop_key_id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create job: {e}")))?;

    Ok(id)
}

pub async fn get_job(state: &AppState, id: &str) -> Result<Option<Job>, AppError> {
    let sql = adapt_sql(
        &format!("SELECT {JOB_COLUMNS} FROM happyview_jobs WHERE id = ?"),
        state.db_backend,
    );
    let row: Option<JobRow> = crate::db::query_as(&sql)
        .bind(id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to fetch job: {e}")))?;

    Ok(row.map(row_to_job))
}

/// Jobs across every user, newest first. Every value is a bound parameter:
/// the `IN` list's placeholders are built from `statuses.len()`, never from
/// the values themselves.
pub async fn list_jobs_any(
    state: &AppState,
    statuses: &[String],
    job_type: Option<&str>,
    limit: u32,
) -> Result<Vec<Job>, AppError> {
    let mut sql = format!("SELECT {JOB_COLUMNS} FROM happyview_jobs WHERE 1 = 1");
    if !statuses.is_empty() {
        let placeholders = vec!["?"; statuses.len()].join(", ");
        sql.push_str(&format!(" AND status IN ({placeholders})"));
    }
    if job_type.is_some() {
        sql.push_str(" AND job_type = ?");
    }
    sql.push_str(" ORDER BY created_at DESC, id DESC LIMIT ?");

    let sql = adapt_sql(&sql, state.db_backend);
    let mut query = crate::db::query_as::<JobRow>(&sql);
    for status in statuses {
        query = query.bind(status.as_str());
    }
    if let Some(job_type) = job_type {
        query = query.bind(job_type);
    }
    let rows: Vec<JobRow> = query
        .bind(i64::from(limit))
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list jobs: {e}")))?;

    Ok(rows.into_iter().map(row_to_job).collect())
}

pub async fn list_jobs(
    state: &AppState,
    status_filter: Option<&str>,
    limit: i64,
    cursor: Option<&str>,
) -> Result<(Vec<Job>, Option<String>), AppError> {
    let sql = if status_filter.is_some() {
        let base = if cursor.is_some() {
            format!(
                "SELECT {JOB_COLUMNS} FROM happyview_jobs WHERE status = ? AND created_at < ? ORDER BY created_at DESC LIMIT ?"
            )
        } else {
            format!(
                "SELECT {JOB_COLUMNS} FROM happyview_jobs WHERE status = ? ORDER BY created_at DESC LIMIT ?"
            )
        };
        adapt_sql(&base, state.db_backend)
    } else {
        let base = if cursor.is_some() {
            format!(
                "SELECT {JOB_COLUMNS} FROM happyview_jobs WHERE created_at < ? ORDER BY created_at DESC LIMIT ?"
            )
        } else {
            format!("SELECT {JOB_COLUMNS} FROM happyview_jobs ORDER BY created_at DESC LIMIT ?")
        };
        adapt_sql(&base, state.db_backend)
    };

    let mut query = crate::db::query_as::<JobRow>(&sql);

    if let Some(status) = status_filter {
        query = query.bind(status);
    }
    if let Some(cursor) = cursor {
        query = query.bind(cursor);
    }
    query = query.bind(limit + 1);

    let rows = query
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list jobs: {e}")))?;

    let has_more = rows.len() as i64 > limit;
    let jobs: Vec<Job> = rows
        .into_iter()
        .take(limit as usize)
        .map(row_to_job)
        .collect();

    let next_cursor = if has_more {
        jobs.last().map(|j| j.created_at.clone())
    } else {
        None
    };

    Ok((jobs, next_cursor))
}

/// Store a running job's progress, while its run still holds the job's
/// lease: a run whose lease was lost, and whose job was queued again or
/// claimed elsewhere (or by another worker here), writes nothing, and the
/// loss is recorded so the run stops. Outside a job's run there is no lease
/// to write under, and nothing is written.
pub async fn update_progress(state: &AppState, id: &str, progress: &Value) -> Result<(), AppError> {
    let progress_str = serde_json::to_string(progress)
        .map_err(|e| AppError::Internal(format!("failed to serialize progress: {e}")))?;
    let Some(held) = super::held::current(id) else {
        tracing::warn!(job_id = %id, "job progress written outside the job's run; ignored");
        return Ok(());
    };
    if held.is_lost() {
        return Ok(());
    }
    let sql = adapt_sql(
        "UPDATE happyview_jobs SET progress = ? \
          WHERE id = ? AND claimed_by = ? AND attempt = ? \
            AND status IN ('running', 'cancelling', 'pausing')",
        state.db_backend,
    );
    let written = crate::db::query(&sql)
        .bind(&progress_str)
        .bind(id)
        .bind(&held.lease.owner)
        .bind(held.lease.attempt)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to update job progress: {e}")))?;
    if written.rows_affected() == 0 {
        held.mark_lost();
    }
    Ok(())
}

/// Whether a running job should stop: it was asked to pause or cancel, or
/// (asked from the job's run) the run's lease was lost to another claim,
/// which answers `"lost"`. Same cooperative pattern as the backfill system.
pub async fn should_stop(state: &AppState, id: &str) -> Option<&'static str> {
    let held = super::held::current(id);
    if held.as_ref().is_some_and(super::held::Held::is_lost) {
        return Some("lost");
    }
    let sql = adapt_sql(
        "SELECT status, claimed_by, CAST(attempt AS BIGINT) FROM happyview_jobs WHERE id = ?",
        state.db_backend,
    );
    let row = crate::db::query_as::<(String, Option<String>, i64)>(&sql)
        .bind(id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    if let (Some(held), Some((_, owner, attempt))) = (&held, &row)
        && (owner.as_deref() != Some(held.lease.owner.as_str()) || *attempt != held.lease.attempt)
    {
        held.mark_lost();
        return Some("lost");
    }
    match row.as_ref().map(|(status, ..)| status.as_str()) {
        Some("cancelling") => Some("cancelling"),
        Some("pausing") => Some("pausing"),
        _ => None,
    }
}

/// Move job `id` from status `from` to `to`, only if it is still in `from`:
/// for an operator's request read against one status and written later,
/// which must not land on a job that moved on meanwhile (a pause landing on
/// a just-completed job would have recovery settle it as paused, and a
/// resume run it again). `false` when the job had moved on.
pub async fn change_status(
    state: &AppState,
    id: &str,
    from: &str,
    to: &str,
) -> Result<bool, AppError> {
    let completed = if matches!(to, "completed" | "failed" | "cancelled") {
        ", completed_at = ?"
    } else {
        ""
    };
    let sql = adapt_sql(
        &format!("UPDATE happyview_jobs SET status = ?{completed} WHERE id = ? AND status = ?"),
        state.db_backend,
    );
    let mut query = crate::db::query(&sql).bind(to);
    if !completed.is_empty() {
        query = query.bind(now_rfc3339());
    }
    let changed = query
        .bind(id)
        .bind(from)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to update job status: {e}")))?;
    Ok(changed.rows_affected() > 0)
}

/// Settle every job whose worker's lease ran out: its worker stopped, by a
/// restart, a crash or a hang. A `cancelling` or `pausing` job is finalised
/// as its request asked. A `running` one is queued again, unless it has now
/// lost its worker [`MAX_LOST_LEASES`] times, when it fails.
///
/// A job with a live lease is left alone whoever holds it, so an instance
/// starting up beside another no longer queues the other's running jobs
/// again. A row with no lease at all was claimed before leases existed, and
/// reads as expired. Returns how many jobs it settled.
pub async fn recover_expired_jobs(state: &AppState) -> Result<u64, AppError> {
    let now = now_millis();
    let at = now_rfc3339();
    let expired = "(lease_until IS NULL OR lease_until < ?)";
    let failure = format!(
        "the job's worker stopped {MAX_LOST_LEASES} times before it finished; it is not run again"
    );
    let mut settled = 0;
    let statements: [(String, Vec<Bind>); 4] = [
        (
            format!(
                "UPDATE happyview_jobs SET status = 'cancelled', completed_at = ?, claimed_by = NULL, lease_until = NULL \
                  WHERE status = 'cancelling' AND {expired}"
            ),
            vec![Bind::Text(at.clone()), Bind::Int(now)],
        ),
        (
            format!(
                "UPDATE happyview_jobs SET status = 'paused', claimed_by = NULL, lease_until = NULL \
                  WHERE status = 'pausing' AND {expired}"
            ),
            vec![Bind::Int(now)],
        ),
        (
            format!(
                "UPDATE happyview_jobs SET status = 'failed', error = ?, completed_at = ?, \
                        lost_leases = lost_leases + 1, claimed_by = NULL, lease_until = NULL \
                  WHERE status = 'running' AND {expired} AND lost_leases + 1 >= ?"
            ),
            vec![
                Bind::Text(failure),
                Bind::Text(at.clone()),
                Bind::Int(now),
                Bind::Int(MAX_LOST_LEASES),
            ],
        ),
        (
            format!(
                "UPDATE happyview_jobs SET status = 'pending', lost_leases = lost_leases + 1, \
                        claimed_by = NULL, lease_until = NULL \
                  WHERE status = 'running' AND {expired}"
            ),
            vec![Bind::Int(now)],
        ),
    ];
    for (sql, binds) in statements {
        let sql = adapt_sql(&sql, state.db_backend);
        let mut query = crate::db::query(&sql);
        for bind in binds {
            query = match bind {
                Bind::Text(text) => query.bind(text),
                Bind::Int(int) => query.bind(int),
            };
        }
        settled += query
            .execute(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("failed to recover jobs: {e}")))?
            .rows_affected();
    }
    Ok(settled)
}

enum Bind {
    Text(String),
    Int(i64),
}

/// Pick the next pending job and atomically set it to running, held by
/// `owner` under a fresh lease. The job's `attempt` counts this run.
pub async fn claim_next_job(state: &AppState, owner: &str) -> Result<Option<Job>, AppError> {
    let now = now_rfc3339();
    let set = "status = 'running', started_at = ?, attempt = attempt + 1, claimed_by = ?, lease_until = ?";

    let sql = match state.db_backend {
        DatabaseBackend::Postgres => adapt_sql(
            &format!(
                "UPDATE happyview_jobs SET {set} WHERE id = (SELECT id FROM happyview_jobs WHERE status = 'pending' ORDER BY created_at ASC LIMIT 1 FOR UPDATE SKIP LOCKED) RETURNING {JOB_COLUMNS}"
            ),
            state.db_backend,
        ),
        DatabaseBackend::Sqlite => adapt_sql(
            &format!(
                "UPDATE happyview_jobs SET {set} WHERE id = (SELECT id FROM happyview_jobs WHERE status = 'pending' ORDER BY created_at ASC LIMIT 1) AND status = 'pending' RETURNING {JOB_COLUMNS}"
            ),
            state.db_backend,
        ),
    };

    let row: Option<JobRow> = crate::db::query_as(&sql)
        .bind(&now)
        .bind(owner)
        .bind(lease_until())
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to claim job: {e}")))?;

    Ok(row.map(row_to_job))
}

/// Claim pending job `id` itself, as [`claim_next_job`] claims the next one:
/// for a job run where it was created rather than by a worker.
pub async fn hold_job(state: &AppState, id: &str, owner: &str) -> Result<Option<Job>, AppError> {
    let sql = adapt_sql(
        &format!(
            "UPDATE happyview_jobs SET status = 'running', started_at = ?, attempt = attempt + 1, claimed_by = ?, lease_until = ? \
              WHERE id = ? AND status = 'pending' RETURNING {JOB_COLUMNS}"
        ),
        state.db_backend,
    );
    let row: Option<JobRow> = crate::db::query_as(&sql)
        .bind(now_rfc3339())
        .bind(owner)
        .bind(lease_until())
        .bind(id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to hold job: {e}")))?;
    Ok(row.map(row_to_job))
}

/// Extend `lease` on job `id`. `false` when it is no longer held: the job was
/// recovered, or claimed again under another lease, after this one ran out.
pub async fn renew_lease(state: &AppState, id: &str, lease: &Lease) -> Result<bool, AppError> {
    let sql = adapt_sql(
        "UPDATE happyview_jobs SET lease_until = ? \
          WHERE id = ? AND claimed_by = ? AND attempt = ? \
            AND status IN ('running', 'cancelling', 'pausing')",
        state.db_backend,
    );
    let renewed = crate::db::query(&sql)
        .bind(lease_until())
        .bind(id)
        .bind(&lease.owner)
        .bind(lease.attempt)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to renew job lease: {e}")))?;
    Ok(renewed.rows_affected() > 0)
}

/// How a worker leaves a job.
#[derive(Clone, Copy)]
pub enum Finish<'a> {
    Completed(&'a Value),
    Failed(&'a str),
    Paused,
    Cancelled,
}

/// Record how job `id` ended and release its lease, only while `lease` is
/// still this worker's and the job still running: otherwise it changes
/// nothing, and says so with `false`.
pub async fn finish(
    state: &AppState,
    id: &str,
    lease: &Lease,
    finish: Finish<'_>,
) -> Result<bool, AppError> {
    let now = now_rfc3339();
    let (set, value): (&str, Option<String>) = match finish {
        Finish::Completed(result) => (
            "status = 'completed', result = ?, completed_at = ?",
            Some(
                serde_json::to_string(result)
                    .map_err(|e| AppError::Internal(format!("failed to serialize result: {e}")))?,
            ),
        ),
        Finish::Failed(error) => (
            "status = 'failed', error = ?, completed_at = ?",
            Some(error.to_string()),
        ),
        Finish::Cancelled => ("status = 'cancelled', completed_at = ?", None),
        Finish::Paused => ("status = 'paused'", None),
    };
    let sql = adapt_sql(
        &format!(
            "UPDATE happyview_jobs SET {set}, lease_until = NULL \
              WHERE id = ? AND claimed_by = ? AND attempt = ? \
                AND status IN ('running', 'cancelling', 'pausing')"
        ),
        state.db_backend,
    );
    let mut query = crate::db::query(&sql);
    if let Some(value) = value {
        query = query.bind(value);
    }
    if !matches!(finish, Finish::Paused) {
        query = query.bind(now);
    }
    query = query.bind(id).bind(lease.owner.clone()).bind(lease.attempt);
    let finished = query
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to finish job: {e}")))?;
    Ok(finished.rows_affected() > 0)
}

#[cfg(test)]
mod lease_tests {
    use super::*;
    use crate::test_support::{
        PreUpgradePool, pre_upgrade_pools, test_pools, test_state_with_pool_on,
    };

    /// The databases one test runs against, its own: an in-memory SQLite
    /// database, and a scratch database on the `TEST_DATABASE_URL` server when
    /// that is Postgres. Never the shared test database: `expire` makes a job
    /// any recovery's to take, and `recover_expired_jobs` sweeps every
    /// expired job in the database it runs on, so another test's recovery
    /// running between this test's `expire` and its next step would requeue
    /// this test's job.
    struct Databases(Vec<PreUpgradePool>);

    impl Databases {
        async fn open() -> Self {
            Self(pre_upgrade_pools().await)
        }

        /// Two instances on each database: two states over the same pool,
        /// which claim as two owners. Only the database coordinates them, as
        /// in production.
        fn instances(&self) -> Vec<(AppState, AppState)> {
            self.0
                .iter()
                .map(|db| {
                    (
                        test_state_with_pool_on(db.pool.clone(), db.backend),
                        test_state_with_pool_on(db.pool.clone(), db.backend),
                    )
                })
                .collect()
        }

        async fn finish(self) {
            for db in self.0 {
                db.finish().await;
            }
        }
    }

    async fn new_job(state: &AppState) -> String {
        create_job(
            state,
            "test.lease",
            &serde_json::json!({}),
            "did:plc:lease",
            false,
            None,
            None,
        )
        .await
        .unwrap()
    }

    async fn job(state: &AppState, id: &str) -> Job {
        get_job(state, id).await.unwrap().expect("the job")
    }

    async fn owner_of(state: &AppState, id: &str) -> Option<String> {
        crate::db::query_as::<(Option<String>,)>(&adapt_sql(
            "SELECT claimed_by FROM happyview_jobs WHERE id = ?",
            state.db_backend,
        ))
        .bind(id)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .0
    }

    /// What a lease reads as once its worker has stopped renewing it.
    async fn expire(state: &AppState, id: &str) {
        crate::db::query(&adapt_sql(
            "UPDATE happyview_jobs SET lease_until = 0 WHERE id = ?",
            state.db_backend,
        ))
        .bind(id)
        .execute(&state.db)
        .await
        .unwrap();
    }

    fn lease(owner: &str, job: &Job) -> Lease {
        Lease {
            owner: owner.to_string(),
            attempt: job.attempt,
        }
    }

    /// The bug leases fix: an instance starting up recovered every running
    /// job, another instance's live ones included, and they ran twice.
    #[tokio::test]
    async fn another_instances_recovery_leaves_a_live_lease_alone() {
        let dbs = Databases::open().await;
        for (a, b) in dbs.instances() {
            let id = new_job(&a).await;
            hold_job(&a, &id, "a").await.unwrap().expect("a holds it");

            recover_expired_jobs(&b).await.unwrap();

            let held = job(&a, &id).await;
            assert_eq!(held.status, "running", "{:?}", a.db_backend);
            assert_eq!(owner_of(&a, &id).await.as_deref(), Some("a"));
        }
        dbs.finish().await;
    }

    #[tokio::test]
    async fn only_one_instance_holds_a_job() {
        let dbs = Databases::open().await;
        for (a, b) in dbs.instances() {
            let id = new_job(&a).await;
            assert!(hold_job(&a, &id, "a").await.unwrap().is_some());
            assert!(
                hold_job(&b, &id, "b").await.unwrap().is_none(),
                "{:?}",
                a.db_backend
            );
        }
        dbs.finish().await;
    }

    #[tokio::test]
    async fn an_expired_lease_is_queued_again_and_counts_as_an_attempt() {
        let dbs = Databases::open().await;
        for (a, b) in dbs.instances() {
            let id = new_job(&a).await;
            let first = hold_job(&a, &id, "a").await.unwrap().unwrap();
            assert_eq!(first.attempt, 1);
            expire(&a, &id).await;

            recover_expired_jobs(&b).await.unwrap();
            let queued = job(&b, &id).await;
            assert_eq!(queued.status, "pending", "{:?}", a.db_backend);
            assert_eq!(owner_of(&b, &id).await, None);

            let second = hold_job(&b, &id, "b").await.unwrap().unwrap();
            assert_eq!(second.attempt, 2);
        }
        dbs.finish().await;
    }

    /// The worker that lost its lease may still be running the job. Its
    /// outcome must not land over the run that took the job over.
    #[tokio::test]
    async fn a_worker_that_lost_its_lease_cannot_finish_the_job() {
        let dbs = Databases::open().await;
        for (a, b) in dbs.instances() {
            let id = new_job(&a).await;
            let first = hold_job(&a, &id, "a").await.unwrap().unwrap();
            expire(&a, &id).await;
            recover_expired_jobs(&b).await.unwrap();
            let second = hold_job(&b, &id, "b").await.unwrap().unwrap();

            let stale = serde_json::json!({ "from": "a" });
            let fresh = serde_json::json!({ "from": "b" });
            assert!(
                !finish(&a, &id, &lease("a", &first), Finish::Completed(&stale))
                    .await
                    .unwrap()
            );
            assert!(!renew_lease(&a, &id, &lease("a", &first)).await.unwrap());
            assert_eq!(job(&a, &id).await.status, "running");

            assert!(
                finish(&b, &id, &lease("b", &second), Finish::Completed(&fresh))
                    .await
                    .unwrap()
            );
            let done = job(&b, &id).await;
            assert_eq!(done.status, "completed", "{:?}", a.db_backend);
            assert_eq!(done.result, Some(fresh));
        }
        dbs.finish().await;
    }

    /// A job that takes its worker down with it fails after a few tries
    /// rather than doing so forever.
    #[tokio::test]
    async fn a_job_that_keeps_losing_its_worker_fails() {
        let dbs = Databases::open().await;
        for (a, _) in dbs.instances() {
            let id = new_job(&a).await;
            for attempt in 1..=MAX_LOST_LEASES {
                let held = hold_job(&a, &id, "a")
                    .await
                    .unwrap()
                    .expect("pending again");
                assert_eq!(held.attempt, attempt);
                expire(&a, &id).await;
                recover_expired_jobs(&a).await.unwrap();
            }
            let failed = job(&a, &id).await;
            assert_eq!(failed.status, "failed", "{:?}", a.db_backend);
            assert!(
                failed
                    .error
                    .as_deref()
                    .unwrap_or_default()
                    .contains("worker stopped"),
                "{:?}",
                failed.error
            );
        }
        dbs.finish().await;
    }

    #[tokio::test]
    async fn a_held_lease_renews() {
        let dbs = Databases::open().await;
        for (a, _) in dbs.instances() {
            let id = new_job(&a).await;
            let held = hold_job(&a, &id, "a").await.unwrap().unwrap();
            expire(&a, &id).await;
            assert!(renew_lease(&a, &id, &lease("a", &held)).await.unwrap());
            recover_expired_jobs(&a).await.unwrap();
            assert_eq!(job(&a, &id).await.status, "running", "renewed in time");
        }
        dbs.finish().await;
    }

    /// A row claimed before leases existed has none, and its worker went with
    /// the upgrade that added them.
    #[tokio::test]
    async fn a_running_job_from_before_leases_is_queued_again() {
        let dbs = Databases::open().await;
        for (a, _) in dbs.instances() {
            let id = new_job(&a).await;
            crate::db::query(&adapt_sql(
                "UPDATE happyview_jobs SET status = 'running' WHERE id = ?",
                a.db_backend,
            ))
            .bind(&id)
            .execute(&a.db)
            .await
            .unwrap();
            recover_expired_jobs(&a).await.unwrap();
            assert_eq!(job(&a, &id).await.status, "pending", "{:?}", a.db_backend);
        }
        dbs.finish().await;
    }

    #[tokio::test]
    async fn a_stop_request_whose_worker_stopped_is_settled() {
        let dbs = Databases::open().await;
        for (a, _) in dbs.instances() {
            for (asked, settled) in [("cancelling", "cancelled"), ("pausing", "paused")] {
                let id = new_job(&a).await;
                hold_job(&a, &id, "a").await.unwrap().unwrap();
                assert!(change_status(&a, &id, "running", asked).await.unwrap());
                expire(&a, &id).await;
                recover_expired_jobs(&a).await.unwrap();
                assert_eq!(job(&a, &id).await.status, settled, "{:?}", a.db_backend);
            }
        }
        dbs.finish().await;
    }

    async fn progress_of(state: &AppState, id: &str) -> Value {
        job(state, id).await.progress
    }

    /// The other instance took the job over: `a`'s run is stale.
    async fn taken_over(a: &AppState, b: &AppState, id: &str) -> Job {
        expire(a, id).await;
        recover_expired_jobs(b).await.unwrap();
        hold_job(b, id, "b")
            .await
            .unwrap()
            .expect("b takes the job")
    }

    use super::super::held::{Held, run_under};

    /// A stale run's progress lands nowhere, and the loss stops it.
    #[tokio::test]
    async fn a_run_that_lost_its_lease_writes_no_progress_and_is_told_to_stop() {
        let dbs = Databases::open().await;
        for (a, b) in dbs.instances() {
            let id = new_job(&a).await;
            let first = hold_job(&a, &id, "a").await.unwrap().unwrap();
            let held = Held::new(&id, lease("a", &first));
            run_under(held.clone(), async {
                update_progress(&a, &id, &serde_json::json!({ "step": 1 }))
                    .await
                    .unwrap();
            })
            .await;
            assert_eq!(progress_of(&a, &id).await, serde_json::json!({ "step": 1 }));

            taken_over(&a, &b, &id).await;
            let stop = run_under(held.clone(), async {
                update_progress(&a, &id, &serde_json::json!({ "step": "stale" }))
                    .await
                    .unwrap();
                should_stop(&a, &id).await
            })
            .await;
            assert_eq!(
                progress_of(&a, &id).await,
                serde_json::json!({ "step": 1 }),
                "{:?}",
                a.db_backend
            );
            assert!(held.is_lost());
            assert_eq!(stop, Some("lost"));
        }
        dbs.finish().await;
    }

    /// `should_stop` finds the loss itself, before any write has.
    #[tokio::test]
    async fn should_stop_answers_lost_once_the_job_is_taken_over() {
        let dbs = Databases::open().await;
        for (a, b) in dbs.instances() {
            let id = new_job(&a).await;
            let first = hold_job(&a, &id, "a").await.unwrap().unwrap();
            let held = Held::new(&id, lease("a", &first));
            let before = run_under(held.clone(), should_stop(&a, &id)).await;
            assert_eq!(before, None);
            taken_over(&a, &b, &id).await;
            let after = run_under(held.clone(), should_stop(&a, &id)).await;
            assert_eq!(after, Some("lost"), "{:?}", a.db_backend);
            assert!(held.is_lost());
        }
        dbs.finish().await;
    }

    /// Two runs of one job in one process: the lease expired here while its
    /// renewals failed, and another worker of this same process re-claimed
    /// the job, under the same owner. The stale run still finds its own
    /// lease rather than the newer run's, so its writes fail the fence and it
    /// is told to stop, before and after the newer run ends.
    #[tokio::test]
    async fn a_stale_run_is_fenced_when_its_job_is_reclaimed_in_the_same_process() {
        let dbs = Databases::open().await;
        for (a, _) in dbs.instances() {
            let id = new_job(&a).await;
            let first = hold_job(&a, &id, "x").await.unwrap().unwrap();
            let stale = Held::new(&id, lease("x", &first));
            expire(&a, &id).await;
            recover_expired_jobs(&a).await.unwrap();
            let second = hold_job(&a, &id, "x").await.unwrap().unwrap();
            let fresh = Held::new(&id, lease("x", &second));

            run_under(fresh.clone(), async {
                update_progress(&a, &id, &serde_json::json!({ "run": "fresh" }))
                    .await
                    .unwrap();
                assert_eq!(should_stop(&a, &id).await, None);
            })
            .await;
            let stop = run_under(stale.clone(), async {
                update_progress(&a, &id, &serde_json::json!({ "run": "stale" }))
                    .await
                    .unwrap();
                should_stop(&a, &id).await
            })
            .await;
            assert_eq!(
                progress_of(&a, &id).await,
                serde_json::json!({ "run": "fresh" }),
                "{:?}",
                a.db_backend
            );
            assert_eq!(stop, Some("lost"));
            assert!(!fresh.is_lost());

            // The newer run has finished; the stale one writes nothing still.
            assert!(
                finish(
                    &a,
                    &id,
                    &fresh.lease,
                    Finish::Completed(&serde_json::json!({}))
                )
                .await
                .unwrap()
            );
            run_under(stale.clone(), async {
                update_progress(&a, &id, &serde_json::json!({ "run": "late" }))
                    .await
                    .unwrap();
            })
            .await;
            assert_eq!(
                progress_of(&a, &id).await,
                serde_json::json!({ "run": "fresh" })
            );
        }
        dbs.finish().await;
    }

    /// Each half of the progress fence holds on its own.
    #[tokio::test]
    async fn a_progress_write_needs_both_the_owner_and_the_attempt() {
        let dbs = Databases::open().await;
        for (a, _) in dbs.instances() {
            let id = new_job(&a).await;
            let held = hold_job(&a, &id, "a").await.unwrap().unwrap();
            let before = progress_of(&a, &id).await;
            for (case, lease) in [
                (
                    "another owner",
                    Lease {
                        owner: "b".into(),
                        attempt: held.attempt,
                    },
                ),
                (
                    "another attempt",
                    Lease {
                        owner: "a".into(),
                        attempt: held.attempt + 1,
                    },
                ),
            ] {
                let run = Held::new(&id, lease);
                run_under(run.clone(), async {
                    update_progress(&a, &id, &serde_json::json!({ "case": case }))
                        .await
                        .unwrap();
                })
                .await;
                assert_eq!(
                    progress_of(&a, &id).await,
                    before,
                    "{case}: {:?}",
                    a.db_backend
                );
                assert!(run.is_lost(), "{case}");
            }
        }
        dbs.finish().await;
    }

    /// Progress belongs to a job's run: written from anywhere else, it lands
    /// nowhere.
    #[tokio::test]
    async fn progress_written_outside_the_jobs_run_lands_nowhere() {
        let dbs = Databases::open().await;
        for (a, _) in dbs.instances() {
            let id = new_job(&a).await;
            let held = hold_job(&a, &id, "a").await.unwrap().unwrap();
            let before = progress_of(&a, &id).await;
            update_progress(&a, &id, &serde_json::json!({ "from": "nowhere" }))
                .await
                .unwrap();
            // Nor from a run of another job.
            let other = new_job(&a).await;
            run_under(Held::new(&other, lease("a", &held)), async {
                update_progress(&a, &id, &serde_json::json!({ "from": "another job" }))
                    .await
                    .unwrap();
            })
            .await;
            assert_eq!(progress_of(&a, &id).await, before, "{:?}", a.db_backend);
        }
        dbs.finish().await;
    }

    /// The renewal loop is what notices a lease lost while the run makes no
    /// writes at all.
    #[tokio::test]
    async fn renewal_marks_a_lease_lost_once_the_job_is_taken_over() {
        let dbs = Databases::open().await;
        for (a, b) in dbs.instances() {
            let id = new_job(&a).await;
            let first = hold_job(&a, &id, "a").await.unwrap().unwrap();
            let held = Held::new(&id, lease("a", &first));
            let renewal = tokio::spawn(super::super::worker::renew_while_running(
                a.clone(),
                held.clone(),
                Duration::from_millis(20),
            ));
            tokio::time::sleep(Duration::from_millis(80)).await;
            assert!(!held.is_lost(), "a live lease renews");
            assert_eq!(job(&a, &id).await.status, "running");

            // In one statement: expiring the lease and then recovering it
            // would race the renewal running every 20ms.
            crate::db::query(&adapt_sql(
                "UPDATE happyview_jobs SET claimed_by = 'b', attempt = attempt + 1 WHERE id = ?",
                b.db_backend,
            ))
            .bind(&id)
            .execute(&b.db)
            .await
            .unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !held.is_lost() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "renewal never noticed"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            renewal.abort();
        }
        dbs.finish().await;
    }

    /// Each half of the fence holds on its own: another owner at the same
    /// attempt, and the same owner at another attempt, are both refused.
    #[tokio::test]
    async fn the_fence_needs_both_owner_and_attempt() {
        let dbs = Databases::open().await;
        for (a, _) in dbs.instances() {
            let id = new_job(&a).await;
            let held = hold_job(&a, &id, "a").await.unwrap().unwrap();
            let done = serde_json::json!({ "ok": true });
            let other_owner = Lease {
                owner: "b".into(),
                attempt: held.attempt,
            };
            let other_attempt = Lease {
                owner: "a".into(),
                attempt: held.attempt + 1,
            };
            assert!(
                !finish(&a, &id, &other_owner, Finish::Completed(&done))
                    .await
                    .unwrap()
            );
            assert!(!renew_lease(&a, &id, &other_owner).await.unwrap());
            assert!(
                !finish(&a, &id, &other_attempt, Finish::Completed(&done))
                    .await
                    .unwrap()
            );
            assert!(!renew_lease(&a, &id, &other_attempt).await.unwrap());
            assert_eq!(job(&a, &id).await.status, "running", "{:?}", a.db_backend);
            assert!(
                finish(&a, &id, &lease("a", &held), Finish::Completed(&done))
                    .await
                    .unwrap()
            );
        }
        dbs.finish().await;
    }

    /// Pausing and resuming claims the job again without losing anything,
    /// so it never counts toward the limit on lost workers.
    #[tokio::test]
    async fn pausing_and_resuming_does_not_count_as_a_lost_worker() {
        let dbs = Databases::open().await;
        for (a, _) in dbs.instances() {
            let id = new_job(&a).await;
            for _ in 0..MAX_LOST_LEASES + 1 {
                let held = hold_job(&a, &id, "a").await.unwrap().unwrap();
                assert!(change_status(&a, &id, "running", "pausing").await.unwrap());
                assert!(
                    finish(&a, &id, &lease("a", &held), Finish::Paused)
                        .await
                        .unwrap()
                );
                assert!(change_status(&a, &id, "paused", "pending").await.unwrap());
            }
            hold_job(&a, &id, "a").await.unwrap().unwrap();
            expire(&a, &id).await;
            recover_expired_jobs(&a).await.unwrap();
            assert_eq!(job(&a, &id).await.status, "pending", "{:?}", a.db_backend);
        }
        dbs.finish().await;
    }

    /// An operator's pause read against a running job doesn't land on one
    /// that finished meanwhile.
    #[tokio::test]
    async fn a_status_change_lands_only_on_the_status_it_was_read_against() {
        let dbs = Databases::open().await;
        for (a, _) in dbs.instances() {
            let id = new_job(&a).await;
            let held = hold_job(&a, &id, "a").await.unwrap().unwrap();
            let done = serde_json::json!({});
            assert!(
                finish(&a, &id, &lease("a", &held), Finish::Completed(&done))
                    .await
                    .unwrap()
            );
            assert!(!change_status(&a, &id, "running", "pausing").await.unwrap());
            assert_eq!(job(&a, &id).await.status, "completed", "{:?}", a.db_backend);
        }
        dbs.finish().await;
    }

    /// Two workers claiming at once: one gets the job. On the SQLite pool
    /// alone, which no other test shares.
    #[tokio::test]
    async fn two_workers_racing_for_one_job_claim_it_once() {
        let (pool, backend) = test_pools().await.remove(0);
        let state = test_state_with_pool_on(pool, backend);
        let id = new_job(&state).await;
        let (x, y) = tokio::join!(claim_next_job(&state, "x"), claim_next_job(&state, "y"));
        let claimed: Vec<Job> = [x.unwrap(), y.unwrap()].into_iter().flatten().collect();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].id, id);
        assert_eq!(claimed[0].attempt, 1);
    }

    /// One instance, as before leases: the worker's claim runs the job and
    /// its outcome is recorded. On the SQLite pool alone, which no other
    /// test shares, since `claim_next_job` takes the oldest pending job.
    #[tokio::test]
    async fn a_single_worker_claims_and_finishes_as_before() {
        let (pool, backend) = test_pools().await.remove(0);
        let state = test_state_with_pool_on(pool, backend);
        let id = new_job(&state).await;

        let claimed = claim_next_job(&state, "solo")
            .await
            .unwrap()
            .expect("a job");
        assert_eq!(claimed.id, id);
        assert_eq!(claimed.status, "running");
        assert_eq!(claimed.attempt, 1);
        assert_eq!(owner_of(&state, &id).await.as_deref(), Some("solo"));
        assert!(claim_next_job(&state, "solo").await.unwrap().is_none());

        let result = serde_json::json!({ "ok": true });
        assert!(
            finish(
                &state,
                &id,
                &lease("solo", &claimed),
                Finish::Completed(&result)
            )
            .await
            .unwrap()
        );
        let done = job(&state, &id).await;
        assert_eq!(done.status, "completed");
        assert_eq!(done.result, Some(result));
        assert!(done.completed_at.is_some());
    }
}
