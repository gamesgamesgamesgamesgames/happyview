//! Enqueuing a background job as the script runner, from a library plugin.
//! Mirrors the Lua `jobs.create` global: the same job-type validator, the
//! same requirement that `auth = true` carry an actual DPoP session rather
//! than silently enqueueing a job with nothing to act as.

use crate::AppState;
use crate::plugin::caller::CallerSession;
use crate::repo::PdsAuth;

use happyview_plugin_sdk::wire::JobCreate;

#[derive(Debug, thiserror::Error)]
pub enum JobsError {
    #[error("{0}")]
    Invalid(String),
    #[error("jobs.create requires an authenticated caller")]
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
}
