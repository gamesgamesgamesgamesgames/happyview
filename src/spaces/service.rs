use crate::AppState;
use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::spaces::host_mode::HostMode;
use crate::spaces::lthash::LtHashState;
use crate::spaces::types::*;
use crate::spaces::{SpaceUri, commit, db, lthash, members, notifications, oplog};
use crate::tid::generate_tid;
use sha2::{Digest, Sha256};

pub(crate) async fn resolve_space(state: &AppState, space_ref: &str) -> Result<Space, AppError> {
    let uri = SpaceUri::parse(space_ref)?;
    db::get_space_by_address(
        &state.db,
        state.db_backend,
        &uri.did,
        &uri.type_nsid,
        &uri.skey,
    )
    .await?
    .ok_or_else(|| AppError::NotFound("Space not found".into()))
}

pub(crate) fn content_cid(record: &serde_json::Value) -> Result<String, AppError> {
    crate::cid_verify::compute_record_cid(record)
        .map(|cid| cid.to_string())
        .ok_or_else(|| {
            AppError::BadRequest(
                "record cannot be encoded as DAG-CBOR (non-finite number, or malformed $link/$bytes)"
                    .into(),
            )
        })
}

pub(crate) async fn require_space_admin(
    state: &AppState,
    space: &Space,
    did: &str,
) -> Result<(), AppError> {
    if space.creator_did == did || is_super_admin(state, did).await? {
        return Ok(());
    }
    Err(AppError::Forbidden(
        "Only the space's creator can perform this action".into(),
    ))
}

/// Whether `did` is a HappyView super admin.
pub(crate) async fn is_super_admin(state: &AppState, did: &str) -> Result<bool, AppError> {
    let sql = adapt_sql(
        "SELECT is_super FROM happyview_users WHERE did = ?",
        state.db_backend,
    );
    let row: Option<(i32,)> = crate::db::query_as(&sql)
        .bind(did)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to check admin status: {e}")))?;
    Ok(row.is_some_and(|(is_super,)| is_super != 0))
}

/// Verify a space credential this instance issued for `space`, returning its
/// claims.
///
/// Verified against the local key rather than through the authority's DID
/// document: this instance is the space host, and the per-space keys of spaces
/// anchored on their creator's DID are published nowhere.
pub(crate) async fn verify_space_credential(
    state: &AppState,
    space: &Space,
    token: &str,
) -> Result<crate::spaces::credential::SpaceCredentialClaims, AppError> {
    let encryption_key = &state.config.keyring().ok_or_else(|| {
        AppError::Internal("TOKEN_ENCRYPTION_KEY is required for space credentials".into())
    })?;
    let key = crate::spaces::auth::space_verifying_key(
        &state.db,
        state.db_backend,
        encryption_key,
        &state.config.public_url,
        space,
    )
    .await?;
    let claims = crate::spaces::credential::verify_credential_with_key(token, &key)?;

    let space_uri = format!(
        "at://{}/space/{}/{}",
        space.did, space.type_nsid, space.skey
    );
    if claims.sub != space_uri {
        return Err(AppError::XrpcError {
            status: axum::http::StatusCode::BAD_REQUEST,
            code: "InvalidCredential",
            message: "space credential is for a different space".into(),
        });
    }
    // A space recreated with its type and key keeps its DID, its key and its
    // address, so a credential minted for the deleted space would verify. It
    // predates this space, and is refused. Compared in whole seconds, as
    // `iat` is.
    let created = chrono::DateTime::parse_from_rfc3339(&space.created_at)
        .map(|t| t.timestamp())
        .unwrap_or(0);
    if (claims.iat as i64) < created {
        return Err(AppError::XrpcError {
            status: axum::http::StatusCode::UNAUTHORIZED,
            code: "InvalidCredential",
            message: "space credential was issued before this space was created".into(),
        });
    }
    if db::is_space_credential_jti_revoked(&state.db, state.db_backend, &space.id, &claims.jti)
        .await?
    {
        return Err(AppError::XrpcError {
            status: axum::http::StatusCode::UNAUTHORIZED,
            code: "CredentialRevoked",
            message: "space credential has been revoked".into(),
        });
    }
    Ok(claims)
}

pub(crate) async fn require_membership(
    state: &AppState,
    space: &Space,
    did: &str,
    require_write: bool,
    space_credential: Option<&str>,
) -> Result<MemberAccess, AppError> {
    if let Some(token) = space_credential {
        verify_space_credential(state, space, token).await?;
        if require_write {
            return Err(AppError::Forbidden(
                "Write access is required for this action".into(),
            ));
        }
        return Ok(MemberAccess::READ);
    }
    // The creator stands in for the space's authority, which no one signs in
    // as, so it keeps access whatever the member list holds, as in
    // `auth::policy_allows`.
    if did == space.creator_did {
        return Ok(MemberAccess::WRITE);
    }
    let access = members::is_member(&state.db, state.db_backend, &space.id, did)
        .await?
        .ok_or_else(|| AppError::Forbidden("You are not a member of this space".into()))?;
    if require_write && !access.can_write() {
        return Err(AppError::Forbidden(
            "Write access is required for this action".into(),
        ));
    }
    Ok(access)
}

/// Fetch a space blob from its author's PDS, with the content type the PDS
/// reported.
///
/// Public only so `tests/test_runner_identity.rs` can show it refuses a
/// forged PDS before any request; the routes are its real callers.
#[doc(hidden)]
pub async fn fetch_space_blob(
    state: &AppState,
    author_did: &str,
    cid: &str,
) -> Result<
    (
        axum::http::StatusCode,
        axum::http::HeaderMap,
        axum::body::Bytes,
    ),
    AppError,
> {
    let pds_endpoint =
        crate::profile::resolve_pds_endpoint(&state.http, &state.config.plc_url, author_did)
            .await?;
    crate::testing::identity::refuse_forged_pds(&pds_endpoint, "fetching a space blob").map_err(
        |message| AppError::XrpcError {
            status: axum::http::StatusCode::NOT_IMPLEMENTED,
            code: "MethodNotImplemented",
            message,
        },
    )?;

    let url = format!(
        "{}/xrpc/com.atproto.sync.getBlob?did={}&cid={}",
        pds_endpoint,
        urlencoding::encode(author_did),
        urlencoding::encode(cid),
    );

    let resp = state
        .http
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::BadGateway(format!("blob fetch failed: {e}")))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(AppError::BadGateway(format!(
            "PDS returned {status} for blob cid={cid}"
        )));
    }

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| AppError::BadGateway(format!("failed to read blob body: {e}")))?;

    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        content_type
            .parse()
            .unwrap_or_else(|_| "application/octet-stream".parse().unwrap()),
    );

    Ok((status, headers, bytes))
}

pub(crate) struct AppliedOp {
    pub action: OplogAction,
    pub collection: String,
    pub rkey: String,
    pub new_cid: Option<String>,
    pub old_cid: Option<String>,
}

// HappyView signs polyfilled commits with its own `#atproto_space` key.
//
// In polyfill mode HappyView is the repo host but holds no per-user signing
// key, so it cannot produce a signature that verifies against the author's DID
// document. Polyfilled repos are therefore host-attested, not author-attested.
// In native mode the user's PDS signs commits, so this path applies only to
// polyfill mode.

/// Provision the `#atproto_space` key if absent, then load it.
///
/// Writes, so it belongs on cold paths only: startup, space creation, and the
/// offline backfill binaries. The write path uses [`service_signing_key`].
pub async fn signing_key_from_pool(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    encryption_key: &crate::plugin::encryption::Keyring,
) -> Result<p256::ecdsa::SigningKey, AppError> {
    crate::verification_methods::ensure_atproto_space_method(
        pool,
        backend,
        encryption_key.current(),
    )
    .await?;
    load_signing_key(pool, backend, encryption_key).await
}

/// Load the `#atproto_space` key. Read-only.
pub async fn load_signing_key(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    encryption_key: &crate::plugin::encryption::Keyring,
) -> Result<p256::ecdsa::SigningKey, AppError> {
    let key_bytes = crate::verification_methods::get_private_key_bytes(
        pool,
        backend,
        "#atproto_space",
        encryption_key,
    )
    .await?
    .ok_or_else(|| {
        AppError::Internal(
            "#atproto_space signing key is missing; it is provisioned at startup and on space creation"
                .into(),
        )
    })?;

    crate::verification_methods::private_key_bytes_to_signing_key(&key_bytes)
}

/// The signing key for a space write.
///
/// Read-only because this runs on every write: provisioning here would issue an
/// INSERT per write, which wastes a round trip and contends with the caller's
/// own transaction. Provisioning happens on the cold paths, startup and
/// `create_space`.
///
/// Call it before opening the write transaction. It queries through the pool,
/// not the transaction's connection, and on SQLite a second connection used
/// while a write transaction is open can deadlock.
pub(crate) async fn service_signing_key(
    state: &AppState,
) -> Result<p256::ecdsa::SigningKey, AppError> {
    let encryption_key = &state.config.keyring().ok_or_else(|| {
        AppError::Internal(
            "spaces require a token encryption key to sign commits; set TOKEN_ENCRYPTION_KEY"
                .into(),
        )
    })?;
    load_signing_key(&state.db, state.db_backend, encryption_key).await
}

/// Begin a write to one space, holding the space for the whole transaction.
///
/// Every write reads its author's repo state and folds into it, so writes to
/// a space have to land one at a time. SQLite's `BEGIN IMMEDIATE` already
/// does that. Postgres runs them in parallel, so the space row is locked
/// first: taken any later, after an insert's foreign key has share-locked the
/// same row, two writers each wait on the other's share lock and deadlock.
pub(crate) async fn begin_space_write(
    state: &AppState,
    space_id: &str,
) -> Result<sqlx::Transaction<'static, sqlx::Any>, AppError> {
    let mut tx = crate::db::begin_write(&state.db, state.db_backend)
        .await
        .map_err(|e| AppError::Internal(format!("failed to begin transaction: {e}")))?;
    if state.db_backend == DatabaseBackend::Postgres {
        crate::db::query("SELECT id FROM happyview_spaces WHERE id = $1 FOR UPDATE")
            .bind(space_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| AppError::Internal(format!("failed to lock space: {e}")))?;
    }
    Ok(tx)
}

pub(crate) async fn commit_write(
    conn: &mut sqlx::AnyConnection,
    backend: DatabaseBackend,
    space: &Space,
    author_did: &str,
    rev: &str,
    ops: &[AppliedOp],
    signing_key: &p256::ecdsa::SigningKey,
) -> Result<CommittedWrite, AppError> {
    let mut repo_state =
        db::get_or_create_repo_state(&mut *conn, backend, &space.id, author_did).await?;

    let lthash_bytes: [u8; 2048] = repo_state.lthash_state.as_slice().try_into().map_err(|_| {
        AppError::Internal(format!(
            "corrupt repo state: lthash is {} bytes, expected 2048",
            repo_state.lthash_state.len()
        ))
    })?;
    let mut set_hash = LtHashState::from_bytes(lthash_bytes);

    let now = now_rfc3339();
    for (idx, op) in ops.iter().enumerate() {
        if let Some(old) = op.old_cid.as_deref() {
            set_hash.remove(&lthash::record_element(&op.collection, &op.rkey, old));
        }
        if let Some(new) = op.new_cid.as_deref() {
            set_hash.add(&lthash::record_element(&op.collection, &op.rkey, new));
        }

        let entry = OplogEntry {
            id: uuid::Uuid::new_v4().to_string(),
            space_id: space.id.clone(),
            author_did: author_did.to_string(),
            rev: rev.to_string(),
            idx: idx as i32,
            action: op.action,
            collection: op.collection.clone(),
            rkey: op.rkey.clone(),
            cid: op.new_cid.clone(),
            prev: op.old_cid.clone(),
            value: None,
            created_at: now.clone(),
        };
        oplog::append_op(&mut *conn, backend, &entry).await?;
    }

    let space_uri = format!(
        "at://{}/space/{}/{}",
        space.did, space.type_nsid, space.skey
    );
    let signed = commit::sign_commit(&set_hash.hash(), &space_uri, author_did, rev, signing_key)?;

    repo_state.lthash_state = set_hash.as_bytes().to_vec();
    repo_state.rev = Some(signed.rev);
    let hash = signed.hash.to_vec();
    repo_state.hash = Some(hash.clone());
    repo_state.ikm = Some(signed.ikm.to_vec());
    repo_state.sig = Some(signed.sig);
    repo_state.mac = Some(signed.mac.to_vec());
    db::update_repo_state(&mut *conn, backend, &repo_state).await?;

    db::update_space_revision(&mut *conn, backend, &space.id, rev).await?;

    let recorded =
        crate::spaces::writers::record(&mut *conn, backend, &space.id, author_did, rev, &hash)
            .await?;

    Ok(CommittedWrite { hash, recorded })
}

/// What a committed write reports to syncers.
pub(crate) struct CommittedWrite {
    pub hash: Vec<u8>,
    pub recorded: crate::spaces::writers::Recorded,
}

/// Tell syncers about a committed write: once for the commit to services
/// registered by identifier, and once per op to legacy webhooks.
pub(crate) async fn notify_ops(
    state: &AppState,
    space: &Space,
    author_did: &str,
    ops: &[AppliedOp],
    rev: &str,
    committed: CommittedWrite,
) {
    if let crate::spaces::writers::Recorded::Advanced {
        space_rev,
        prev_space_rev,
    } = committed.recorded
    {
        notifications::forward_repo_update(
            state,
            &space.id,
            notifications::RepoUpdate {
                space_uri: format!(
                    "at://{}/space/{}/{}",
                    space.did, space.type_nsid, space.skey
                ),
                repo: author_did.to_string(),
                rev: rev.to_string(),
                hash: committed.hash,
                space_rev,
                prev_space_rev,
            },
        );
    }

    notifications::announce_writes(
        state,
        &space.id,
        author_did,
        ops.iter()
            .map(|op| notifications::WebhookWrite {
                collection: op.collection.clone(),
                rkey: op.rkey.clone(),
                cid: op.new_cid.clone(),
            })
            .collect(),
    );
}

/// Whether this repo's writes still belong to HappyView, and if not, forward them.
///
/// The AppView does not belong in the write path:
/// `createRecord`/`putRecord`/`deleteRecord`/`applyWrites` are `pds`-role
/// methods, and a client holding the user's session should write to their PDS
/// directly while HappyView only indexes.
///
/// This bridge is deprecated. It keeps existing clients working once their
/// repos migrate. Remove it once clients write to PDSes directly.
pub(crate) async fn forward_write_if_native(
    state: &AppState,
    space: &Space,
    author_did: &str,
    method: &str,
    body: serde_json::Value,
) -> Result<Option<serde_json::Value>, AppError> {
    let mut conn = state
        .db
        .acquire()
        .await
        .map_err(|e| AppError::Internal(format!("failed to acquire connection: {e}")))?;
    // Read only. Creating the row here, outside the write transaction and
    // its lock on the space, would deadlock on Postgres against a write
    // already holding that lock. A missing row is created as polyfill by the
    // write itself.
    let host_mode = db::read_repo_state(&mut conn, state.db_backend, &space.id, author_did)
        .await?
        .map_or(HostMode::Polyfill, |repo_state| repo_state.host_mode);
    drop(conn);

    if host_mode.is_authoritative_here() {
        return Ok(None);
    }

    // The in-process PDS answers repo writes only, so re-authenticating, which
    // is the advice below, could never help here.
    if state.test.is_some() {
        return Err(AppError::XrpcError {
            status: axum::http::StatusCode::NOT_IMPLEMENTED,
            code: "MethodNotImplemented",
            message: format!(
                "this space lives on {author_did}'s PDS, and a test run serves no \
                 PDS-backed spaces; {method} reaches only spaces HappyView hosts"
            ),
        });
    }

    tracing::warn!(
        space_id = %space.id,
        author_did,
        method,
        "forwarding a space write to the user's PDS; this bridge is deprecated and \
         clients should write to the PDS directly"
    );

    // Without a usable session the write cannot reach the PDS, which is the
    // source of truth. Writing locally instead would fork the two copies, and
    // the next sync would drop the record.
    let session = crate::repo::get_oauth_session(state, author_did)
        .await
        .map_err(|e| {
            AppError::Forbidden(format!(
                "this space lives on your PDS and HappyView cannot write to it for you; \
             re-authenticate to continue writing ({e})"
            ))
        })?;

    let resp = crate::repo::pds::pds_post_json_raw(state, &session, method, &body).await?;
    if !resp.status().is_success() {
        let detail = resp.text().await.unwrap_or_default();
        return Err(AppError::BadGateway(format!(
            "the user's PDS rejected {method}: {detail}"
        )));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| AppError::Internal(format!("failed to read PDS response: {e}")))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| AppError::Internal(format!("PDS returned invalid JSON: {e}")))?;
    Ok(Some(value))
}

/// Add `swapRecord` to a forwarded write only when the caller gave one.
///
/// `"swapRecord": null` is not the same as leaving it out: it asks the PDS to
/// write only if no record exists, so an unconditional update would fail.
fn with_swap_record(mut body: serde_json::Value, swap_cid: Option<&str>) -> serde_json::Value {
    if let Some(swap) = swap_cid {
        body["swapRecord"] = serde_json::json!(swap);
    }
    body
}

pub(crate) async fn create_record(
    state: &AppState,
    did: &str,
    space_credential: Option<&str>,
    space_ref: &str,
    collection: &str,
    record: serde_json::Value,
) -> Result<(String, String), AppError> {
    let space = resolve_space(state, space_ref).await?;
    require_membership(state, &space, did, true, space_credential).await?;

    let space_uri = format!(
        "at://{}/space/{}/{}",
        space.did, space.type_nsid, space.skey
    );
    if let Some(forwarded) = forward_write_if_native(
        state,
        &space,
        did,
        "com.atproto.space.createRecord",
        serde_json::json!({
            "space": space_uri,
            "repo": did,
            "collection": collection,
            "record": record,
        }),
    )
    .await?
    {
        let uri = forwarded
            .get("uri")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let cid = forwarded
            .get("cid")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        return Ok((uri, cid));
    }

    let rkey = generate_tid();
    let cid = content_cid(&record)?;
    let record_uri = format!(
        "at://{}/space/{}/{}/{}/{}/{}",
        space.did, space.type_nsid, space.skey, did, collection, rkey
    );
    let rec = SpaceRecord {
        uri: record_uri.clone(),
        space_id: space.id.clone(),
        author_did: did.to_string(),
        collection: collection.to_string(),
        rkey,
        record,
        cid: cid.clone(),
        indexed_at: now_rfc3339(),
    };
    let rev = generate_tid();
    let ops = vec![AppliedOp {
        action: OplogAction::Create,
        collection: collection.to_string(),
        rkey: rec.rkey.clone(),
        new_cid: Some(cid.clone()),
        old_cid: None,
    }];

    // Before the transaction; see `service_signing_key`.
    let signing_key = service_signing_key(state).await?;

    let mut tx = begin_space_write(state, &space.id).await?;
    db::insert_space_record(&mut *tx, state.db_backend, &rec).await?;
    let committed = commit_write(
        &mut tx,
        state.db_backend,
        &space,
        did,
        &rev,
        &ops,
        &signing_key,
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("failed to commit transaction: {e}")))?;

    notify_ops(state, &space, did, &ops, &rev, committed).await;

    Ok((record_uri, cid))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn put_record(
    state: &AppState,
    did: &str,
    space_credential: Option<&str>,
    space_ref: &str,
    collection: &str,
    rkey: &str,
    record: serde_json::Value,
    swap_cid: Option<String>,
) -> Result<(String, String), AppError> {
    let space = resolve_space(state, space_ref).await?;
    require_membership(state, &space, did, true, space_credential).await?;

    let space_uri = format!(
        "at://{}/space/{}/{}",
        space.did, space.type_nsid, space.skey
    );
    if let Some(forwarded) = forward_write_if_native(
        state,
        &space,
        did,
        "com.atproto.space.putRecord",
        with_swap_record(
            serde_json::json!({
                "space": space_uri,
                "repo": did,
                "collection": collection,
                "rkey": rkey,
                "record": record,
            }),
            swap_cid.as_deref(),
        ),
    )
    .await?
    {
        let uri = forwarded
            .get("uri")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let cid = forwarded
            .get("cid")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        return Ok((uri, cid));
    }

    let cid = content_cid(&record)?;
    let record_uri = format!(
        "at://{}/space/{}/{}/{}/{}/{}",
        space.did, space.type_nsid, space.skey, did, collection, rkey
    );
    let rec = SpaceRecord {
        uri: record_uri.clone(),
        space_id: space.id.clone(),
        author_did: did.to_string(),
        collection: collection.to_string(),
        rkey: rkey.to_string(),
        record,
        cid: cid.clone(),
        indexed_at: now_rfc3339(),
    };
    let rev = generate_tid();
    // Before the transaction; see `service_signing_key`.
    let signing_key = service_signing_key(state).await?;

    let mut tx = begin_space_write(state, &space.id).await?;

    let old_cid = match swap_cid.as_deref() {
        Some(swap) => Some(swap.to_string()),
        None => db::get_space_record(&mut *tx, state.db_backend, &record_uri)
            .await?
            .map(|r| r.cid),
    };
    let action = if old_cid.is_some() {
        OplogAction::Update
    } else {
        OplogAction::Create
    };

    if let Some(swap) = swap_cid {
        db::upsert_space_record_with_swap(&mut tx, state.db_backend, &rec, &swap).await?;
    } else {
        db::upsert_space_record(&mut *tx, state.db_backend, &rec).await?;
    }

    let ops = vec![AppliedOp {
        action,
        collection: collection.to_string(),
        rkey: rkey.to_string(),
        new_cid: Some(cid.clone()),
        old_cid,
    }];
    let committed = commit_write(
        &mut tx,
        state.db_backend,
        &space,
        did,
        &rev,
        &ops,
        &signing_key,
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("failed to commit transaction: {e}")))?;

    notify_ops(state, &space, did, &ops, &rev, committed).await;

    Ok((record_uri, cid))
}

pub(crate) async fn delete_record(
    state: &AppState,
    did: &str,
    space_ref: &str,
    collection: &str,
    rkey: &str,
    swap_cid: Option<String>,
) -> Result<(), AppError> {
    let space = resolve_space(state, space_ref).await?;
    require_membership(state, &space, did, true, None).await?;

    let space_uri = format!(
        "at://{}/space/{}/{}",
        space.did, space.type_nsid, space.skey
    );
    if forward_write_if_native(
        state,
        &space,
        did,
        "com.atproto.space.deleteRecord",
        with_swap_record(
            serde_json::json!({
                "space": space_uri,
                "repo": did,
                "collection": collection,
                "rkey": rkey,
            }),
            swap_cid.as_deref(),
        ),
    )
    .await?
    .is_some()
    {
        return Ok(());
    }

    let record_uri = format!(
        "at://{}/space/{}/{}/{}/{}/{}",
        space.did, space.type_nsid, space.skey, did, collection, rkey
    );
    let rev = generate_tid();
    // Before the transaction; see `service_signing_key`.
    let signing_key = service_signing_key(state).await?;

    let mut tx = begin_space_write(state, &space.id).await?;

    let old_cid = if let Some(swap) = swap_cid {
        db::delete_space_record_with_swap(&mut tx, state.db_backend, &record_uri, &swap).await?;
        swap
    } else {
        let record = db::get_space_record(&mut *tx, state.db_backend, &record_uri).await?;
        let cid = match record {
            Some(r) if r.author_did != did => {
                return Err(AppError::Forbidden(
                    "You can only delete your own records".into(),
                ));
            }
            None => return Err(AppError::NotFound("Record not found".into())),
            Some(r) => r.cid,
        };
        db::delete_space_record(&mut *tx, state.db_backend, &record_uri).await?;
        cid
    };

    let ops = vec![AppliedOp {
        action: OplogAction::Delete,
        collection: collection.to_string(),
        rkey: rkey.to_string(),
        new_cid: None,
        old_cid: Some(old_cid),
    }];
    let committed = commit_write(
        &mut tx,
        state.db_backend,
        &space,
        did,
        &rev,
        &ops,
        &signing_key,
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("failed to commit transaction: {e}")))?;

    notify_ops(state, &space, did, &ops, &rev, committed).await;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn create_space(
    state: &AppState,
    did: &str,
    type_nsid: &str,
    skey: &str,
    display_name: Option<String>,
    description: Option<String>,
    read_policy: Option<Policy>,
    write_policy: Option<Policy>,
    app_access: Option<AppAccess>,
    config: Option<SpaceConfig>,
    creator_rotation_key: Option<&str>,
) -> Result<Space, AppError> {
    if type_nsid.is_empty() || skey.is_empty() {
        return Err(AppError::BadRequest("type and skey are required".into()));
    }
    // Every space gets a DID of its own, so no two can share an address. A
    // creator still cannot make two spaces with one type and key: callers
    // create spaces idempotently by name, and a second would split its members.
    // Checked before the DID is created, so a refusal leaves none behind.
    let taken = db::list_spaces_by_owner(&state.db, state.db_backend, did)
        .await?
        .into_iter()
        .any(|s| s.type_nsid == type_nsid && s.skey == skey);
    if taken {
        return Err(AppError::Conflict(
            "You already have a space with this type and key".into(),
        ));
    }
    let authority =
        crate::spaces::authority::create(state, did, type_nsid, skey, creator_rotation_key).await?;
    let config = config.unwrap_or_default();
    let space = Space {
        id: uuid::Uuid::new_v4().to_string(),
        did: authority.clone(),
        authority_did: authority,
        creator_did: did.to_string(),
        type_nsid: type_nsid.to_string(),
        skey: skey.to_string(),
        display_name,
        description,
        read_policy: read_policy.unwrap_or_default(),
        write_policy: write_policy.unwrap_or_default(),
        app_access: app_access.unwrap_or_default(),
        config,
        revision: None,
        created_at: now_rfc3339(),
        updated_at: now_rfc3339(),
    };
    db::create_space(&state.db, state.db_backend, &space).await?;
    if let Some(encryption_key) = &state.config.token_encryption_key
        && let Err(e) = crate::verification_methods::ensure_atproto_space_method(
            &state.db,
            state.db_backend,
            encryption_key,
        )
        .await
    {
        tracing::warn!("failed to auto-provision #atproto_space verification method: {e}");
    }
    if let Err(e) =
        crate::service_entries::ensure_space_host_entry(&state.db, state.db_backend).await
    {
        tracing::warn!("failed to auto-provision #atproto_space_host service entry: {e}");
    }
    let member = SpaceMember {
        id: uuid::Uuid::new_v4().to_string(),
        space_id: space.id.clone(),
        did: did.to_string(),
        access: MemberAccess::WRITE,
        is_delegation: false,
        granted_by: Some(did.to_string()),
        created_at: now_rfc3339(),
    };
    db::add_member(&state.db, state.db_backend, &member).await?;
    Ok(space)
}

/// Add a member, or replace their read and write access if they already exist.
///
/// `putMember` semantics: a repeat call updates the member rather than
/// conflicting. The upsert preserves `read_self` instead of resetting it.
/// `read_self` is HappyView-local and never sent over the wire, so a caller
/// replacing read/write access has not asked to lift an own-records-only
/// restriction.
/// Revoke a member's credentials, and tell the hosts that verify them.
async fn revoke_member_credentials(
    state: &AppState,
    space: &Space,
    member_did: &str,
) -> Result<(), AppError> {
    let jtis =
        db::outstanding_credential_jtis(&state.db, state.db_backend, &space.id, member_did).await?;
    db::revoke_space_credentials_for_member(&state.db, state.db_backend, &space.id, member_did)
        .await?;
    if !jtis.is_empty() {
        notifications::announce_revoked_credentials(state, space, jtis);
    }
    Ok(())
}

/// A managedspace has no member list: its managing app decides who reads and
/// writes. A member row on one would still grant access through
/// [`require_membership`], around the managing app, so none may be added.
fn no_members_on_managedspace() -> AppError {
    AppError::XrpcError {
        status: axum::http::StatusCode::BAD_REQUEST,
        code: "InvalidRequest",
        message: "a managedspace has no member list; its managing app decides access".into(),
    }
}

/// Insert or update a member, refusing a space that takes none. The policy is
/// read in the insert's own transaction, under the space's row lock, so an
/// insert racing a migration cannot land after the member list is discarded.
async fn insert_member(state: &AppState, member: &SpaceMember) -> Result<(), AppError> {
    let mut tx = crate::db::begin_write(&state.db, state.db_backend)
        .await
        .map_err(|e| AppError::Internal(format!("failed to begin transaction: {e}")))?;
    if !db::lock_space_for_members(&mut *tx, state.db_backend, &member.space_id)
        .await
        .map_err(|e| AppError::Internal(format!("failed to read space: {e}")))?
    {
        return Err(no_members_on_managedspace());
    }
    db::add_member(&mut *tx, state.db_backend, member).await?;
    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("failed to commit member: {e}")))
}

/// Upsert a member. `access` of `None` leaves an existing member's read and
/// write untouched, and gives a new one read — so a caller who names only a
/// DID, or only `is_delegation`, neither grants nor withdraws access.
///
/// `read_self` is carried forward on the same principle: it is HappyView-local,
/// and replacing read and write is not a request to lift an own-records-only
/// restriction.
pub(crate) async fn put_member(
    state: &AppState,
    actor_did: &str,
    space_ref: &str,
    member_did: &str,
    access: Option<MemberAccess>,
    is_delegation: Option<bool>,
) -> Result<SpaceMember, AppError> {
    let space = resolve_space(state, space_ref).await?;
    require_space_admin(state, &space, actor_did).await?;

    let existing = db::get_member(&state.db, state.db_backend, &space.id, member_did).await?;
    let current = existing.as_ref().map(|m| m.access);
    let access = access.unwrap_or_else(|| current.unwrap_or(MemberAccess::READ));
    let member = SpaceMember {
        id: existing
            .as_ref()
            .map(|m| m.id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        space_id: space.id.clone(),
        did: member_did.to_string(),
        access: MemberAccess {
            read_self: current.map(|a| a.read_self).unwrap_or(false),
            ..access
        },
        is_delegation: is_delegation.unwrap_or(false),
        granted_by: Some(actor_did.to_string()),
        created_at: now_rfc3339(),
    };
    insert_member(state, &member).await?;
    // A credential is the member's read access, so it goes when that does.
    if !member.access.read {
        revoke_member_credentials(state, &space, member_did).await?;
    }
    Ok(member)
}

pub(crate) async fn add_member(
    state: &AppState,
    actor_did: &str,
    space_ref: &str,
    member_did: &str,
    access: Option<MemberAccess>,
    is_delegation: Option<bool>,
) -> Result<SpaceMember, AppError> {
    let space = resolve_space(state, space_ref).await?;
    require_space_admin(state, &space, actor_did).await?;
    if db::get_member(&state.db, state.db_backend, &space.id, member_did)
        .await?
        .is_some()
    {
        return Err(AppError::Conflict(
            "Member already exists in this space".into(),
        ));
    }
    let member = SpaceMember {
        id: uuid::Uuid::new_v4().to_string(),
        space_id: space.id,
        did: member_did.to_string(),
        access: access.unwrap_or(MemberAccess::READ),
        is_delegation: is_delegation.unwrap_or(false),
        granted_by: Some(actor_did.to_string()),
        created_at: now_rfc3339(),
    };
    insert_member(state, &member).await?;
    Ok(member)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn update_space(
    state: &AppState,
    actor_did: &str,
    space_ref: &str,
    display_name: Option<Option<String>>,
    description: Option<Option<String>>,
    read_policy: Option<Policy>,
    write_policy: Option<Policy>,
    app_access: Option<AppAccess>,
    config: Option<SpaceConfig>,
) -> Result<Space, AppError> {
    let mut space = resolve_space(state, space_ref).await?;
    require_space_admin(state, &space, actor_did).await?;
    if let Some(name) = display_name {
        space.display_name = name;
    }
    if let Some(desc) = description {
        space.description = desc;
    }
    // Each supplied policy replaces the current one wholesale, per the lexicon.
    if let Some(policy) = read_policy {
        space.read_policy = policy;
    }
    if let Some(policy) = write_policy {
        space.write_policy = policy;
    }
    if let Some(access) = app_access {
        space.app_access = access;
    }
    if let Some(cfg) = config {
        space.config = cfg;
    }
    // Rules that now make a managedspace leave it no member list, as
    // `updateSpace` does on a migration.
    let managed = matches!(
        crate::spaces::policy::SpacePolicy::of(&space),
        Ok(crate::spaces::policy::SpacePolicy::Managed(_))
    );
    store_space(state, &space, managed).await?;
    Ok(space)
}

/// Store a space's fields and, with `discard_members`, discard its member
/// list and its invites, all in one transaction, so a failure cannot leave a
/// managedspace with the member list of the space it replaced.
///
/// The `UPDATE` takes the space's row first, as every member insert does
/// (`db::lock_space_for_members`), so an insert racing this one either lands
/// before the list is discarded or sees the new policy.
pub(crate) async fn store_space(
    state: &AppState,
    space: &Space,
    discard_members: bool,
) -> Result<(), AppError> {
    let mut tx = crate::db::begin_write(&state.db, state.db_backend)
        .await
        .map_err(|e| AppError::Internal(format!("failed to begin transaction: {e}")))?;
    db::update_space(&mut *tx, state.db_backend, space).await?;
    if discard_members {
        db::remove_all_members(&mut *tx, state.db_backend, &space.id).await?;
        // An outstanding invite would add a member once accepted.
        db::delete_invites(&mut *tx, state.db_backend, &space.id).await?;
    }
    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("failed to commit space update: {e}")))
}

pub(crate) async fn delete_space(
    state: &AppState,
    actor_did: &str,
    space_ref: &str,
) -> Result<(), AppError> {
    let space = resolve_space(state, space_ref).await?;
    require_space_admin(state, &space, actor_did).await?;
    let registrations =
        db::list_notify_registrations(&state.db, state.db_backend, &space.id).await?;
    db::delete_space(&state.db, state.db_backend, &space.id).await?;
    notifications::announce_space_deleted(state, &space, registrations);
    Ok(())
}

pub(crate) async fn remove_member(
    state: &AppState,
    actor_did: &str,
    space_ref: &str,
    member_did: &str,
) -> Result<(), AppError> {
    let space = resolve_space(state, space_ref).await?;
    require_space_admin(state, &space, actor_did).await?;
    revoke_member_credentials(state, &space, member_did).await?;
    let removed = db::remove_member(&state.db, state.db_backend, &space.id, member_did).await?;
    if !removed {
        return Err(AppError::NotFound("Member not found in this space".into()));
    }
    Ok(())
}

/// An invite's `expiresAt` as given at creation, as the UTC timestamp it is
/// stored as: the form [`now_rfc3339`] writes, so every stored expiry reads the
/// same way. A value that isn't a lexicon `datetime` is refused, naming the
/// field the way lexicon validation does.
///
/// An expiry already in the past is kept, not refused: creation takes the
/// other limits as given too (`maxUses: 0` is stored), and such an invite is
/// simply one redemption refuses.
fn normalize_invite_expiry(raw: &str) -> Result<String, AppError> {
    crate::xrpc::validate::parse_datetime(raw)
        .map(|at| at.with_timezone(&chrono::Utc).to_rfc3339())
        .ok_or_else(|| AppError::XrpcError {
            status: axum::http::StatusCode::BAD_REQUEST,
            code: "InvalidRequest",
            message: "Input/expiresAt must be a valid datetime (ISO 8601 with a timezone)".into(),
        })
}

/// Whether a stored invite expiry has passed at `now`, compared as times. A
/// stored value that doesn't parse as a timestamp (one written before creation
/// parsed it) counts as passed, so an invite is never open-ended by accident.
fn invite_expired(stored: &str, now: chrono::DateTime<chrono::Utc>) -> bool {
    match chrono::DateTime::parse_from_rfc3339(stored) {
        Ok(at) => now > at.with_timezone(&chrono::Utc),
        Err(err) => {
            tracing::warn!(expires_at = stored, "unparseable invite expiry: {err}");
            true
        }
    }
}

pub(crate) async fn create_invite(
    state: &AppState,
    actor_did: &str,
    space_ref: &str,
    access: Option<MemberAccess>,
    max_uses: Option<i64>,
    expires_at: Option<String>,
) -> Result<(SpaceInvite, String), AppError> {
    let expires_at = expires_at
        .as_deref()
        .map(normalize_invite_expiry)
        .transpose()?;
    let space = resolve_space(state, space_ref).await?;
    require_space_admin(state, &space, actor_did).await?;
    // Accepting re-checks under the space's row lock; this answers early.
    if matches!(
        crate::spaces::policy::SpacePolicy::of(&space),
        Ok(crate::spaces::policy::SpacePolicy::Managed(_))
    ) {
        return Err(no_members_on_managedspace());
    }
    let mut token_bytes = [0u8; 24];
    rand::Rng::fill_bytes(&mut rand::rng(), &mut token_bytes);
    let token = hex::encode(token_bytes);
    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
    let invite = SpaceInvite {
        id: uuid::Uuid::new_v4().to_string(),
        space_id: space.id,
        token_hash,
        created_by: actor_did.to_string(),
        access: access.unwrap_or(MemberAccess::READ),
        max_uses,
        uses: 0,
        expires_at,
        revoked: false,
        created_at: now_rfc3339(),
    };
    db::create_invite(&state.db, state.db_backend, &invite).await?;
    Ok((invite, token))
}

pub(crate) async fn accept_invite(
    state: &AppState,
    did: &str,
    token: &str,
) -> Result<(String, MemberAccess), AppError> {
    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
    let invite = db::get_invite_by_token_hash(&state.db, state.db_backend, &token_hash)
        .await?
        .ok_or_else(|| AppError::NotFound("Invalid invite token".into()))?;
    if invite.revoked {
        return Err(AppError::BadRequest("This invite has been revoked".into()));
    }
    if let Some(max) = invite.max_uses
        && invite.uses >= max
    {
        return Err(AppError::BadRequest(
            "This invite has reached its maximum uses".into(),
        ));
    }
    if let Some(ref expires) = invite.expires_at
        && invite_expired(expires, chrono::Utc::now())
    {
        return Err(AppError::BadRequest("This invite has expired".into()));
    }
    if db::get_member(&state.db, state.db_backend, &invite.space_id, did)
        .await?
        .is_some()
    {
        return Err(AppError::Conflict(
            "You are already a member of this space".into(),
        ));
    }
    let member = SpaceMember {
        id: uuid::Uuid::new_v4().to_string(),
        space_id: invite.space_id.clone(),
        did: did.to_string(),
        access: invite.access,
        is_delegation: false,
        granted_by: Some(invite.created_by.clone()),
        created_at: now_rfc3339(),
    };
    match db::claim_invite(&state.db, state.db_backend, &invite.id, &member).await? {
        db::InviteClaim::Joined => {}
        db::InviteClaim::Unavailable => {
            return Err(AppError::BadRequest(
                "This invite has been revoked or has reached its maximum uses".into(),
            ));
        }
        db::InviteClaim::AlreadyMember => {
            return Err(AppError::Conflict(
                "You are already a member of this space".into(),
            ));
        }
        db::InviteClaim::NoMembers => return Err(no_members_on_managedspace()),
    }
    let space = db::get_space(&state.db, state.db_backend, &invite.space_id).await?;
    let space_uri = space
        .map(|s| format!("at://{}/space/{}/{}", s.did, s.type_nsid, s.skey))
        .ok_or_else(|| AppError::Internal("space vanished after member insert".into()))?;
    Ok((space_uri, member.access))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DatabaseBackend;

    macro_rules! require_test_db {
        () => {
            if std::env::var("TEST_DATABASE_URL").is_err() {
                crate::test_support::unavailable(crate::test_support::NO_DATABASE);
                return;
            }
        };
    }

    async fn service_empty_db() -> AppState {
        let url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL must be set for spaces::service integration tests");
        let backend = DatabaseBackend::from_url(&url);
        let pool = crate::db::connect(&url, backend).await;

        // Port 0: the one field the shared fixture's configuration sets
        // differently.
        let mut state = crate::test_support::test_state_with_pool_on(pool, backend);
        state.config.port = 0;

        crate::test_support::provision_space_signing_key(&state).await;

        state
    }

    async fn service_test_db() -> (AppState, String, String) {
        service_test_db_with_config(SpaceConfig::default()).await
    }

    async fn service_test_db_with_config(config: SpaceConfig) -> (AppState, String, String) {
        let state = service_empty_db().await;

        let unique = uuid::Uuid::new_v4().simple().to_string();
        let space_id = uuid::Uuid::new_v4().to_string();
        let member_did = format!("did:plc:writer{unique}");
        let space_did = format!("did:plc:owner{unique}");
        let type_nsid = "com.example.forum";
        let skey = format!("main{unique}");

        let space = Space {
            id: space_id.clone(),
            did: space_did.clone(),
            authority_did: member_did.clone(),
            creator_did: member_did.clone(),
            type_nsid: type_nsid.to_string(),
            skey: skey.clone(),
            display_name: None,
            description: None,
            read_policy: Policy::MemberList,
            write_policy: Policy::MemberList,
            app_access: AppAccess::default(),
            config,
            revision: None,
            created_at: now_rfc3339(),
            updated_at: now_rfc3339(),
        };
        db::create_space(&state.db, state.db_backend, &space)
            .await
            .expect("failed to seed test space");

        let member = SpaceMember {
            id: uuid::Uuid::new_v4().to_string(),
            space_id: space_id.clone(),
            did: member_did.clone(),
            access: MemberAccess::WRITE,
            is_delegation: false,
            granted_by: None,
            created_at: now_rfc3339(),
        };
        db::add_member(&state.db, state.db_backend, &member)
            .await
            .expect("failed to seed test member");

        let space_uri = format!("at://{space_did}/space/{type_nsid}/{skey}");
        (state, space_uri, member_did)
    }

    fn config_with_allowed_collections(allowed: &[&str]) -> SpaceConfig {
        let mut config = SpaceConfig::default();
        config.extra.insert(
            "allowedCollections".to_string(),
            serde_json::Value::Array(
                allowed
                    .iter()
                    .map(|s| serde_json::Value::String(s.to_string()))
                    .collect(),
            ),
        );
        config
    }

    #[tokio::test]
    async fn delete_record_forbidden_for_non_author() {
        require_test_db!();
        let (state, space_uri, member_a) = service_test_db().await; // member_a is a write-member (and authority)
        let member_b = format!("did:plc:writerB{}", uuid::Uuid::new_v4().simple());
        super::add_member(
            &state,
            &member_a,
            &space_uri,
            &member_b,
            Some(MemberAccess::WRITE),
            None,
        )
        .await
        .expect("authority may add member B as a write-member");

        let space = super::resolve_space(&state, &space_uri).await.unwrap();
        let collection = "com.example.item";
        let rkey = "fixedrkey-ownership";
        let record_uri = format!(
            "at://{}/space/{}/{}/{}/{}/{}",
            space.did, space.type_nsid, space.skey, member_b, collection, rkey
        );
        let content = serde_json::json!({ "text": "authored by A" });
        let rec = SpaceRecord {
            uri: record_uri.clone(),
            space_id: space.id.clone(),
            author_did: member_a.clone(), // stored author is A, not B
            collection: collection.to_string(),
            rkey: rkey.to_string(),
            record: content.clone(),
            cid: super::content_cid(&content).expect("test record must be DAG-CBOR encodable"),
            indexed_at: now_rfc3339(),
        };
        db::insert_space_record(&state.db, state.db_backend, &rec)
            .await
            .expect("failed to seed mismatched-author record");

        let err = super::delete_record(&state, &member_b, &space_uri, collection, rkey, None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, crate::error::AppError::Forbidden(_)),
            "expected Forbidden, got: {err:?}"
        );
        // record must still be present since the delete was rejected
        assert!(
            db::get_space_record(&state.db, state.db_backend, &record_uri)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn delete_record_rejects_non_member() {
        require_test_db!();
        let (state, space_uri, _member_did) = service_test_db().await;
        let stranger = format!("did:plc:stranger{}", uuid::Uuid::new_v4().simple());

        let err = super::delete_record(
            &state,
            &stranger,
            &space_uri,
            "com.example.item",
            "rk1",
            None,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, crate::error::AppError::Forbidden(_)),
            "expected Forbidden, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn accept_invite_rejects_revoked() {
        require_test_db!();
        let (state, space_uri, authority) = service_test_db().await;
        let (invite, token) =
            super::create_invite(&state, &authority, &space_uri, None, None, None)
                .await
                .expect("authority creates invite");
        let revoked = db::revoke_invite(&state.db, state.db_backend, &invite.space_id, &invite.id)
            .await
            .expect("revoke should succeed");
        assert!(revoked);

        let err = super::accept_invite(&state, "did:plc:joiner-revoked", &token)
            .await
            .unwrap_err();
        match err {
            crate::error::AppError::BadRequest(msg) => {
                assert!(
                    msg.to_lowercase().contains("revoked"),
                    "expected 'revoked' in message, got: {msg}"
                );
            }
            other => panic!("expected BadRequest, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn accept_invite_rejects_expired() {
        require_test_db!();
        let (state, space_uri, authority) = service_test_db().await;
        let (_invite, token) = super::create_invite(
            &state,
            &authority,
            &space_uri,
            None,
            None,
            Some("2000-01-01T00:00:00+00:00".to_string()),
        )
        .await
        .expect("authority creates invite");

        let err = super::accept_invite(&state, "did:plc:joiner-expired", &token)
            .await
            .unwrap_err();
        match err {
            crate::error::AppError::BadRequest(msg) => {
                assert!(
                    msg.to_lowercase().contains("expired"),
                    "expected 'expired' in message, got: {msg}"
                );
            }
            other => panic!("expected BadRequest, got: {other:?}"),
        }
    }

    /// An invite created with `expires_at` exactly as given, the way one was
    /// stored before creation parsed it.
    async fn seed_raw_invite(state: &AppState, space_uri: &str, expires_at: &str) -> String {
        let space = super::resolve_space(state, space_uri)
            .await
            .expect("space resolves");
        let token = uuid::Uuid::new_v4().simple().to_string();
        let invite = SpaceInvite {
            id: uuid::Uuid::new_v4().to_string(),
            space_id: space.id,
            token_hash: hex::encode(Sha256::digest(token.as_bytes())),
            created_by: space.authority_did,
            access: MemberAccess::READ,
            max_uses: None,
            uses: 0,
            expires_at: Some(expires_at.to_string()),
            revoked: false,
            created_at: now_rfc3339(),
        };
        db::create_invite(&state.db, state.db_backend, &invite)
            .await
            .expect("failed to seed invite");
        token
    }

    fn assert_expired(err: crate::error::AppError) {
        match err {
            crate::error::AppError::BadRequest(msg) => assert!(
                msg.to_lowercase().contains("expired"),
                "expected 'expired' in message, got: {msg}"
            ),
            other => panic!("expected BadRequest, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_offset_expiry_in_the_past_is_refused_at_redeem() {
        require_test_db!();
        let (state, space_uri, authority) = service_test_db().await;
        // One hour ago, written in +02:00: its local clock reads an hour
        // later than UTC now, so as text it sorts after the current time.
        let past = (chrono::Utc::now() - chrono::Duration::hours(1))
            .with_timezone(&chrono::FixedOffset::east_opt(2 * 3600).unwrap())
            .to_rfc3339();
        let (_invite, token) =
            super::create_invite(&state, &authority, &space_uri, None, None, Some(past))
                .await
                .expect("authority creates invite");

        let err = super::accept_invite(&state, "did:plc:joiner-offset-past", &token)
            .await
            .unwrap_err();
        assert_expired(err);
    }

    #[tokio::test]
    async fn an_offset_expiry_in_the_future_is_accepted_at_redeem() {
        require_test_db!();
        let (state, space_uri, authority) = service_test_db().await;
        let at = chrono::Utc::now() + chrono::Duration::hours(1);
        let future = at
            .with_timezone(&chrono::FixedOffset::east_opt(2 * 3600).unwrap())
            .to_rfc3339();
        let (invite, token) =
            super::create_invite(&state, &authority, &space_uri, None, None, Some(future))
                .await
                .expect("authority creates invite");
        assert_eq!(
            invite.expires_at.as_deref(),
            Some(at.to_rfc3339().as_str()),
            "the expiry is stored in UTC, in the form now_rfc3339 writes"
        );

        super::accept_invite(&state, "did:plc:joiner-offset-future", &token)
            .await
            .expect("an unexpired invite is redeemed");
    }

    #[tokio::test]
    async fn a_utc_expiry_still_works() {
        require_test_db!();
        let (state, space_uri, authority) = service_test_db().await;
        let (invite, token) = super::create_invite(
            &state,
            &authority,
            &space_uri,
            None,
            None,
            Some("2999-01-01T00:00:00Z".to_string()),
        )
        .await
        .expect("authority creates invite");
        assert_eq!(
            invite.expires_at.as_deref(),
            Some("2999-01-01T00:00:00+00:00")
        );
        let stored = db::get_invite_by_token_hash(
            &state.db,
            state.db_backend,
            &hex::encode(Sha256::digest(token.as_bytes())),
        )
        .await
        .unwrap()
        .expect("invite stored");
        assert_eq!(stored.expires_at, invite.expires_at);

        super::accept_invite(&state, "did:plc:joiner-utc-future", &token)
            .await
            .expect("an unexpired invite is redeemed");
    }

    #[tokio::test]
    async fn a_non_timestamp_expiry_is_refused_at_creation() {
        require_test_db!();
        let (state, space_uri, authority) = service_test_db().await;
        for bad in ["next tuesday", "2026-06-01", "2026-06-01 00:00:00Z", ""] {
            let err = super::create_invite(
                &state,
                &authority,
                &space_uri,
                None,
                None,
                Some(bad.to_string()),
            )
            .await
            .unwrap_err();
            match err {
                crate::error::AppError::XrpcError {
                    status,
                    code,
                    message,
                } => {
                    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{bad:?}");
                    assert_eq!(code, "InvalidRequest", "{bad:?}");
                    assert!(message.contains("expiresAt"), "{bad:?}: {message}");
                }
                other => panic!("{bad:?}: expected InvalidRequest, got: {other:?}"),
            }
        }
        let space = super::resolve_space(&state, &space_uri).await.unwrap();
        let invites = db::list_invites(&state.db, state.db_backend, &space.id)
            .await
            .unwrap();
        assert!(invites.is_empty(), "a refused invite is not stored");
    }

    #[tokio::test]
    async fn a_stored_expiry_that_does_not_parse_counts_as_expired() {
        require_test_db!();
        let (state, space_uri, _authority) = service_test_db().await;
        let token = seed_raw_invite(&state, &space_uri, "never").await;

        let err = super::accept_invite(&state, "did:plc:joiner-legacy", &token)
            .await
            .unwrap_err();
        assert_expired(err);
    }

    #[tokio::test]
    async fn a_stored_offset_expiry_is_compared_as_a_time() {
        require_test_db!();
        let (state, space_uri, _authority) = service_test_db().await;
        let past = (chrono::Utc::now() - chrono::Duration::hours(1))
            .with_timezone(&chrono::FixedOffset::east_opt(2 * 3600).unwrap())
            .to_rfc3339();
        let token = seed_raw_invite(&state, &space_uri, &past).await;

        let err = super::accept_invite(&state, "did:plc:joiner-legacy-offset", &token)
            .await
            .unwrap_err();
        assert_expired(err);
    }

    #[test]
    fn invite_expiry_is_normalized_to_utc() {
        assert_eq!(
            super::normalize_invite_expiry("2026-06-01T02:00:00+02:00").unwrap(),
            "2026-06-01T00:00:00+00:00"
        );
        assert_eq!(
            super::normalize_invite_expiry("2026-06-01T00:00:00.5Z").unwrap(),
            "2026-06-01T00:00:00.500+00:00"
        );
        assert!(super::normalize_invite_expiry("2026-06-01T00:00:00-00:00").is_err());
    }

    #[test]
    fn invite_expiry_compares_times_not_text() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-06-01T00:30:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        // 2026-06-01T00:00:00Z: passed, though it sorts after `now` as text.
        assert!(super::invite_expired("2026-06-01T02:00:00+02:00", now));
        // 2026-06-01T01:00:00Z: not passed, though it sorts before `now`.
        assert!(!super::invite_expired("2026-05-31T20:00:00-05:00", now));
        assert!(super::invite_expired("not a date", now));
    }

    #[tokio::test]
    async fn accept_invite_rejects_after_max_uses_exhausted() {
        require_test_db!();
        let (state, space_uri, authority) = service_test_db().await;
        let (_invite, token) =
            super::create_invite(&state, &authority, &space_uri, None, Some(1), None)
                .await
                .expect("authority creates invite");

        super::accept_invite(&state, "did:plc:joiner-x", &token)
            .await
            .expect("first accept (X) should succeed");

        let err = super::accept_invite(&state, "did:plc:joiner-y", &token)
            .await
            .unwrap_err();
        match err {
            crate::error::AppError::BadRequest(msg) => {
                assert!(
                    msg.to_lowercase().contains("maximum uses"),
                    "expected 'maximum uses' in message, got: {msg}"
                );
            }
            other => panic!("expected BadRequest, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn accept_invite_rejects_already_member() {
        require_test_db!();
        let (state, space_uri, authority) = service_test_db().await;
        let (_invite, token) =
            super::create_invite(&state, &authority, &space_uri, None, None, None)
                .await
                .expect("authority creates invite");

        super::accept_invite(&state, "did:plc:joiner-twice", &token)
            .await
            .expect("first accept should succeed");

        let err = super::accept_invite(&state, "did:plc:joiner-twice", &token)
            .await
            .unwrap_err();
        assert!(
            matches!(err, crate::error::AppError::Conflict(_)),
            "expected Conflict, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn put_record_upserts_then_rejects_swap_mismatch() {
        require_test_db!();
        let (state, space_uri, member_did) = service_test_db().await;
        let collection = "com.example.item";
        let rkey = "fixedrkey-put";

        let (uri1, cid1) = super::put_record(
            &state,
            &member_did,
            None,
            &space_uri,
            collection,
            rkey,
            serde_json::json!({ "text": "v1" }),
            None,
        )
        .await
        .expect("first put_record should create the record");

        let (uri2, cid2) = super::put_record(
            &state,
            &member_did,
            None,
            &space_uri,
            collection,
            rkey,
            serde_json::json!({ "text": "v2" }),
            None,
        )
        .await
        .expect("second put_record should update the existing record");

        assert_eq!(
            uri1, uri2,
            "put_record on the same rkey should be idempotent on URI"
        );
        assert_ne!(cid1, cid2, "content changed, so the CID should change");

        let stored = db::get_space_record(&state.db, state.db_backend, &uri1)
            .await
            .unwrap()
            .expect("record should exist");
        assert_eq!(stored.record, serde_json::json!({ "text": "v2" }));
        assert_eq!(stored.cid, cid2);

        // confirm only one row exists for this collection (no duplicate insert)
        let space = super::resolve_space(&state, &space_uri).await.unwrap();
        let (records, _cursor) = db::list_space_records(
            &state.db,
            state.db_backend,
            &space.id,
            None,
            Some(collection),
            100,
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(records.len(), 1, "expected exactly one row after upsert");

        // wrong swap_cid should fail with a CID-mismatch Conflict
        let err = super::put_record(
            &state,
            &member_did,
            None,
            &space_uri,
            collection,
            rkey,
            serde_json::json!({ "text": "v3" }),
            Some("bafyreiwrongwrongwrongwrongwrong".to_string()),
        )
        .await
        .unwrap_err();
        match err {
            crate::error::AppError::Conflict(msg) => {
                assert!(
                    msg.contains("CID mismatch"),
                    "expected 'CID mismatch' in message, got: {msg}"
                );
            }
            other => panic!("expected Conflict, got: {other:?}"),
        }
        // content must be unchanged after the rejected swap
        let stored = db::get_space_record(&state.db, state.db_backend, &uri1)
            .await
            .unwrap()
            .expect("record should still exist");
        assert_eq!(stored.record, serde_json::json!({ "text": "v2" }));
    }

    #[tokio::test]
    async fn delete_record_with_swap_cid() {
        require_test_db!();
        let (state, space_uri, member_did) = service_test_db().await;
        let collection = "com.example.item";
        let rkey = "fixedrkey-del";

        let (uri, cid) = super::put_record(
            &state,
            &member_did,
            None,
            &space_uri,
            collection,
            rkey,
            serde_json::json!({ "text": "to-delete" }),
            None,
        )
        .await
        .expect("put_record should create the record");

        // wrong swap_cid -> Conflict (record exists, per db::delete_space_record_with_swap)
        let err = super::delete_record(
            &state,
            &member_did,
            &space_uri,
            collection,
            rkey,
            Some("bafyreiwrongwrongwrongwrongwrong".to_string()),
        )
        .await
        .unwrap_err();
        match err {
            crate::error::AppError::Conflict(msg) => {
                assert!(
                    msg.contains("CID mismatch"),
                    "expected 'CID mismatch' in message, got: {msg}"
                );
            }
            other => panic!("expected Conflict, got: {other:?}"),
        }
        // record must still be present
        assert!(
            db::get_space_record(&state.db, state.db_backend, &uri)
                .await
                .unwrap()
                .is_some()
        );

        // correct swap_cid -> deletes
        super::delete_record(&state, &member_did, &space_uri, collection, rkey, Some(cid))
            .await
            .expect("delete with correct swap_cid should succeed");
        assert!(
            db::get_space_record(&state.db, state.db_backend, &uri)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn create_record_inserts_and_bumps_revision() {
        require_test_db!();
        let (state, space_uri, member_did) = service_test_db().await; // space with member_did as write-member
        let (uri, cid) = super::create_record(
            &state,
            &member_did,
            None,
            &space_uri,
            "com.example.item",
            serde_json::json!({ "text": "hello" }),
        )
        .await
        .expect("write should succeed for a write-member");
        assert!(uri.starts_with("at://"));
        assert!(cid.starts_with("bafyrei"));
        // revision bumped
        let space = super::resolve_space(&state, &space_uri).await.unwrap();
        assert!(space.revision.is_some());
    }

    #[tokio::test]
    async fn create_record_rejects_non_member() {
        require_test_db!();
        let (state, space_uri, _member) = service_test_db().await;
        let err = super::create_record(
            &state,
            "did:plc:stranger",
            None,
            &space_uri,
            "com.example.item",
            serde_json::json!({ "text": "no" }),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, crate::error::AppError::Forbidden(_)));
    }

    /// Any collection may appear in a space, as Proposal 0016 has it. An
    /// `allowedCollections` list stored by an earlier HappyView limits nothing.
    #[tokio::test]
    async fn a_stored_collection_list_does_not_limit_writes() {
        require_test_db!();
        let (state, space_uri, member_did) =
            service_test_db_with_config(config_with_allowed_collections(&["com.example.allowed"]))
                .await;

        super::create_record(
            &state,
            &member_did,
            None,
            &space_uri,
            "com.example.unlisted",
            serde_json::json!({ "text": "yes" }),
        )
        .await
        .expect("create to an unlisted collection");
        super::put_record(
            &state,
            &member_did,
            None,
            &space_uri,
            "com.example.unlisted",
            "fixedrkey-put-unlisted",
            serde_json::json!({ "text": "yes" }),
            None,
        )
        .await
        .expect("put to an unlisted collection");
    }

    #[tokio::test]
    async fn create_record_allows_listed_collection() {
        require_test_db!();
        let (state, space_uri, member_did) =
            service_test_db_with_config(config_with_allowed_collections(&["com.example.allowed"]))
                .await;

        let (uri, _cid) = super::create_record(
            &state,
            &member_did,
            None,
            &space_uri,
            "com.example.allowed",
            serde_json::json!({ "text": "yes" }),
        )
        .await
        .expect("write to an allowed collection should succeed");
        assert!(uri.contains("/com.example.allowed/"));
    }

    #[tokio::test]
    async fn create_record_allows_any_collection_when_config_absent() {
        require_test_db!();
        let (state, space_uri, member_did) = service_test_db().await; // default config
        super::create_record(
            &state,
            &member_did,
            None,
            &space_uri,
            "com.example.anything",
            serde_json::json!({ "text": "ok" }),
        )
        .await
        .expect("space without allowedCollections config should allow any collection");
    }

    #[tokio::test]
    async fn create_record_allows_any_collection_when_list_empty() {
        require_test_db!();
        let (state, space_uri, member_did) =
            service_test_db_with_config(config_with_allowed_collections(&[])).await;
        super::create_record(
            &state,
            &member_did,
            None,
            &space_uri,
            "com.example.anything",
            serde_json::json!({ "text": "ok" }),
        )
        .await
        .expect("empty allowedCollections list should allow any collection");
    }

    #[tokio::test]
    async fn put_record_allows_listed_collection() {
        require_test_db!();
        let (state, space_uri, member_did) =
            service_test_db_with_config(config_with_allowed_collections(&["com.example.allowed"]))
                .await;

        let (uri, _cid) = super::put_record(
            &state,
            &member_did,
            None,
            &space_uri,
            "com.example.allowed",
            "fixedrkey-put-allowed",
            serde_json::json!({ "text": "yes" }),
            None,
        )
        .await
        .expect("put to an allowed collection should succeed");
        assert!(uri.contains("/com.example.allowed/"));
    }

    #[tokio::test]
    async fn create_space_inserts_and_adds_creator_as_writer() {
        require_test_db!();
        let state = service_empty_db().await; // migrated DB, no spaces
        let unique = uuid::Uuid::new_v4().simple().to_string();
        let creator_did = format!("did:plc:creator{unique}");
        let skey = format!("general{unique}");
        let mut state = state;
        let _plc = accepting_plc(&mut state).await;
        let space = super::create_space(
            &state,
            &creator_did,
            "com.example.chat",
            &skey,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create should succeed");
        assert!(space.authority_did.starts_with("did:plc:"));
        assert_ne!(space.authority_did, creator_did);
        assert_eq!(space.creator_did, creator_did);
        let access =
            crate::spaces::members::is_member(&state.db, state.db_backend, &space.id, &creator_did)
                .await
                .unwrap();
        assert_eq!(access, Some(crate::spaces::types::MemberAccess::WRITE));
    }

    async fn create_in_memory(
        state: &AppState,
        creator: &str,
        skey: &str,
    ) -> Result<Space, AppError> {
        super::create_space(
            state,
            creator,
            "com.example.chat",
            skey,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .await
    }

    /// A PLC directory that accepts every genesis operation, with `state`
    /// pointed at it. Keep the server for the length of the test.
    async fn accepting_plc(state: &mut AppState) -> wiremock::MockServer {
        use wiremock::matchers::method;
        use wiremock::{Mock, ResponseTemplate};

        let server = wiremock::MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        state.config.plc_url = server.uri();
        server
    }

    async fn memory_state_with_plc() -> (AppState, wiremock::MockServer) {
        let mut state = crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        );
        let plc = accepting_plc(&mut state).await;
        (state, plc)
    }

    /// The genesis operations the mock PLC received, by DID.
    async fn published(plc: &wiremock::MockServer) -> Vec<(String, serde_json::Value)> {
        plc.received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.method.as_str() == "POST")
            .map(|r| {
                (
                    r.url.path().trim_start_matches('/').to_string(),
                    serde_json::from_slice(&r.body).unwrap(),
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn creating_a_space_advertises_this_instance_as_space_host() {
        let (state, _plc) = memory_state_with_plc().await;
        create_in_memory(&state, "did:plc:creator", "general")
            .await
            .expect("create should succeed");

        let entries = crate::service_entries::list_entries(&state.db, state.db_backend)
            .await
            .unwrap();
        assert!(
            entries
                .iter()
                .any(|e| e.fragment_id == "#atproto_space_host"),
            "no #atproto_space_host service entry: {entries:?}"
        );
    }

    const INSTANCE_DID: &str = "did:plc:happyviewinstance";

    async fn state_with_instance_identity() -> (AppState, wiremock::MockServer) {
        let (state, plc) = memory_state_with_plc().await;
        crate::service_identity::upsert_identity(
            &state.db,
            state.db_backend,
            &crate::service_identity::IdentityMode::DidPlc,
            Some(INSTANCE_DID),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        (state, plc)
    }

    #[tokio::test]
    async fn every_space_gets_a_did_of_its_own_hosted_here() {
        let (state, plc) = state_with_instance_identity().await;
        let space = create_in_memory(&state, "did:plc:creator", "general")
            .await
            .expect("create should succeed");
        assert_ne!(space.authority_did, INSTANCE_DID);
        assert_eq!(space.did, space.authority_did);
        assert_eq!(space.creator_did, "did:plc:creator");

        let ops = published(&plc).await;
        assert_eq!(ops.len(), 1, "one genesis operation: {ops:?}");
        let (did, op) = &ops[0];
        assert_eq!(did, &space.authority_did);
        assert_eq!(happyview_plc::derive_did(op).unwrap(), space.authority_did);
        assert_eq!(
            op["services"]["atproto_pds"],
            serde_json::json!({
                "type": "AtprotoPersonalDataServer",
                "endpoint": state.config.public_url,
            })
        );
        assert!(op["verificationMethods"]["atproto"].is_string());
        assert!(op["verificationMethods"]["atproto_kem"].is_string());
        assert_ne!(
            op["verificationMethods"]["atproto"],
            op["verificationMethods"]["atproto_kem"]
        );
        assert_eq!(op["rotationKeys"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_creator_rotation_key_outranks_this_instances() {
        let (state, plc) = memory_state_with_plc().await;
        let creator_key = happyview_plc::private_key_to_did_key(&[7u8; 32]).unwrap();
        let space = super::create_space(
            &state,
            "did:plc:creator",
            "com.example.chat",
            "general",
            None,
            None,
            None,
            None,
            None,
            None,
            Some(&creator_key),
        )
        .await
        .unwrap();

        let (_, op) = published(&plc).await.remove(0);
        let keys = op["rotationKeys"].as_array().unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(
            keys[0],
            creator_key.as_str(),
            "the creator's key comes first"
        );
        assert!(
            crate::spaces::authority::holds(&state.db, state.db_backend, &space.authority_did)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn a_creator_rotation_key_that_is_not_a_did_key_is_refused() {
        let (state, plc) = memory_state_with_plc().await;
        let err = super::create_space(
            &state,
            "did:plc:creator",
            "com.example.chat",
            "general",
            None,
            None,
            None,
            None,
            None,
            None,
            Some("not-a-key"),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)), "{err:?}");
        assert!(published(&plc).await.is_empty());
    }

    #[tokio::test]
    async fn a_space_the_directory_refuses_is_not_created() {
        let mut state = crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        );
        let plc = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(500))
            .mount(&plc)
            .await;
        state.config.plc_url = plc.uri();

        create_in_memory(&state, "did:plc:creator", "general")
            .await
            .unwrap_err();
        assert!(
            db::list_spaces_by_owner(&state.db, state.db_backend, "did:plc:creator")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn the_creator_administers_a_space_this_instance_is_authority_for() {
        let (state, _plc) = state_with_instance_identity().await;
        let space = create_in_memory(&state, "did:plc:creator", "general")
            .await
            .unwrap();
        let uri = format!(
            "at://{}/space/{}/{}",
            space.did, space.type_nsid, space.skey
        );

        super::put_member(
            &state,
            "did:plc:creator",
            &uri,
            "did:plc:friend",
            Some(MemberAccess::READ),
            None,
        )
        .await
        .expect("the creator may manage members");
        let err = super::put_member(
            &state,
            "did:plc:friend",
            &uri,
            "did:plc:other",
            Some(MemberAccess::READ),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Forbidden(_)));
    }

    /// A DID the directory refused is submitted again, unchanged, when the
    /// space is created again, instead of leaving a stranded key row.
    #[tokio::test]
    async fn a_refused_did_is_submitted_again_on_the_next_create() {
        let mut state = crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        );
        let plc = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(500))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&plc)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .mount(&plc)
            .await;
        state.config.plc_url = plc.uri();

        create_in_memory(&state, "did:plc:creator", "general")
            .await
            .unwrap_err();
        let space = create_in_memory(&state, "did:plc:creator", "general")
            .await
            .expect("the second create publishes the stored operation");

        let ops = published(&plc).await;
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0], ops[1], "the same operation, for the same DID");
        assert_eq!(ops[1].0, space.authority_did);
    }

    /// Two creates of one space that both pass the per-creator check share
    /// its DID, so the second space row meets the address constraint, and is
    /// a conflict rather than an internal error.
    #[tokio::test]
    async fn a_second_space_at_one_address_is_a_conflict() {
        let (state, _plc) = memory_state_with_plc().await;
        let space = create_in_memory(&state, "did:plc:creator", "general")
            .await
            .unwrap();
        let twin = Space {
            id: uuid::Uuid::new_v4().to_string(),
            ..space
        };
        let err = db::create_space(&state.db, state.db_backend, &twin)
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)), "{err:?}");
    }

    /// A directory that stored a genesis but whose answer was lost refuses a
    /// second submission of it as misordered. The next create reads the DID's
    /// log, finds its operation there, and finishes.
    #[tokio::test]
    async fn a_genesis_the_directory_already_holds_is_not_submitted_again() {
        use std::sync::{Arc, Mutex};
        use wiremock::{Mock, Request, Respond, ResponseTemplate, matchers};

        struct StoreThenFail(Arc<Mutex<Option<serde_json::Value>>>);
        impl Respond for StoreThenFail {
            fn respond(&self, request: &Request) -> ResponseTemplate {
                let mut stored = self.0.lock().unwrap();
                if stored.is_none() {
                    *stored = serde_json::from_slice(&request.body).ok();
                    ResponseTemplate::new(500)
                } else {
                    ResponseTemplate::new(400).set_body_json(serde_json::json!({
                        "message": "Operations not correctly ordered",
                    }))
                }
            }
        }
        // The log cannot be read at first, so the first create fails with
        // its row left `prepared`; the next create finds the operation there.
        struct Log(
            Arc<Mutex<Option<serde_json::Value>>>,
            std::sync::atomic::AtomicBool,
        );
        impl Respond for Log {
            fn respond(&self, _: &Request) -> ResponseTemplate {
                if !self.1.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    return ResponseTemplate::new(500);
                }
                match self.0.lock().unwrap().clone() {
                    Some(op) => ResponseTemplate::new(200).set_body_json(serde_json::json!([op])),
                    None => ResponseTemplate::new(404),
                }
            }
        }

        let mut state = crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        );
        let stored = Arc::new(Mutex::new(None));
        let plc = wiremock::MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .respond_with(StoreThenFail(stored.clone()))
            .mount(&plc)
            .await;
        Mock::given(matchers::method("GET"))
            .and(matchers::path_regex(r"^/did:plc:[a-z2-7]{24}/log$"))
            .respond_with(Log(stored.clone(), Default::default()))
            .mount(&plc)
            .await;
        state.config.plc_url = plc.uri();

        create_in_memory(&state, "did:plc:creator", "general")
            .await
            .unwrap_err();
        let space = create_in_memory(&state, "did:plc:creator", "general")
            .await
            .expect("the directory holds the genesis, so the create finishes");
        let posts = plc
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.method.as_str() == "POST")
            .count();
        assert_eq!(posts, 1, "the held genesis is not submitted again");
        let held = stored.lock().unwrap().clone().unwrap();
        assert_eq!(
            happyview_plc::derive_did(&held).unwrap(),
            space.authority_did
        );
        assert!(
            crate::spaces::authority::holds(&state.db, state.db_backend, &space.authority_did)
                .await
                .unwrap()
        );
    }

    /// A submission refused after the directory stored it, as when its answer
    /// is lost, is checked against the DID's log, and the space is created.
    #[tokio::test]
    async fn a_refused_submission_the_directory_holds_still_creates_the_space() {
        use std::sync::{Arc, Mutex};
        use wiremock::{Mock, Request, Respond, ResponseTemplate, matchers};

        struct StoreAndFail(Arc<Mutex<Option<serde_json::Value>>>);
        impl Respond for StoreAndFail {
            fn respond(&self, request: &Request) -> ResponseTemplate {
                *self.0.lock().unwrap() = serde_json::from_slice(&request.body).ok();
                ResponseTemplate::new(504)
            }
        }
        struct Log(Arc<Mutex<Option<serde_json::Value>>>);
        impl Respond for Log {
            fn respond(&self, _: &Request) -> ResponseTemplate {
                match self.0.lock().unwrap().clone() {
                    Some(op) => ResponseTemplate::new(200).set_body_json(serde_json::json!([op])),
                    None => ResponseTemplate::new(404),
                }
            }
        }

        let mut state = crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        );
        let stored = Arc::new(Mutex::new(None));
        let plc = wiremock::MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .respond_with(StoreAndFail(stored.clone()))
            .mount(&plc)
            .await;
        Mock::given(matchers::method("GET"))
            .and(matchers::path_regex(r"^/did:plc:[a-z2-7]{24}/log$"))
            .respond_with(Log(stored.clone()))
            .mount(&plc)
            .await;
        state.config.plc_url = plc.uri();

        let space = create_in_memory(&state, "did:plc:creator", "general")
            .await
            .expect("the directory holds the genesis");
        let held = stored.lock().unwrap().clone().unwrap();
        assert_eq!(
            happyview_plc::derive_did(&held).unwrap(),
            space.authority_did
        );
    }

    /// A recreated space keeps its DID and key, so a credential minted for the
    /// deleted one would verify. One issued before this space was created is
    /// refused.
    #[tokio::test]
    async fn a_credential_older_than_its_space_is_refused() {
        let (state, _plc) = memory_state_with_plc().await;
        let space = create_in_memory(&state, "did:plc:creator", "general")
            .await
            .unwrap();
        let key = crate::spaces::authority::signing_key(
            &state.db,
            state.db_backend,
            &state.config.keyring().unwrap(),
            &space.authority_did,
        )
        .await
        .unwrap()
        .unwrap();
        let created = chrono::DateTime::parse_from_rfc3339(&space.created_at)
            .unwrap()
            .timestamp() as u64;
        let credential = |iat: u64| {
            let claims = crate::spaces::credential::SpaceCredentialClaims {
                iss: space.authority_did.clone(),
                sub: format!(
                    "at://{}/space/{}/{}",
                    space.did, space.type_nsid, space.skey
                ),
                iat,
                exp: iat + 600,
                jti: crate::spaces::credential::make_jti(),
                cnf: None,
            };
            crate::spaces::credential::sign_credential_with_kid(&claims, &key, "#atproto").unwrap()
        };

        let err = super::verify_space_credential(&state, &space, &credential(created - 60))
            .await
            .unwrap_err();
        assert!(
            matches!(
                &err,
                AppError::XrpcError {
                    code: "InvalidCredential",
                    ..
                }
            ),
            "{err:?}"
        );
        super::verify_space_credential(&state, &space, &credential(created))
            .await
            .expect("a credential issued as the space was created verifies");
    }

    /// A deleted space recreated with the same type and key gets its DID back,
    /// so its address, which its members' records name, is unchanged.
    #[tokio::test]
    async fn a_recreated_space_keeps_its_did() {
        let (state, plc) = memory_state_with_plc().await;
        let first = create_in_memory(&state, "did:plc:creator", "general")
            .await
            .unwrap();
        let uri = format!(
            "at://{}/space/{}/{}",
            first.did, first.type_nsid, first.skey
        );
        super::delete_space(&state, "did:plc:creator", &uri)
            .await
            .unwrap();
        let again = create_in_memory(&state, "did:plc:creator", "general")
            .await
            .unwrap();
        assert_eq!(again.authority_did, first.authority_did);
        assert_eq!(published(&plc).await.len(), 1, "published once");
    }

    #[tokio::test]
    async fn creators_can_share_an_skey_but_one_creator_cannot_reuse_it() {
        let (state, _plc) = state_with_instance_identity().await;
        let alice = create_in_memory(&state, "did:plc:alice", "general")
            .await
            .unwrap();
        let bob = create_in_memory(&state, "did:plc:bob", "general")
            .await
            .expect("each space has its own DID, so two creators may use one key");
        assert_ne!(alice.authority_did, bob.authority_did);

        let err = create_in_memory(&state, "did:plc:alice", "general")
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)));
    }

    #[tokio::test]
    async fn delete_space_requires_admin() {
        require_test_db!();
        let (state, space_uri, authority) = service_test_db().await;
        let err = super::delete_space(&state, "did:plc:notadmin", &space_uri)
            .await
            .unwrap_err();
        assert!(matches!(err, crate::error::AppError::Forbidden(_)));
        super::delete_space(&state, &authority, &space_uri)
            .await
            .expect("authority may delete");
        assert!(super::resolve_space(&state, &space_uri).await.is_err()); // gone
    }

    #[tokio::test]
    async fn add_member_requires_admin() {
        require_test_db!();
        let (state, space_uri, authority) = service_test_db().await; // authority is the space authority_did
        // authority can add
        super::add_member(
            &state,
            &authority,
            &space_uri,
            "did:plc:newbie",
            Some(crate::spaces::types::MemberAccess::WRITE),
            None,
        )
        .await
        .expect("authority may add members");
        // a non-admin cannot
        let err = super::add_member(
            &state,
            "did:plc:randomer",
            &space_uri,
            "did:plc:x",
            None,
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, crate::error::AppError::Forbidden(_)));
    }

    #[tokio::test]
    async fn invite_roundtrip() {
        require_test_db!();
        let (state, space_uri, authority) = service_test_db().await;
        let (_invite, token) = super::create_invite(
            &state,
            &authority,
            &space_uri,
            Some(crate::spaces::types::MemberAccess::WRITE),
            None,
            None,
        )
        .await
        .expect("authority creates invite");
        let (joined_uri, access) = super::accept_invite(&state, "did:plc:joiner", &token)
            .await
            .expect("joiner redeems invite");
        assert_eq!(joined_uri, space_uri);
        assert_eq!(access, crate::spaces::types::MemberAccess::WRITE);
        // now a member with write access
        let space = super::resolve_space(&state, &space_uri).await.unwrap();
        let acc = crate::spaces::members::is_member(
            &state.db,
            state.db_backend,
            &space.id,
            "did:plc:joiner",
        )
        .await
        .unwrap();
        assert_eq!(acc, Some(crate::spaces::types::MemberAccess::WRITE));
    }
}

#[cfg(test)]
mod native_write_bridge_tests {
    use super::*;

    #[test]
    fn swap_record_is_left_out_unless_given() {
        let body = with_swap_record(serde_json::json!({ "rkey": "a" }), None);
        assert!(body.get("swapRecord").is_none(), "{body}");

        let body = with_swap_record(serde_json::json!({ "rkey": "a" }), Some("bafyold"));
        assert_eq!(body["swapRecord"], "bafyold");
    }
    use crate::spaces::host_mode::HostMode;

    const USER: &str = "did:plc:aaaaaaaaaaaaaaaaaaaaaaaa";

    async fn state_with_space(mode: HostMode) -> (AppState, Space) {
        let pool = crate::test_support::migrated_memory_pool().await;
        let state = crate::test_support::test_state_with_pool(pool);

        let space = Space {
            id: "sp-bridge".into(),
            did: USER.into(),
            authority_did: USER.into(),
            creator_did: USER.into(),
            type_nsid: "com.example.forum".into(),
            skey: "main".into(),
            display_name: None,
            description: None,
            read_policy: Policy::MemberList,
            write_policy: Policy::MemberList,
            app_access: AppAccess::Open,
            config: SpaceConfig::default(),
            revision: None,
            created_at: now_rfc3339(),
            updated_at: now_rfc3339(),
        };
        db::create_space(&state.db, state.db_backend, &space)
            .await
            .expect("seed space");

        let mut conn = state.db.acquire().await.unwrap();
        let mut repo_state =
            db::get_or_create_repo_state(&mut conn, state.db_backend, &space.id, USER)
                .await
                .unwrap();
        repo_state.host_mode = mode;
        db::update_repo_state(&mut *conn, state.db_backend, &repo_state)
            .await
            .unwrap();

        (state, space)
    }

    #[tokio::test]
    async fn a_polyfill_write_stays_local() {
        let (state, space) = state_with_space(HostMode::Polyfill).await;
        let forwarded = forward_write_if_native(
            &state,
            &space,
            USER,
            "com.atproto.space.createRecord",
            serde_json::json!({}),
        )
        .await
        .expect("no error");
        assert!(forwarded.is_none(), "polyfill writes must not be forwarded");
    }

    #[tokio::test]
    async fn a_migrating_write_stays_local() {
        // HappyView is still authoritative until the handoff verifies, so a
        // write mid-migration belongs here, not on the PDS.
        let (state, space) = state_with_space(HostMode::Migrating).await;
        let forwarded = forward_write_if_native(
            &state,
            &space,
            USER,
            "com.atproto.space.createRecord",
            serde_json::json!({}),
        )
        .await
        .expect("no error");
        assert!(forwarded.is_none());
    }

    #[tokio::test]
    async fn a_native_write_without_a_session_fails() {
        let (state, space) = state_with_space(HostMode::Native).await;
        let err = forward_write_if_native(
            &state,
            &space,
            USER,
            "com.atproto.space.createRecord",
            serde_json::json!({}),
        )
        .await
        .expect_err("a native write with no session must not write locally");

        assert!(matches!(err, AppError::Forbidden(_)), "got {err:?}");
        assert!(
            format!("{err}").contains("re-authenticate"),
            "the error should tell the user what to do: {err}"
        );
    }

    #[tokio::test]
    async fn a_native_write_in_a_test_run_says_the_run_serves_no_pds_spaces() {
        let (mut state, space) = state_with_space(HostMode::Native).await;
        state.test = Some(std::sync::Arc::new(crate::testing::TestHarness::new(
            "src/a.test.lua",
        )));
        let err = forward_write_if_native(
            &state,
            &space,
            USER,
            "com.atproto.space.createRecord",
            serde_json::json!({}),
        )
        .await
        .expect_err("a test run cannot forward a write to a PDS");

        let AppError::XrpcError {
            status, message, ..
        } = &err
        else {
            panic!("got {err:?}");
        };
        assert_eq!(*status, axum::http::StatusCode::NOT_IMPLEMENTED);
        assert!(message.contains("test run"), "{message}");
        assert!(!message.contains("re-authenticate"), "{message}");
    }
}

#[cfg(test)]
mod invite_access_tests {
    use super::*;
    use crate::spaces::types::{AppAccess, Policy, SpaceConfig};

    const ADMIN: &str = "did:plc:aaaaaaaaaaaaaaaaaaaaaaaa";
    const JOINER: &str = "did:plc:bbbbbbbbbbbbbbbbbbbbbbbb";

    async fn state_with_space() -> (AppState, Space) {
        let pool = crate::test_support::migrated_memory_pool().await;
        let state = crate::test_support::test_state_with_pool(pool);

        let space = Space {
            id: "sp-invite".into(),
            did: ADMIN.into(),
            authority_did: ADMIN.into(),
            creator_did: ADMIN.into(),
            type_nsid: "com.example.forum".into(),
            skey: "main".into(),
            display_name: None,
            description: None,
            read_policy: Policy::MemberList,
            write_policy: Policy::MemberList,
            app_access: AppAccess::Open,
            config: SpaceConfig::default(),
            revision: None,
            created_at: now_rfc3339(),
            updated_at: now_rfc3339(),
        };
        db::create_space(&state.db, state.db_backend, &space)
            .await
            .expect("seed space");
        (state, space)
    }

    fn space_uri(space: &Space) -> String {
        format!(
            "at://{}/space/{}/{}",
            space.did, space.type_nsid, space.skey
        )
    }

    fn write_only() -> MemberAccess {
        MemberAccess {
            read: false,
            write: true,
            read_self: false,
        }
    }

    /// The combination the single access word could not hold: `as_wire_str`
    /// rendered any `write` as `"write"` and `parse_wire` read that back as
    /// read *and* write, so the row silently granted the read the caller
    /// withheld.
    ///
    /// Three reads, because they failed independently. `create_invite` returns
    /// the struct it built and so was right all along, which is what made this
    /// invisible: the caller was told the truth and the stored invite said
    /// something else.
    #[tokio::test]
    async fn a_write_only_invite_survives_storage() {
        let (state, space) = state_with_space().await;

        let (created, token) = create_invite(
            &state,
            ADMIN,
            &space_uri(&space),
            Some(write_only()),
            None,
            None,
        )
        .await
        .expect("mint the invite");
        assert_eq!(created.access, write_only(), "the create response");

        let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
        let fetched = db::get_invite_by_token_hash(&state.db, state.db_backend, &token_hash)
            .await
            .expect("read the invite back")
            .expect("the invite exists");
        assert_eq!(fetched.access, write_only(), "the stored row");

        let listed = db::list_invites(&state.db, state.db_backend, &space.id)
            .await
            .expect("list the invites");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].access, write_only(), "the invite list");
    }

    /// Redemption reads the row, so this is where the collapse actually cost
    /// something: the joiner became a full read/write member of a space whose
    /// admin had granted write alone.
    #[tokio::test]
    async fn redeeming_a_write_only_invite_grants_write_alone() {
        let (state, space) = state_with_space().await;

        let (_, token) = create_invite(
            &state,
            ADMIN,
            &space_uri(&space),
            Some(write_only()),
            None,
            None,
        )
        .await
        .expect("mint the invite");

        let (joined, granted) = accept_invite(&state, JOINER, &token)
            .await
            .expect("redeem the invite");
        assert_eq!(joined, space_uri(&space), "redemption names the space");
        assert_eq!(granted, write_only(), "the access redemption reports");

        let member = db::get_member(&state.db, state.db_backend, &space.id, JOINER)
            .await
            .expect("read the new member")
            .expect("the joiner is a member");
        assert_eq!(member.access, write_only(), "the membership row");
    }

    /// Every other combination the word could express still round-trips, so
    /// the migration's backfill has nothing left to get wrong. `read_self` is
    /// HappyView-local and reachable only through the script access word, but
    /// it is stored, so it is checked.
    #[tokio::test]
    async fn every_access_combination_round_trips() {
        let (state, space) = state_with_space().await;

        for access in [
            MemberAccess::READ,
            MemberAccess::WRITE,
            MemberAccess::READ_SELF,
            MemberAccess {
                read: false,
                write: false,
                read_self: false,
            },
            write_only(),
        ] {
            let (_, token) =
                create_invite(&state, ADMIN, &space_uri(&space), Some(access), None, None)
                    .await
                    .expect("mint the invite");
            let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
            let fetched = db::get_invite_by_token_hash(&state.db, state.db_backend, &token_hash)
                .await
                .expect("read the invite back")
                .expect("the invite exists");
            assert_eq!(fetched.access, access, "round-trip of {access:?}");
        }
    }

    /// The migration's backfill, against rows that actually predate it.
    ///
    /// A fresh database and a migrated one both cross this migration with
    /// nothing to convert, so the four mappings are exercised by no ordinary
    /// run. They matter: each has to be exactly what `parse_wire` made of that
    /// word, or an outstanding invite changes meaning as it crosses.
    ///
    /// The SQL is read from the migration rather than restated, so the test
    /// cannot drift from the thing it checks. The two backends' files are
    /// asserted byte-identical, which is what lets one execution speak for
    /// both — reversing the migration on the shared Postgres database would
    /// break it for every other test.
    #[tokio::test]
    async fn the_migration_backfills_each_access_word() {
        let (state, space) = state_with_space().await;

        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
        let name = "20260930000005_space_invite_booleans.sql";
        let sqlite_sql = std::fs::read_to_string(dir.join("sqlite").join(name)).expect("read it");
        let postgres_sql =
            std::fs::read_to_string(dir.join("postgres").join(name)).expect("read it");
        assert_eq!(
            sqlite_sql, postgres_sql,
            "the two backends' migrations have diverged, so this covers only one"
        );

        // Back to the shape the migration expects to find.
        for stmt in [
            "ALTER TABLE happyview_space_invites ADD COLUMN access TEXT NOT NULL DEFAULT 'read'",
            "ALTER TABLE happyview_space_invites DROP COLUMN can_read",
            "ALTER TABLE happyview_space_invites DROP COLUMN can_write",
            "ALTER TABLE happyview_space_invites DROP COLUMN read_self",
        ] {
            crate::db::query(stmt)
                .execute(&state.db)
                .await
                .expect("undo the migration");
        }

        let words = ["read", "write", "read_self", "none"];
        for word in words {
            crate::db::query(
                "INSERT INTO happyview_space_invites (id, space_id, token_hash, created_by, access, uses, revoked, created_at) \
                 VALUES (?, ?, ?, ?, ?, 0, 0, ?)",
            )
            .bind(word)
            .bind(&space.id)
            .bind(format!("hash-{word}"))
            .bind(ADMIN)
            .bind(word)
            .bind(now_rfc3339())
            .execute(&state.db)
            .await
            .expect("seed a pre-migration invite");
        }

        // The string comes off disk from this repo, not from a caller.
        sqlx::raw_sql(sqlx::AssertSqlSafe(sqlite_sql.clone()))
            .execute(&state.db)
            .await
            .expect("run the migration");

        for word in words {
            let row =
                db::get_invite_by_token_hash(&state.db, state.db_backend, &format!("hash-{word}"))
                    .await
                    .expect("read the invite back")
                    .expect("the invite survived");
            assert_eq!(
                row.access,
                MemberAccess::parse_wire(word).unwrap(),
                "'{word}' crossed the migration as something else"
            );
        }
    }
}
