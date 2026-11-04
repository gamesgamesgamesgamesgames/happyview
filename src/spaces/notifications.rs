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

/// What signs a webhook delivery: the instance's keyring, to open its
/// `#atproto` signing key, and its public URL, which names its DID.
#[derive(Clone, Copy)]
pub struct WebhookSigner<'a> {
    pub encryption_key: &'a crate::plugin::encryption::Keyring,
    pub public_url: &'a str,
}

/// POST `body` to a webhook registration. The request carries service auth
/// from this instance in `Authorization: Bearer`, the same JWT a
/// registration by identifier receives: `aud` is the registration's service
/// DID and `lxm` the method the payload stands for, so a receiver can check
/// the token was minted for it and for this kind of notification. The body
/// is unchanged, so a receiver that ignores the header works as before.
/// Without a signer, or when minting fails, the payload is still sent,
/// unsigned, and the reason is logged.
async fn post_webhook(
    http: &reqwest::Client,
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    signer: Option<WebhookSigner<'_>>,
    reg: &NotifyRegistration,
    lxm: &str,
    body: &serde_json::Value,
) -> Result<reqwest::Response, reqwest::Error> {
    let mut request = http.post(&reg.endpoint).json(body);
    match signer {
        Some(signer) => match crate::auth::service_auth::mint_service_auth(
            pool,
            backend,
            signer.encryption_key,
            signer.public_url,
            &reg.service,
            lxm,
        )
        .await
        {
            Ok(token) => request = request.bearer_auth(token),
            Err(e) => {
                tracing::warn!(service = %reg.service, error = %e, "could not sign a webhook delivery; sending it unsigned");
            }
        },
        None => {
            tracing::warn!(service = %reg.service, "TOKEN_ENCRYPTION_KEY is unset, so a webhook delivery is sent unsigned");
        }
    }
    request.send().await
}

#[allow(clippy::too_many_arguments)]
pub async fn dispatch_write_notification(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    http: &reqwest::Client,
    signer: Option<WebhookSigner<'_>>,
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
        let _ = post_webhook(http, pool, backend, signer, reg, NOTIFY_WRITE_LXM, &payload).await;
    }

    Ok(())
}

/// Tell the services registered for a space that it was deleted.
///
/// Takes the registrations rather than reading them, because they are deleted
/// with the space. Services registered by identifier receive
/// `com.atproto.space.notifySpaceDeleted` signed by this instance; webhooks
/// receive the legacy payload. Runs in the background and is best effort.
pub fn announce_space_deleted(
    state: &crate::AppState,
    space: &crate::spaces::types::Space,
    registrations: Vec<NotifyRegistration>,
) {
    if registrations.is_empty() {
        return;
    }
    let state = state.clone();
    let space = space.clone();
    tokio::spawn(async move {
        let space_uri = format!(
            "at://{}/space/{}/{}",
            space.did, space.type_nsid, space.skey
        );
        for reg in &registrations {
            let result = match reg.delivery {
                NotifyDelivery::Webhook => {
                    let keyring = state.config.keyring();
                    let signer = keyring.as_ref().map(|encryption_key| WebhookSigner {
                        encryption_key,
                        public_url: &state.config.public_url,
                    });
                    post_webhook(
                        &state.http,
                        &state.db,
                        state.db_backend,
                        signer,
                        reg,
                        NOTIFY_SPACE_DELETED_LXM,
                        &serde_json::json!({ "space": space.id }),
                    )
                    .await
                }
                NotifyDelivery::Xrpc => {
                    let Some(key) = &state.config.keyring() else {
                        tracing::warn!("TOKEN_ENCRYPTION_KEY is required to sign a space deletion");
                        continue;
                    };
                    let token = match crate::auth::service_auth::mint_service_auth(
                        &state.db,
                        state.db_backend,
                        key,
                        &state.config.public_url,
                        &reg.service,
                        NOTIFY_SPACE_DELETED_LXM,
                    )
                    .await
                    {
                        Ok(token) => token,
                        Err(e) => {
                            tracing::warn!(service = %reg.service, error = %e, "could not sign a space deletion");
                            continue;
                        }
                    };
                    state
                        .http
                        .post(format!(
                            "{}/xrpc/{NOTIFY_SPACE_DELETED_LXM}",
                            reg.endpoint.trim_end_matches('/')
                        ))
                        .bearer_auth(token)
                        .json(&serde_json::json!({ "space": space_uri }))
                        .send()
                        .await
                }
            };
            if let Err(e) = result {
                tracing::warn!(service = %reg.service, error = %e, "could not deliver a space deletion");
            }
        }
    });
}

/// The method that tells a syncer a space was deleted, and the `lxm` its
/// service auth is bound to.
const NOTIFY_SPACE_DELETED_LXM: &str = "com.atproto.space.notifySpaceDeleted";

/// The method a forwarded write notification calls, and the `lxm` its service
/// auth is bound to.
const NOTIFY_WRITE_LXM: &str = "com.atproto.space.notifyWrite";

/// Where a forwarded notification carries the space revision that ordered it,
/// and the one before. A syncer that sees a gap between them missed an update.
const SPACE_REV_FIELD: &str = "spaceRev";
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
        let digest = serde_json::json!({
            "$bytes": base64::engine::general_purpose::STANDARD_NO_PAD.encode(&self.hash),
        });
        let mut body = serde_json::json!({
            "space": self.space_uri,
            "repo": self.repo,
            "repoRev": self.rev,
            // The alpha lexicon's name for `repoRev`. Sent until v3.
            "rev": self.rev,
            "data": digest,
            // The name for `data` before proposal 0016. Sent until every
            // syncer reads `data`.
            "hash": digest,
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

    let encryption_key = &state.config.keyring().ok_or_else(|| {
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
    let encryption_key = &state.config.keyring().ok_or_else(|| {
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

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use p256::ecdsa::signature::Verifier;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const INSTANCE_KEY: [u8; 32] = [0x33; 32];

    /// A pool whose service identity holds `INSTANCE_KEY`, sealed the way
    /// setup stores it, unless `with_identity` is false.
    async fn pool(with_identity: bool) -> (sqlx::AnyPool, DatabaseBackend) {
        let pool = crate::test_support::migrated_memory_pool().await;
        let backend = DatabaseBackend::Sqlite;
        if with_identity {
            let sealed = crate::plugin::encryption::encrypt(
                &crate::test_support::TEST_ENCRYPTION_KEY,
                &INSTANCE_KEY,
            )
            .unwrap();
            crate::service_identity::upsert_identity(
                &pool,
                backend,
                &crate::service_identity::IdentityMode::DidPlc,
                Some("did:plc:instance"),
                Some(&base64::engine::general_purpose::STANDARD.encode(sealed)),
                None,
                None,
            )
            .await
            .unwrap();
        }
        (pool, backend)
    }

    fn webhook(endpoint: String) -> NotifyRegistration {
        NotifyRegistration {
            id: "reg".into(),
            space_id: "space".into(),
            service: "did:web:syncer.example".into(),
            endpoint,
            registered_by: "did:web:syncer.example".into(),
            expires_at: "2999-01-01T00:00:00+00:00".into(),
            created_at: "2026-01-01T00:00:00+00:00".into(),
            delivery: NotifyDelivery::Webhook,
        }
    }

    /// Deliver `body` once and answer the one request the receiver saw.
    async fn deliver(
        pool: &sqlx::AnyPool,
        backend: DatabaseBackend,
        signer: Option<WebhookSigner<'_>>,
        lxm: &str,
        body: &serde_json::Value,
    ) -> wiremock::Request {
        let receiver = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&receiver)
            .await;
        post_webhook(
            &reqwest::Client::new(),
            pool,
            backend,
            signer,
            &webhook(receiver.uri()),
            lxm,
            body,
        )
        .await
        .expect("delivered");
        receiver.received_requests().await.unwrap().remove(0)
    }

    fn payload() -> serde_json::Value {
        serde_json::json!({
            "space": "space", "did": "did:plc:alice", "collection": "com.example.post",
            "rkey": "rk", "cid": null,
        })
    }

    #[tokio::test]
    async fn a_webhook_delivery_carries_service_auth_for_its_receiver() {
        let (pool, backend) = pool(true).await;
        let keyring =
            crate::plugin::encryption::Keyring::from(crate::test_support::TEST_ENCRYPTION_KEY);
        let signer = WebhookSigner {
            encryption_key: &keyring,
            public_url: "https://hv.example",
        };
        let request = deliver(&pool, backend, Some(signer), NOTIFY_WRITE_LXM, &payload()).await;

        // The body is the payload as before.
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body, payload());

        let auth = request.headers.get("authorization").expect("signed");
        let token = auth.to_str().unwrap().strip_prefix("Bearer ").unwrap();
        let parts: Vec<&str> = token.split('.').collect();
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["iss"], "did:plc:instance");
        assert_eq!(claims["aud"], "did:web:syncer.example");
        assert_eq!(claims["lxm"], NOTIFY_WRITE_LXM);

        // It verifies against the instance's #atproto key.
        let verifying = *p256::ecdsa::SigningKey::from_slice(&INSTANCE_KEY)
            .unwrap()
            .verifying_key();
        let sig =
            p256::ecdsa::Signature::from_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
        verifying
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
            .expect("the receiver can verify it against the instance's DID document key");
    }

    #[tokio::test]
    async fn a_space_deletion_webhook_is_bound_to_its_own_method() {
        let (pool, backend) = pool(true).await;
        let keyring =
            crate::plugin::encryption::Keyring::from(crate::test_support::TEST_ENCRYPTION_KEY);
        let signer = WebhookSigner {
            encryption_key: &keyring,
            public_url: "https://hv.example",
        };
        let body = serde_json::json!({ "space": "space" });
        let request = deliver(
            &pool,
            backend,
            Some(signer),
            NOTIFY_SPACE_DELETED_LXM,
            &body,
        )
        .await;
        let token =
            request.headers["authorization"].to_str().unwrap()["Bearer ".len()..].to_string();
        let claims: serde_json::Value = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(token.split('.').nth(1).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(claims["lxm"], NOTIFY_SPACE_DELETED_LXM);
    }

    #[tokio::test]
    async fn without_a_signer_the_payload_is_still_delivered() {
        let (pool, backend) = pool(true).await;
        let request = deliver(&pool, backend, None, NOTIFY_WRITE_LXM, &payload()).await;
        assert!(request.headers.get("authorization").is_none());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&request.body).unwrap(),
            payload()
        );
    }

    #[tokio::test]
    async fn a_delivery_that_cannot_be_signed_is_still_sent() {
        // No service identity, so there is no key to mint with.
        let (pool, backend) = pool(false).await;
        let keyring =
            crate::plugin::encryption::Keyring::from(crate::test_support::TEST_ENCRYPTION_KEY);
        let signer = WebhookSigner {
            encryption_key: &keyring,
            public_url: "https://hv.example",
        };
        let request = deliver(&pool, backend, Some(signer), NOTIFY_WRITE_LXM, &payload()).await;
        assert!(request.headers.get("authorization").is_none());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&request.body).unwrap(),
            payload()
        );
    }
}
