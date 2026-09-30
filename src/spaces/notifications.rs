use crate::db::{DatabaseBackend, now_rfc3339};
use crate::error::AppError;
use crate::spaces::db;
use crate::spaces::types::{NotifyDelivery, NotifyRegistration};
use uuid::Uuid;

const NOTIFY_REGISTRATION_TTL_SECS: u64 = 24 * 60 * 60; // 24 hours

/// Store a registration, returning its id and expiry.
pub async fn register(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    space_id: &str,
    service_did: &str,
    endpoint: &str,
    registered_by: &str,
    delivery: NotifyDelivery,
) -> Result<(String, String), AppError> {
    let id = Uuid::new_v4().to_string();
    let now = now_rfc3339();
    let expires_at = {
        let expiry = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + NOTIFY_REGISTRATION_TTL_SECS;
        chrono::DateTime::from_timestamp(expiry as i64, 0)
            .unwrap()
            .to_rfc3339()
    };
    let reg = NotifyRegistration {
        id: id.clone(),
        space_id: space_id.to_string(),
        service: service_did.to_string(),
        endpoint: endpoint.to_string(),
        registered_by: registered_by.to_string(),
        expires_at: expires_at.clone(),
        created_at: now,
        delivery,
    };
    db::register_notify(pool, backend, &reg).await?;
    Ok((id, expires_at))
}

#[allow(clippy::too_many_arguments)]
pub async fn dispatch_write_notification(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    http: &reqwest::Client,
    space_id: &str,
    author_did: &str,
    collection: &str,
    rkey: &str,
    cid: Option<&str>,
) -> Result<(), AppError> {
    let registrations = db::list_notify_registrations(pool, backend, space_id).await?;

    let payload = serde_json::json!({
        "space": space_id,
        "did": author_did,
        "collection": collection,
        "rkey": rkey,
        "cid": cid,
    });

    for reg in registrations
        .iter()
        .filter(|r| r.delivery == NotifyDelivery::Webhook)
    {
        let _ = http.post(&reg.endpoint).json(&payload).send().await;
    }

    Ok(())
}

pub async fn dispatch_space_deleted(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    http: &reqwest::Client,
    space_id: &str,
) -> Result<(), AppError> {
    let registrations = db::list_notify_registrations(pool, backend, space_id).await?;

    let payload = serde_json::json!({ "space": space_id });

    for reg in &registrations {
        let _ = http.post(&reg.endpoint).json(&payload).send().await;
    }

    Ok(())
}

/// The method a forwarded write notification calls, and the `lxm` its service
/// auth is bound to.
const NOTIFY_WRITE_LXM: &str = "com.atproto.space.notifyWrite";

/// Where a forwarded notification carries the space revision that ordered it,
/// and the one before. Proposal 0016 adds both without naming them yet.
pub(crate) const SPACE_REV_FIELD: &str = "spaceRev";
const PREV_SPACE_REV_FIELD: &str = "prevSpaceRev";

/// A repo's new state, as `com.atproto.space.notifyWrite` reports it.
#[derive(Debug, Clone)]
pub struct RepoUpdate {
    pub space_uri: String,
    pub repo: String,
    pub rev: String,
    pub hash: Vec<u8>,
    pub space_rev: String,
    pub prev_space_rev: Option<String>,
}

impl RepoUpdate {
    fn body(&self) -> serde_json::Value {
        use base64::Engine;
        let mut body = serde_json::json!({
            "space": self.space_uri,
            "repo": self.repo,
            "rev": self.rev,
            "hash": {
                "$bytes": base64::engine::general_purpose::STANDARD_NO_PAD.encode(&self.hash),
            },
        });
        body[SPACE_REV_FIELD] = self.space_rev.clone().into();
        if let Some(prev) = &self.prev_space_rev {
            body[PREV_SPACE_REV_FIELD] = prev.clone().into();
        }
        body
    }
}

/// Pass a repo update to the services registered for its space by identifier.
///
/// Runs in the background, so neither a write nor an inbound notification waits
/// on syncers. Delivery is best effort: a syncer that misses one catches up
/// through `listRepos`.
pub fn forward_repo_update(state: &crate::AppState, space_id: &str, update: RepoUpdate) {
    let state = state.clone();
    let space_id = space_id.to_string();
    tokio::spawn(async move {
        if let Err(e) = deliver_repo_update(&state, &space_id, &update).await {
            tracing::warn!(space_id, error = %e, "failed to forward a write notification");
        }
    });
}

async fn deliver_repo_update(
    state: &crate::AppState,
    space_id: &str,
    update: &RepoUpdate,
) -> Result<(), AppError> {
    let registrations = db::list_notify_registrations(&state.db, state.db_backend, space_id)
        .await?
        .into_iter()
        .filter(|r| r.delivery == NotifyDelivery::Xrpc)
        .collect::<Vec<_>>();
    if registrations.is_empty() {
        return Ok(());
    }

    let encryption_key = state.config.token_encryption_key.as_ref().ok_or_else(|| {
        AppError::Internal("TOKEN_ENCRYPTION_KEY is required to sign write notifications".into())
    })?;
    let body = update.body();

    for reg in &registrations {
        let token = match crate::auth::service_auth::mint_service_auth(
            &state.db,
            state.db_backend,
            encryption_key,
            &state.config.public_url,
            &reg.service,
            NOTIFY_WRITE_LXM,
        )
        .await
        {
            Ok(token) => token,
            Err(e) => {
                tracing::warn!(service = %reg.service, error = %e, "could not sign a write notification");
                continue;
            }
        };
        let url = format!(
            "{}/xrpc/{NOTIFY_WRITE_LXM}",
            reg.endpoint.trim_end_matches('/')
        );
        let result = state
            .http
            .post(&url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await;
        match result {
            Ok(resp) if !resp.status().is_success() => {
                tracing::warn!(service = %reg.service, status = %resp.status(), "syncer rejected a write notification");
            }
            Err(e) => {
                tracing::warn!(service = %reg.service, error = %e, "could not deliver a write notification");
            }
            Ok(_) => {}
        }
    }

    Ok(())
}

/// The method that tells a repo host a credential is revoked, and the `lxm` its
/// service auth is bound to.
const NOTIFY_CREDENTIAL_REVOKED_LXM: &str = "com.atproto.space.notifyCredentialRevoked";

/// How many `jti`s one `notifyCredentialRevoked` call may carry.
const MAX_REVOKED_PER_CALL: usize = 100;

/// Tell the hosts of a space's native repos that credentials were revoked.
///
/// Those hosts verify this instance's credentials without asking it, so until
/// they hear, a revoked credential keeps working there until it expires. Only
/// sent for spaces this instance is the authority for: the call is service
/// auth from the authority, which this instance can only sign as itself.
/// Best effort, and a host that does not implement the method is skipped.
pub fn announce_revoked_credentials(
    state: &crate::AppState,
    space: &crate::spaces::types::Space,
    jtis: Vec<String>,
) {
    let state = state.clone();
    let space = space.clone();
    tokio::spawn(async move {
        if let Err(e) = deliver_revocations(&state, &space, &jtis).await {
            tracing::warn!(space_id = %space.id, error = %e, "failed to announce revoked credentials");
        }
    });
}

async fn deliver_revocations(
    state: &crate::AppState,
    space: &crate::spaces::types::Space,
    jtis: &[String],
) -> Result<(), AppError> {
    let instance = crate::auth::service_auth::instance_did(
        &state.db,
        state.db_backend,
        &state.config.public_url,
    )
    .await;
    if instance.ok().as_deref() != Some(space.authority_did.as_str()) {
        return Ok(());
    }
    let encryption_key = state.config.token_encryption_key.as_ref().ok_or_else(|| {
        AppError::Internal("TOKEN_ENCRYPTION_KEY is required to sign revocations".into())
    })?;
    let space_uri = format!(
        "at://{}/space/{}/{}",
        space.did, space.type_nsid, space.skey
    );

    for repo in db::list_native_repo_authors(&state.db, state.db_backend, &space.id).await? {
        let Some(endpoint) = crate::spaces::auth::resolve_service_identifier(
            &state.http,
            &state.config.plc_url,
            &repo,
        )
        .await
        else {
            tracing::warn!(
                repo,
                "could not resolve a native repo's host to revoke credentials"
            );
            continue;
        };
        let url = format!(
            "{}/xrpc/{NOTIFY_CREDENTIAL_REVOKED_LXM}",
            endpoint.trim_end_matches('/')
        );
        for batch in jtis.chunks(MAX_REVOKED_PER_CALL) {
            // Addressed to the repo, which is what its host accepts service auth for.
            let token = crate::auth::service_auth::mint_service_auth(
                &state.db,
                state.db_backend,
                encryption_key,
                &state.config.public_url,
                &repo,
                NOTIFY_CREDENTIAL_REVOKED_LXM,
            )
            .await?;
            let result = state
                .http
                .post(&url)
                .bearer_auth(token)
                .json(&serde_json::json!({ "space": space_uri, "credentials": batch }))
                .send()
                .await;
            match result {
                Ok(resp) if !resp.status().is_success() => {
                    tracing::warn!(repo, status = %resp.status(), "repo host did not accept a revocation");
                }
                Err(e) => tracing::warn!(repo, error = %e, "could not deliver a revocation"),
                Ok(_) => {}
            }
        }
    }
    Ok(())
}
