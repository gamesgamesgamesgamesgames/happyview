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
/// `#atproto` signing key, its public URL, which names its DID, and the PLC
/// directory, to resolve the registered service's DID document.
#[derive(Clone, Copy)]
pub struct WebhookSigner<'a> {
    pub encryption_key: &'a crate::plugin::encryption::Keyring,
    pub public_url: &'a str,
    pub plc_url: &'a str,
}

/// Whether the missing-key warning has been logged, so it is once a process.
static UNSIGNED_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// POST `body` to a webhook registration. When the registered service DID's
/// document lists a service endpoint on the same origin as the webhook URL,
/// the request carries service auth from this instance in
/// `Authorization: Bearer`, the same JWT a registration by identifier
/// receives: `aud` is that DID and `lxm` the method the payload stands for,
/// so a receiver can check the token was minted for it and for this kind of
/// notification. A legacy registration names its DID and its URL
/// separately, so a token addressed to a DID only goes to a URL that DID
/// publishes. The match is by origin, not full URL, so a DID that lists an
/// origin other parties also receive requests at (a shared, multi-tenant
/// host) has its tokens reach any path there. The body is unchanged, so a receiver that ignores the header
/// works as before. Without a signer, when the URL isn't one the DID lists,
/// or when minting fails, the payload is still sent, without the header.
async fn post_webhook(
    http: &reqwest::Client,
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    signer: Option<WebhookSigner<'_>>,
    reg: &NotifyRegistration,
    lxm: &str,
    body: &serde_json::Value,
) -> Result<reqwest::Response, reqwest::Error> {
    let signer = signer_for(http, signer, reg).await;
    let token = mint(pool, backend, signer, reg, lxm).await;
    send_webhook(http, reg, token.as_deref(), body).await
}

/// The signer to use for a registration's deliveries, or `None` to send them
/// without service auth: see [`post_webhook`]. This resolves the
/// registration's DID, so a batch asks once per registration; each post
/// still gets a token of its own from [`mint`], so a slow receiver never
/// sees one that expired mid-batch, or the same `jti` twice.
async fn signer_for<'a>(
    http: &reqwest::Client,
    signer: Option<WebhookSigner<'a>>,
    reg: &NotifyRegistration,
) -> Option<WebhookSigner<'a>> {
    let Some(signer) = signer else {
        if !UNSIGNED_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            tracing::warn!(
                "TOKEN_ENCRYPTION_KEY is unset, so webhook deliveries are sent without service auth"
            );
        }
        return None;
    };
    if crate::spaces::auth::did_lists_endpoint_origin(
        http,
        signer.plc_url,
        &reg.service,
        &reg.endpoint,
    )
    .await
    {
        Some(signer)
    } else {
        tracing::debug!(service = %reg.service, "webhook URL isn't listed by its service DID; sending it without service auth");
        None
    }
}

/// A fresh service-auth token for one delivery, or `None` when there is no
/// signer or minting fails.
async fn mint(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    signer: Option<WebhookSigner<'_>>,
    reg: &NotifyRegistration,
    lxm: &str,
) -> Option<String> {
    let signer = signer?;
    match crate::auth::service_auth::mint_service_auth(
        pool,
        backend,
        signer.encryption_key,
        signer.public_url,
        &reg.service,
        lxm,
    )
    .await
    {
        Ok(token) => Some(token),
        Err(e) => {
            tracing::warn!(service = %reg.service, error = %e, "could not sign a webhook delivery; sending it unsigned");
            None
        }
    }
}

async fn send_webhook(
    http: &reqwest::Client,
    reg: &NotifyRegistration,
    token: Option<&str>,
    body: &serde_json::Value,
) -> Result<reqwest::Response, reqwest::Error> {
    webhook_request(http, reg, token, body).send().await
}

fn webhook_request(
    http: &reqwest::Client,
    reg: &NotifyRegistration,
    token: Option<&str>,
    body: &serde_json::Value,
) -> reqwest::RequestBuilder {
    // A total bound, beyond the shared client's connect and read-inactivity
    // timeouts: a receiver that answers a byte at a time would otherwise
    // hold its space's delivery worker, and so every other webhook of the
    // space, for as long as it liked.
    let mut request = http
        .post(&reg.endpoint)
        .timeout(WEBHOOK_POST_TIMEOUT)
        .json(body);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    request
}

/// One record a legacy webhook hears about.
#[derive(Debug, Clone)]
pub struct WebhookWrite {
    pub collection: String,
    pub rkey: String,
    pub cid: Option<String>,
}

/// Send each write to every webhook registered for the space, in order.
/// The registrations are read once, and each one's DID is resolved once for
/// the whole batch; every post carries a token of its own.
pub async fn dispatch_write_notifications(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    http: &reqwest::Client,
    signer: Option<WebhookSigner<'_>>,
    space_id: &str,
    author_did: &str,
    writes: &[WebhookWrite],
) -> Result<(), AppError> {
    if writes.is_empty() {
        return Ok(());
    }
    let registrations = db::list_notify_registrations(pool, backend, space_id).await?;
    for reg in registrations
        .iter()
        .filter(|r| r.delivery == NotifyDelivery::Webhook)
    {
        let reg_signer = signer_for(http, signer, reg).await;
        for write in writes {
            let payload = serde_json::json!({
                "space": space_id,
                "did": author_did,
                "collection": write.collection,
                "rkey": write.rkey,
                "cid": write.cid,
            });
            let token = mint(pool, backend, reg_signer, reg, NOTIFY_WRITE_LXM).await;
            let _ = send_webhook(http, reg, token.as_deref(), &payload).await;
        }
    }
    Ok(())
}

/// How long one webhook POST may take in all.
const WEBHOOK_POST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How many writes may wait for one space's webhooks. Past that, a write's
/// webhook deliveries are dropped with a warning: they are best effort, and
/// a slow receiver mustn't hold an unbounded backlog.
const WEBHOOK_QUEUE: usize = 256;

/// How long a space's delivery worker waits for more writes before it stops.
const WEBHOOK_WORKER_IDLE: std::time::Duration = std::time::Duration::from_secs(60);

struct WebhookBatch {
    author_did: String,
    writes: Vec<WebhookWrite>,
}

type WebhookQueues =
    std::sync::Mutex<std::collections::HashMap<String, tokio::sync::mpsc::Sender<WebhookBatch>>>;

/// Each space's delivery worker, by space id.
fn webhook_queues() -> &'static WebhookQueues {
    static QUEUES: std::sync::OnceLock<WebhookQueues> = std::sync::OnceLock::new();
    QUEUES.get_or_init(Default::default)
}

/// Tell the space's legacy webhooks about `writes`, without the write waiting
/// on any receiver or on resolving a DID. Each space has one delivery worker,
/// so its webhooks hear about writes in the order they were announced, which
/// is the order they committed. The worker's queue is bounded: when it is
/// full the writes are dropped, with a warning. Delivery is best effort, as
/// it always was.
pub fn announce_writes(
    state: &crate::AppState,
    space_id: &str,
    author_did: &str,
    writes: Vec<WebhookWrite>,
) {
    if writes.is_empty() {
        return;
    }
    let mut batch = WebhookBatch {
        author_did: author_did.to_string(),
        writes,
    };
    // Sending under the lock means a worker that removes itself (also under
    // the lock) can't miss a batch: see `webhook_worker`.
    let mut queues = webhook_queues().lock().unwrap_or_else(|e| e.into_inner());
    for _ in 0..2 {
        let sender = queues
            .entry(space_id.to_string())
            .or_insert_with(|| spawn_webhook_worker(state, space_id));
        match sender.try_send(batch) {
            Ok(()) => return,
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!(
                    space_id,
                    "a space's webhook queue is full, so a write's webhook deliveries were dropped"
                );
                return;
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(back)) => {
                // Its worker stopped; start another and send again.
                queues.remove(space_id);
                batch = back;
            }
        }
    }
}

fn spawn_webhook_worker(
    state: &crate::AppState,
    space_id: &str,
) -> tokio::sync::mpsc::Sender<WebhookBatch> {
    let (tx, rx) = tokio::sync::mpsc::channel(WEBHOOK_QUEUE);
    tokio::spawn(webhook_worker(state.clone(), space_id.to_string(), rx));
    tx
}

async fn webhook_worker(
    state: crate::AppState,
    space_id: String,
    mut rx: tokio::sync::mpsc::Receiver<WebhookBatch>,
) {
    loop {
        let batch = match tokio::time::timeout(WEBHOOK_WORKER_IDLE, rx.recv()).await {
            Ok(Some(batch)) => batch,
            Ok(None) => return,
            Err(_idle) => {
                // Stop only if nothing arrived meanwhile: senders send under
                // the same lock, so after this nothing reaches `rx`.
                let mut queues = webhook_queues().lock().unwrap_or_else(|e| e.into_inner());
                if rx.is_empty() {
                    queues.remove(&space_id);
                    return;
                }
                continue;
            }
        };
        let keyring = state.config.keyring();
        let signer = keyring.as_ref().map(|encryption_key| WebhookSigner {
            encryption_key,
            public_url: &state.config.public_url,
            plc_url: &state.config.plc_url,
        });
        if let Err(e) = dispatch_write_notifications(
            &state.db,
            state.db_backend,
            &state.http,
            signer,
            &space_id,
            &batch.author_did,
            &batch.writes,
        )
        .await
        {
            tracing::warn!(space_id, error = %e, "could not notify a space's webhooks of a write");
        }
    }
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
                        plc_url: &state.config.plc_url,
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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const INSTANCE_KEY: [u8; 32] = [0x33; 32];
    const SERVICE: &str = "did:plc:syncer";

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

    /// A PLC directory whose document for `SERVICE` lists `listed` as its
    /// syncer service endpoint.
    async fn plc_listing(listed: &str) -> MockServer {
        let plc = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/{SERVICE}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": SERVICE,
                "service": [{
                    "id": "#atproto_space_syncer",
                    "type": "AtprotoSpaceSyncer",
                    "serviceEndpoint": listed,
                }],
            })))
            .mount(&plc)
            .await;
        plc
    }

    struct Delivery {
        receiver: MockServer,
    }

    impl Delivery {
        async fn new() -> Self {
            let receiver = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(200))
                .expect(1)
                .mount(&receiver)
                .await;
            Self { receiver }
        }

        /// Deliver `body` to this receiver, registered for `SERVICE`, and
        /// answer the one request it saw.
        async fn send(
            &self,
            pool: &sqlx::AnyPool,
            backend: DatabaseBackend,
            signer: Option<WebhookSigner<'_>>,
            lxm: &str,
            body: &serde_json::Value,
        ) -> wiremock::Request {
            let reg = NotifyRegistration {
                id: "reg".into(),
                space_id: "space".into(),
                service: SERVICE.into(),
                endpoint: format!("{}/hooks/space", self.receiver.uri()),
                registered_by: "did:plc:member".into(),
                expires_at: "2999-01-01T00:00:00+00:00".into(),
                created_at: "2026-01-01T00:00:00+00:00".into(),
                delivery: NotifyDelivery::Webhook,
            };
            post_webhook(
                &reqwest::Client::new(),
                pool,
                backend,
                signer,
                &reg,
                lxm,
                body,
            )
            .await
            .expect("delivered");
            self.receiver.received_requests().await.unwrap().remove(0)
        }
    }

    fn keyring() -> crate::plugin::encryption::Keyring {
        crate::plugin::encryption::Keyring::from(crate::test_support::TEST_ENCRYPTION_KEY)
    }

    fn signer<'a>(
        keyring: &'a crate::plugin::encryption::Keyring,
        plc: &'a str,
    ) -> WebhookSigner<'a> {
        WebhookSigner {
            encryption_key: keyring,
            public_url: "https://hv.example",
            plc_url: plc,
        }
    }

    fn payload() -> serde_json::Value {
        serde_json::json!({
            "space": "space", "did": "did:plc:alice", "collection": "com.example.post",
            "rkey": "rk", "cid": null,
        })
    }

    fn claims(request: &wiremock::Request) -> (serde_json::Value, Vec<String>) {
        let auth = request.headers.get("authorization").expect("signed");
        let token = auth.to_str().unwrap().strip_prefix("Bearer ").unwrap();
        let parts: Vec<String> = token.split('.').map(str::to_string).collect();
        let claims = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(&parts[1]).unwrap()).unwrap();
        (claims, parts)
    }

    /// A batch of writes reaches each webhook in order, with one DID lookup
    /// for the whole batch, and a token of its own for each post.
    #[tokio::test]
    async fn a_batch_of_writes_resolves_once_and_signs_each_post() {
        use crate::spaces::types::{AppAccess, Policy, Space, SpaceConfig};
        let (pool, backend) = pool(true).await;
        let now = crate::db::now_rfc3339();
        crate::spaces::db::create_space(
            &pool,
            backend,
            &Space {
                id: "space".into(),
                did: "did:plc:instance".into(),
                authority_did: "did:plc:instance".into(),
                creator_did: "did:plc:instance".into(),
                type_nsid: "com.example.forum".into(),
                skey: "main".into(),
                display_name: None,
                description: None,
                read_policy: Policy::MemberList,
                write_policy: Policy::MemberList,
                app_access: AppAccess::Open,
                config: SpaceConfig::default(),
                revision: None,
                created_at: now.clone(),
                updated_at: now,
            },
        )
        .await
        .unwrap();

        let receiver = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(3)
            .mount(&receiver)
            .await;
        let plc = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/{SERVICE}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": SERVICE,
                "service": [{ "id": "#s", "type": "S", "serviceEndpoint": receiver.uri() }],
            })))
            .expect(1)
            .mount(&plc)
            .await;
        register(
            &pool,
            backend,
            "space",
            SERVICE,
            &format!("{}/hook", receiver.uri()),
            "did:plc:member",
            NotifyDelivery::Webhook,
        )
        .await
        .unwrap();

        let writes: Vec<WebhookWrite> = ["a", "b", "c"]
            .iter()
            .map(|rkey| WebhookWrite {
                collection: "com.example.post".into(),
                rkey: rkey.to_string(),
                cid: None,
            })
            .collect();
        let keyring = keyring();
        dispatch_write_notifications(
            &pool,
            backend,
            &reqwest::Client::new(),
            Some(signer(&keyring, &plc.uri())),
            "space",
            "did:plc:alice",
            &writes,
        )
        .await
        .unwrap();

        let got = receiver.received_requests().await.unwrap();
        let rkeys: Vec<String> = got
            .iter()
            .map(|r| {
                serde_json::from_slice::<serde_json::Value>(&r.body).unwrap()["rkey"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(rkeys, ["a", "b", "c"], "in order");
        let tokens: std::collections::BTreeSet<_> = got
            .iter()
            .map(|r| r.headers["authorization"].to_str().unwrap().to_string())
            .collect();
        assert_eq!(tokens.len(), 3, "a fresh token for each post");
        // The PLC mock's expect(1) is checked when it drops.
    }

    /// A put then a delete of one record reach the webhook in that order,
    /// even when the put waits behind a slow delivery earlier in its own
    /// write: each space has one delivery worker, so a later write never
    /// overtakes an earlier one.
    #[tokio::test]
    async fn writes_reach_a_webhook_in_the_order_they_were_announced() {
        use crate::spaces::types::{AppAccess, Policy, Space, SpaceConfig};
        let (pool, backend) = pool(false).await;
        let space_id = format!("space-{}", uuid::Uuid::new_v4());
        let now = crate::db::now_rfc3339();
        crate::spaces::db::create_space(
            &pool,
            backend,
            &Space {
                id: space_id.clone(),
                did: "did:plc:instance".into(),
                authority_did: "did:plc:instance".into(),
                creator_did: "did:plc:instance".into(),
                type_nsid: "com.example.forum".into(),
                skey: space_id.clone(),
                display_name: None,
                description: None,
                read_policy: Policy::MemberList,
                write_policy: Policy::MemberList,
                app_access: AppAccess::Open,
                config: SpaceConfig::default(),
                revision: None,
                created_at: now.clone(),
                updated_at: now,
            },
        )
        .await
        .unwrap();
        let receiver = MockServer::start().await;
        // The write's first record is answered slowly; everything else at once.
        Mock::given(method("POST"))
            .and(wiremock::matchers::body_partial_json(
                serde_json::json!({ "cid": "bafyslow" }),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_delay(std::time::Duration::from_millis(400)),
            )
            .mount(&receiver)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&receiver)
            .await;
        register(
            &pool,
            backend,
            &space_id,
            SERVICE,
            &format!("{}/hook", receiver.uri()),
            "did:plc:member",
            NotifyDelivery::Webhook,
        )
        .await
        .unwrap();

        let mut config = crate::test_support::test_config(backend);
        config.token_encryption_key = None;
        let state = crate::state::state_with_pool_on(config, pool, backend);
        let record = |rkey: &str, cid: Option<&str>| WebhookWrite {
            collection: "com.example.post".into(),
            rkey: rkey.into(),
            cid: cid.map(str::to_string),
        };
        // One write touching two records, the second a put of `rk`; then a
        // write deleting `rk`.
        announce_writes(
            &state,
            &space_id,
            "did:plc:alice",
            vec![
                record("other", Some("bafyslow")),
                record("rk", Some("bafyput")),
            ],
        );
        announce_writes(&state, &space_id, "did:plc:alice", vec![record("rk", None)]);

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let got = loop {
            let got = receiver.received_requests().await.unwrap();
            if got.len() >= 3 || tokio::time::Instant::now() > deadline {
                break got;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        let cids: Vec<serde_json::Value> = got
            .iter()
            .map(|r| serde_json::from_slice::<serde_json::Value>(&r.body).unwrap()["cid"].clone())
            .collect();
        assert_eq!(
            cids,
            [
                serde_json::json!("bafyslow"),
                serde_json::json!("bafyput"),
                serde_json::Value::Null
            ],
            "the delete must not overtake the put"
        );
    }

    #[test]
    fn a_webhook_post_has_a_total_time_limit() {
        let reg = NotifyRegistration {
            id: "reg".into(),
            space_id: "space".into(),
            service: SERVICE.into(),
            endpoint: "https://receiver.example/hook".into(),
            registered_by: "did:plc:member".into(),
            expires_at: "2999-01-01T00:00:00+00:00".into(),
            created_at: "2026-01-01T00:00:00+00:00".into(),
            delivery: NotifyDelivery::Webhook,
        };
        let request = webhook_request(&reqwest::Client::new(), &reg, None, &payload())
            .build()
            .unwrap();
        assert_eq!(request.timeout(), Some(&WEBHOOK_POST_TIMEOUT));
    }

    #[tokio::test]
    async fn a_webhook_the_service_lists_carries_service_auth_for_it() {
        let (pool, backend) = pool(true).await;
        let delivery = Delivery::new().await;
        let plc = plc_listing(&delivery.receiver.uri()).await;
        let keyring = keyring();
        let request = delivery
            .send(
                &pool,
                backend,
                Some(signer(&keyring, &plc.uri())),
                NOTIFY_WRITE_LXM,
                &payload(),
            )
            .await;

        // The body is the payload as before.
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body, payload());

        let (claims, parts) = claims(&request);
        assert_eq!(claims["iss"], "did:plc:instance");
        assert_eq!(claims["aud"], SERVICE);
        assert_eq!(claims["lxm"], NOTIFY_WRITE_LXM);

        // It verifies against the instance's #atproto key.
        let verifying = *p256::ecdsa::SigningKey::from_slice(&INSTANCE_KEY)
            .unwrap()
            .verifying_key();
        let sig = p256::ecdsa::Signature::from_slice(&URL_SAFE_NO_PAD.decode(&parts[2]).unwrap())
            .unwrap();
        verifying
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
            .expect("the receiver can verify it against the instance's DID document key");
    }

    #[tokio::test]
    async fn a_space_deletion_webhook_is_bound_to_its_own_method() {
        let (pool, backend) = pool(true).await;
        let delivery = Delivery::new().await;
        let plc = plc_listing(&delivery.receiver.uri()).await;
        let keyring = keyring();
        let body = serde_json::json!({ "space": "space" });
        let request = delivery
            .send(
                &pool,
                backend,
                Some(signer(&keyring, &plc.uri())),
                NOTIFY_SPACE_DELETED_LXM,
                &body,
            )
            .await;
        assert_eq!(claims(&request).0["lxm"], NOTIFY_SPACE_DELETED_LXM);
    }

    /// The registered DID publishes a different host than the webhook URL,
    /// so the two belong to different parties: the payload goes, without a
    /// token addressed to that DID.
    #[tokio::test]
    async fn a_webhook_url_its_service_does_not_list_is_sent_without_service_auth() {
        let (pool, backend) = pool(true).await;
        let delivery = Delivery::new().await;
        let plc = plc_listing("https://syncer.example").await;
        let keyring = keyring();
        let request = delivery
            .send(
                &pool,
                backend,
                Some(signer(&keyring, &plc.uri())),
                NOTIFY_WRITE_LXM,
                &payload(),
            )
            .await;
        assert!(request.headers.get("authorization").is_none());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&request.body).unwrap(),
            payload()
        );
    }

    #[tokio::test]
    async fn a_service_that_does_not_resolve_gets_no_service_auth() {
        let (pool, backend) = pool(true).await;
        let delivery = Delivery::new().await;
        // A PLC directory that knows no DIDs.
        let plc = MockServer::start().await;
        let keyring = keyring();
        let request = delivery
            .send(
                &pool,
                backend,
                Some(signer(&keyring, &plc.uri())),
                NOTIFY_WRITE_LXM,
                &payload(),
            )
            .await;
        assert!(request.headers.get("authorization").is_none());
    }

    #[tokio::test]
    async fn without_a_signer_the_payload_is_still_delivered() {
        let (pool, backend) = pool(true).await;
        let delivery = Delivery::new().await;
        let request = delivery
            .send(&pool, backend, None, NOTIFY_WRITE_LXM, &payload())
            .await;
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
        let delivery = Delivery::new().await;
        let plc = plc_listing(&delivery.receiver.uri()).await;
        let keyring = keyring();
        let request = delivery
            .send(
                &pool,
                backend,
                Some(signer(&keyring, &plc.uri())),
                NOTIFY_WRITE_LXM,
                &payload(),
            )
            .await;
        assert!(request.headers.get("authorization").is_none());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&request.body).unwrap(),
            payload()
        );
    }
}
