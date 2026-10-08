//! Enqueuing a background job as the script runner, from a library plugin.
//! Mirrors the Lua `jobs.create` global: the same job-type validator, the
//! same requirement that `auth = true` carry an actual DPoP session rather
//! than silently enqueueing a job with nothing to act as.

use crate::AppState;
use crate::plugin::caller::CallerSession;
use crate::repo::PdsAuth;

use happyview_plugin_sdk::wire::{JobCreate, JobGet, JobListAny, JobView};

#[derive(Debug, thiserror::Error)]
pub enum JobsError {
    #[error("{0}")]
    Invalid(String),
    #[error("this call requires an authenticated caller")]
    NoCaller,
    #[error("auth = true requires a DPoP session to carry into the job")]
    NoSession,
    #[error("{0}")]
    Database(String),
}

impl JobsError {
    /// A bad job type and a missing caller are both the plugin's mistake, so
    /// they share `BAD_INPUT`; a runner with no DPoP session to carry is a
    /// different problem the plugin cannot fix on its own.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) | Self::NoCaller => "BAD_INPUT",
            Self::NoSession => "NO_SESSION",
            Self::Database(_) => "HOST_ERROR",
        }
    }
}

pub async fn create(
    state: &AppState,
    caller_did: Option<&str>,
    session: Option<&CallerSession>,
    spec: JobCreate,
) -> Result<String, JobsError> {
    crate::jobs::validate_job_type(&spec.job_type).map_err(JobsError::Invalid)?;

    let caller_did = caller_did.ok_or(JobsError::NoCaller)?;

    let (api_client_id, dpop_key_id) = if spec.auth {
        let dpop_ids = session
            .and_then(|s| match s.pds_auth.as_ref() {
                PdsAuth::Dpop {
                    api_client_id,
                    dpop_key_id,
                    ..
                } => Some((api_client_id.clone(), dpop_key_id.clone())),
                _ => None,
            })
            .ok_or(JobsError::NoSession)?;
        (Some(dpop_ids.0), Some(dpop_ids.1))
    } else {
        (None, None)
    };

    crate::jobs::db::create_job(
        state,
        &spec.job_type,
        &spec.input,
        caller_did,
        spec.auth,
        api_client_id.as_deref(),
        dpop_key_id.as_deref(),
    )
    .await
    .map_err(|e| JobsError::Database(e.to_string()))
}

/// Drops the session fields (`inherit_auth`, `api_client_id`, `dpop_key_id`)
/// so a plugin never sees what a job could act as.
pub(crate) fn view(job: crate::jobs::Job) -> JobView {
    JobView {
        id: job.id,
        job_type: job.job_type,
        status: job.status,
        input: job.input,
        progress: job.progress,
        result: job.result,
        error: job.error,
        created_by: job.created_by,
        created_at: job.created_at,
        started_at: job.started_at,
        completed_at: job.completed_at,
    }
}

/// One of the caller's own jobs. Another user's job answers `None`, exactly
/// like a missing one, so a plugin cannot probe for ids it does not own.
pub async fn get(
    state: &AppState,
    caller_did: Option<&str>,
    spec: JobGet,
) -> Result<Option<JobView>, JobsError> {
    let caller_did = caller_did.ok_or(JobsError::NoCaller)?;
    let job = crate::jobs::db::get_job(state, &spec.id)
        .await
        .map_err(|e| JobsError::Database(e.to_string()))?;
    Ok(job.filter(|j| j.created_by == caller_did).map(view))
}

pub async fn get_any(state: &AppState, spec: JobGet) -> Result<Option<JobView>, JobsError> {
    let job = crate::jobs::db::get_job(state, &spec.id)
        .await
        .map_err(|e| JobsError::Database(e.to_string()))?;
    Ok(job.map(view))
}

const DEFAULT_LIST_LIMIT: u32 = 50;
const MAX_LIST_LIMIT: u32 = 200;

fn effective_limit(limit: Option<u32>) -> Result<u32, JobsError> {
    match limit {
        None => Ok(DEFAULT_LIST_LIMIT),
        Some(0) => Err(JobsError::Invalid("limit must be at least 1".into())),
        Some(n) => Ok(n.min(MAX_LIST_LIMIT)),
    }
}

pub async fn list_any(state: &AppState, spec: JobListAny) -> Result<Vec<JobView>, JobsError> {
    let limit = effective_limit(spec.limit)?;
    let jobs = crate::jobs::db::list_jobs_any(state, &spec.status, spec.job_type.as_deref(), limit)
        .await
        .map_err(|e| JobsError::Database(e.to_string()))?;
    Ok(jobs.into_iter().map(view).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Claims;
    use std::sync::Arc;

    async fn seeded_state() -> AppState {
        let pool = crate::test_support::migrated_memory_pool().await;
        crate::test_support::test_state_with_pool(pool)
    }

    fn dpop_session(state: &AppState, did: &str) -> CallerSession {
        CallerSession {
            did: did.to_string(),
            delegate_did: None,
            claims: Arc::new(Claims::internal(did.to_string())),
            pds_auth: Arc::new(PdsAuth::Dpop {
                api_client_id: "test_client".into(),
                dpop_key_id: "test_key".into(),
                encryption_key: [0u8; 32],
            }),
            app_state: state.clone(),
        }
    }

    // `sqlx::Any`'s SQLite bridge cannot decode the real `inherit_auth
    // BOOLEAN` column at all (not even into `bool`) — the same reason
    // `jobs::db`'s own queries cast it to `INTEGER` first.
    async fn job_row(state: &AppState, id: &str) -> (String, i32, Option<String>, Option<String>) {
        crate::db::query_as(
            "SELECT created_by, CAST(inherit_auth AS INTEGER), api_client_id, dpop_key_id FROM happyview_jobs WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&state.db)
        .await
        .expect("job row should exist")
    }

    fn spec(job_type: &str, auth: bool) -> JobCreate {
        JobCreate {
            job_type: job_type.into(),
            input: serde_json::json!({}),
            auth,
        }
    }

    #[tokio::test]
    async fn create_without_auth_omits_dpop_context() {
        let state = seeded_state().await;

        let id = create(
            &state,
            Some("did:plc:caller"),
            None,
            spec("test.no-auth", false),
        )
        .await
        .expect("create should succeed");

        let (created_by, inherit_auth, api_client_id, dpop_key_id) = job_row(&state, &id).await;
        assert_eq!(created_by, "did:plc:caller");
        assert_eq!(inherit_auth, 0);
        assert!(api_client_id.is_none());
        assert!(dpop_key_id.is_none());
    }

    #[tokio::test]
    async fn create_with_auth_carries_the_sessions_dpop_ids() {
        let state = seeded_state().await;
        let session = dpop_session(&state, "did:plc:caller");

        let id = create(
            &state,
            Some("did:plc:caller"),
            Some(&session),
            spec("test.with-auth", true),
        )
        .await
        .expect("create should succeed");

        let (created_by, inherit_auth, api_client_id, dpop_key_id) = job_row(&state, &id).await;
        assert_eq!(created_by, "did:plc:caller");
        assert_eq!(inherit_auth, 1);
        assert_eq!(api_client_id.as_deref(), Some("test_client"));
        assert_eq!(dpop_key_id.as_deref(), Some("test_key"));
    }

    #[tokio::test]
    async fn a_reserved_job_type_is_invalid() {
        let state = seeded_state().await;
        let err = create(
            &state,
            Some("did:plc:caller"),
            None,
            spec("happyview.x", false),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, JobsError::Invalid(_)), "{err}");
        assert_eq!(err.code(), "BAD_INPUT");
    }

    #[tokio::test]
    async fn no_caller_did_is_bad_input() {
        let state = seeded_state().await;
        let err = create(&state, None, None, spec("test.no-caller", false))
            .await
            .unwrap_err();
        assert!(matches!(err, JobsError::NoCaller), "{err}");
        assert_eq!(err.code(), "BAD_INPUT");
    }

    #[tokio::test]
    async fn auth_without_a_session_has_no_session() {
        let state = seeded_state().await;
        let err = create(
            &state,
            Some("did:plc:caller"),
            None,
            spec("test.needs-session", true),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, JobsError::NoSession), "{err}");
        assert_eq!(err.code(), "NO_SESSION");
    }

    /// A cookie-authenticated runner has a session, but it is `PdsAuth::OAuth`
    /// rather than `PdsAuth::Dpop`, and there are no DPoP identifiers to carry
    /// into the job — the `_ => None` arm in `create`'s match is what refuses
    /// this rather than silently enqueueing with nothing to restore later.
    /// Building a real `PdsAuth::OAuth` means restoring an actual
    /// `atrium_oauth` session, since its constructor is private to that
    /// crate: a stub session (dpop key + token set) is written straight into
    /// the same session table `linked_repos_client` reads from, and a
    /// wiremock server stands in for the token set's issuer, which
    /// `restore()` fetches authorization-server metadata from.
    async fn oauth_session(state: &AppState, did_str: &str) -> crate::HappyViewOAuthSession {
        use atrium_common::store::Store;

        let did =
            atrium_api::types::string::Did::new(did_str.to_string()).expect("valid did for test");

        let issuer = wiremock::MockServer::start().await;
        let issuer_url = issuer.uri();
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/.well-known/oauth-authorization-server",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "issuer": issuer_url,
                    "authorization_endpoint": format!("{issuer_url}/authorize"),
                    "token_endpoint": format!("{issuer_url}/token"),
                    "scopes_supported": ["atproto"],
                    "response_types_supported": ["code"],
                })),
            )
            .mount(&issuer)
            .await;

        let dpop_key = crate::oauth::client_keys::generate_client_key("test")
            .expect("generate a test dpop key")
            .private_jwk;
        let session: atrium_oauth::store::session::Session =
            serde_json::from_value(serde_json::json!({
                "dpop_key": dpop_key,
                "token_set": {
                    "iss": issuer_url,
                    "sub": did_str,
                    "aud": issuer_url,
                    "scope": null,
                    "refresh_token": null,
                    "access_token": "test-access-token",
                    "token_type": "DPoP",
                    "expires_at": null,
                },
            }))
            .expect("build a stub oauth session");

        let session_store = crate::auth::oauth_store::DbSessionStore::new_with_table(
            state.db.clone(),
            state.db_backend,
            crate::linked_repos::client::LINKED_SESSIONS_TABLE,
        );
        Store::set(&session_store, did.clone(), session)
            .await
            .expect("store the stub session");

        state
            .linked_repos_client
            .restore(&did)
            .await
            .expect("restore the stub session")
    }

    #[tokio::test]
    async fn auth_with_a_non_dpop_session_has_no_session() {
        let state = seeded_state().await;
        let did = "did:plc:oauthcaller";
        let session = oauth_session(&state, did).await;

        let caller = CallerSession {
            did: did.to_string(),
            delegate_did: None,
            claims: Arc::new(Claims::internal(did.to_string())),
            pds_auth: Arc::new(PdsAuth::OAuth(Arc::new(session))),
            app_state: state.clone(),
        };

        let err = create(
            &state,
            Some(did),
            Some(&caller),
            spec("test.oauth-caller", true),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, JobsError::NoSession), "{err}");
        assert_eq!(err.code(), "NO_SESSION");
    }

    async fn seed(state: &AppState, by: &str, job_type: &str) -> String {
        crate::jobs::db::create_job(
            state,
            job_type,
            &serde_json::json!({"k": 1}),
            by,
            false,
            None,
            None,
        )
        .await
        .expect("seed a job")
    }

    #[tokio::test]
    async fn get_returns_the_callers_own_job_without_session_fields() {
        let state = seeded_state().await;
        let id = seed(&state, "did:plc:owner", "instance.operation").await;
        let view = get(&state, Some("did:plc:owner"), JobGet { id: id.clone() })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(view.id, id);
        assert_eq!(view.created_by, "did:plc:owner");
        assert_eq!(view.input, serde_json::json!({"k": 1}));
        let text = serde_json::to_string(&view).unwrap();
        assert!(!text.contains("dpop_key_id") && !text.contains("api_client_id"));
        assert!(!text.contains("inherit_auth"));
    }

    #[tokio::test]
    async fn get_hides_another_users_job_exactly_like_a_missing_one() {
        let state = seeded_state().await;
        let id = seed(&state, "did:plc:owner", "t").await;
        assert_eq!(
            get(&state, Some("did:plc:other"), JobGet { id })
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            get(&state, Some("did:plc:other"), JobGet { id: "nope".into() })
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn get_without_a_caller_is_bad_input() {
        let state = seeded_state().await;
        let err = get(&state, None, JobGet { id: "x".into() })
            .await
            .unwrap_err();
        assert_eq!(err.code(), "BAD_INPUT");
    }

    #[tokio::test]
    async fn get_any_reads_any_users_job() {
        let state = seeded_state().await;
        let id = seed(&state, "did:plc:owner", "t").await;
        assert_eq!(
            get_any(&state, JobGet { id: id.clone() })
                .await
                .unwrap()
                .unwrap()
                .id,
            id
        );
        assert_eq!(
            get_any(&state, JobGet { id: "nope".into() }).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn list_any_filters_by_status_and_type_newest_first_and_caps_the_limit() {
        let state = seeded_state().await;
        let a = seed(&state, "did:plc:a", "instance.operation").await;
        let b = seed(&state, "did:plc:b", "instance.operation").await;
        seed(&state, "did:plc:c", "other").await;
        let rows = list_any(
            &state,
            JobListAny {
                status: vec!["pending".into()],
                job_type: Some("instance.operation".into()),
                limit: None,
            },
        )
        .await
        .unwrap();
        let expected = vec![b, a];
        assert_eq!(
            rows.iter().map(|j| j.id.clone()).collect::<Vec<_>>(),
            expected
        );
        assert!(
            list_any(
                &state,
                JobListAny {
                    limit: Some(1000),
                    ..Default::default()
                }
            )
            .await
            .unwrap()
            .len()
                <= 200
        );
        assert_eq!(
            list_any(
                &state,
                JobListAny {
                    limit: Some(0),
                    ..Default::default()
                }
            )
            .await
            .unwrap_err()
            .code(),
            "BAD_INPUT"
        );
    }

    #[tokio::test]
    async fn list_any_status_filter_excludes_other_statuses() {
        let state = seeded_state().await;
        seed(&state, "did:plc:a", "t").await;
        let rows = list_any(
            &state,
            JobListAny {
                status: vec!["completed".into(), "failed".into()],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn list_any_treats_hostile_values_as_data() {
        let state = seeded_state().await;
        seed(&state, "did:plc:a", "t").await;
        let rows = list_any(
            &state,
            JobListAny {
                status: vec!["pending' OR '1'='1".into()],
                job_type: Some("t' OR '1'='1".into()),
                limit: None,
            },
        )
        .await
        .unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn effective_limit_defaults_to_50_and_clamps_to_200() {
        assert_eq!(effective_limit(None).unwrap(), 50);
        assert_eq!(effective_limit(Some(1)).unwrap(), 1);
        assert_eq!(effective_limit(Some(200)).unwrap(), 200);
        assert_eq!(effective_limit(Some(1000)).unwrap(), 200);
        assert_eq!(effective_limit(Some(0)).unwrap_err().code(), "BAD_INPUT");
    }
}
