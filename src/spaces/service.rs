use crate::AppState;
use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::error::AppError;
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
    if space.creator_did == did {
        return Ok(());
    }
    let sql = adapt_sql(
        "SELECT is_super FROM happyview_users WHERE did = ?",
        state.db_backend,
    );
    let row: Option<(i32,)> = crate::db::query_as(&sql)
        .bind(did)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to check admin status: {e}")))?;
    if row.is_some_and(|(is_super,)| is_super != 0) {
        return Ok(());
    }
    Err(AppError::Forbidden(
        "Only the space's creator can perform this action".into(),
    ))
}

pub(crate) fn check_collection_allowed(space: &Space, collection: &str) -> Result<(), AppError> {
    if let Some(serde_json::Value::Array(allowed)) = space.config.extra.get("allowedCollections")
        && !allowed.is_empty()
        && !allowed.iter().any(|v| v.as_str() == Some(collection))
    {
        return Err(AppError::BadRequest(format!(
            "collection '{collection}' is not allowed in this space"
        )));
    }
    Ok(())
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
    let encryption_key = state.config.token_encryption_key.as_ref().ok_or_else(|| {
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
pub(crate) async fn fetch_space_blob(
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
    encryption_key: &[u8; 32],
) -> Result<p256::ecdsa::SigningKey, AppError> {
    crate::verification_methods::ensure_atproto_space_method(pool, backend, encryption_key).await?;
    load_signing_key(pool, backend, encryption_key).await
}

/// Load the `#atproto_space` key. Read-only.
pub async fn load_signing_key(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    encryption_key: &[u8; 32],
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
    let encryption_key = state.config.token_encryption_key.as_ref().ok_or_else(|| {
        AppError::Internal(
            "spaces require a token encryption key to sign commits; set TOKEN_ENCRYPTION_KEY"
                .into(),
        )
    })?;
    load_signing_key(&state.db, state.db_backend, encryption_key).await
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

    for op in ops {
        let _ = notifications::dispatch_write_notification(
            &state.db,
            state.db_backend,
            &state.http,
            &space.id,
            author_did,
            &op.collection,
            &op.rkey,
            op.new_cid.as_deref(),
        )
        .await;
    }
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
async fn forward_write_if_native(
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
    let repo_state =
        db::get_or_create_repo_state(&mut conn, state.db_backend, &space.id, author_did).await?;
    drop(conn);

    if repo_state.host_mode.is_authoritative_here() {
        return Ok(None);
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
    check_collection_allowed(&space, collection)?;

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

    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| AppError::Internal(format!("failed to begin transaction: {e}")))?;
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
    check_collection_allowed(&space, collection)?;

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

    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| AppError::Internal(format!("failed to begin transaction: {e}")))?;

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

    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| AppError::Internal(format!("failed to begin transaction: {e}")))?;

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

/// The authority for a space this instance is about to create.
///
/// Peers find a space's credential key and host through its authority's DID
/// document, so the authority must be a DID whose document points here: this
/// instance's own. Without a published identity nothing resolves to this
/// instance, and the space is anchored on the creator's DID.
async fn space_authority_for_new_space(state: &AppState, creator_did: &str) -> String {
    match crate::auth::service_auth::instance_did(
        &state.db,
        state.db_backend,
        &state.config.public_url,
    )
    .await
    {
        Ok(did) => did,
        Err(e) => {
            tracing::debug!("creating a space under its creator's DID: {e}");
            creator_did.to_string()
        }
    }
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
) -> Result<Space, AppError> {
    if type_nsid.is_empty() || skey.is_empty() {
        return Err(AppError::BadRequest("type and skey are required".into()));
    }
    let authority = space_authority_for_new_space(state, did).await;
    let existing =
        db::get_space_by_address(&state.db, state.db_backend, &authority, type_nsid, skey).await?;
    if existing.is_some() {
        return Err(AppError::Conflict(
            "A space with this address already exists".into(),
        ));
    }
    let mut config = config.unwrap_or_default();
    if let Some(decl) = state.lexicons.get_space_declaration(type_nsid).await
        && let Some(collections) = decl.space_collections
        && !collections.is_empty()
        && !config.extra.contains_key("allowedCollections")
    {
        config.extra.insert(
            "allowedCollections".to_string(),
            serde_json::Value::Array(
                collections
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
    }
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
    db::add_member(&state.db, state.db_backend, &member).await?;
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
    db::add_member(&state.db, state.db_backend, &member).await?;
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
    db::update_space(&state.db, state.db_backend, &space).await?;
    Ok(space)
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

pub(crate) async fn create_invite(
    state: &AppState,
    actor_did: &str,
    space_ref: &str,
    access: Option<MemberAccess>,
    max_uses: Option<i64>,
    expires_at: Option<String>,
) -> Result<(SpaceInvite, String), AppError> {
    let space = resolve_space(state, space_ref).await?;
    require_space_admin(state, &space, actor_did).await?;
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
        && now_rfc3339() > *expires
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
    db::add_member(&state.db, state.db_backend, &member).await?;
    db::increment_invite_uses(&state.db, state.db_backend, &invite.id).await?;
    let space = db::get_space(&state.db, state.db_backend, &invite.space_id).await?;
    let space_uri = space
        .map(|s| format!("at://{}/space/{}/{}", s.did, s.type_nsid, s.skey))
        .ok_or_else(|| AppError::Internal("space vanished after member insert".into()))?;
    Ok((space_uri, member.access))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::db::DatabaseBackend;
    use crate::lexicon::LexiconRegistry;
    use tokio::sync::watch;

    macro_rules! require_test_db {
        () => {
            if std::env::var("TEST_DATABASE_URL").is_err() {
                eprintln!("skipped (TEST_DATABASE_URL not set)");
                return;
            }
        };
    }

    async fn service_empty_db() -> AppState {
        let url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL must be set for spaces::service integration tests");
        let backend = DatabaseBackend::from_url(&url);
        let pool = crate::db::connect(&url, backend).await;

        let config = Config {
            host: "127.0.0.1".into(),
            port: 0,
            database_url: String::new(),
            database_backend: backend,
            sqlite_journal_size_limit: crate::db::DEFAULT_JOURNAL_SIZE_LIMIT,
            public_url: String::new(),
            user_agent: String::new(),
            session_secret: "test-secret".into(),
            jetstream_url: String::new(),
            relay_url: String::new(),
            plc_url: String::new(),
            static_dir: String::new(),
            base_path: None,
            event_log_retention_days: 30,
            app_name: None,
            logo_uri: None,
            tos_uri: None,
            policy_uri: None,
            // Space writes need this to decrypt the `#atproto_space` signing key.
            token_encryption_key: Some(crate::test_support::TEST_ENCRYPTION_KEY),
            default_rate_limit_capacity: 100,
            default_rate_limit_refill_rate: 2.0,
            telemetry_collector_url: String::new(),
            plugin_cache_dir: None,
            platform_api_key_hash: None,
            job_worker_concurrency: 1,
        };
        let (collections_tx, _) = watch::channel(vec![]);
        let (labeler_subscriptions_tx, _) = watch::channel(());

        let atrium_http = std::sync::Arc::new(crate::http_retry::HappyViewHttpClient::default());
        let did_resolver = atrium_identity::did::CommonDidResolver::new(
            atrium_identity::did::CommonDidResolverConfig {
                plc_directory_url: "https://plc.directory".into(),
                http_client: std::sync::Arc::clone(&atrium_http),
            },
        );
        let handle_resolver = atrium_identity::handle::AtprotoHandleResolver::new(
            atrium_identity::handle::AtprotoHandleResolverConfig {
                dns_txt_resolver: crate::dns::NativeDnsResolver::new(),
                http_client: atrium_http,
            },
        );
        let oauth = atrium_oauth::OAuthClient::new(atrium_oauth::OAuthClientConfig {
            client_metadata: atrium_oauth::AtprotoLocalhostClientMetadata {
                redirect_uris: Some(vec!["http://127.0.0.1:0/auth/callback".into()]),
                scopes: Some(vec![atrium_oauth::Scope::Known(
                    atrium_oauth::KnownScope::Atproto,
                )]),
            },
            keys: None,
            state_store: crate::auth::oauth_store::DbStateStore::new(pool.clone(), backend),
            session_store: crate::auth::oauth_store::DbSessionStore::new(pool.clone(), backend),
            resolver: atrium_oauth::OAuthResolverConfig {
                did_resolver,
                handle_resolver,
                authorization_server_metadata: Default::default(),
                protected_resource_metadata: Default::default(),
            },
            http_client: crate::http_retry::HappyViewHttpClient::default(),
        })
        .expect("Failed to create test OAuth client");

        let state = AppState {
            config,
            http: reqwest::Client::new(),
            db: pool.clone(),
            backfill_db: pool.clone(),
            db_backend: backend,
            domain_cache: crate::domain::DomainCache::new(),
            lexicons: LexiconRegistry::new(),
            collections_tx,
            labeler_subscriptions_tx,
            rate_limiter: crate::rate_limit::RateLimiter::new(
                crate::rate_limit::RateLimitDefaults {
                    query_cost: 1,
                    procedure_cost: 1,
                    proxy_cost: 1,
                },
            ),
            oauth: std::sync::Arc::new(crate::auth::OAuthClientRegistry::new(std::sync::Arc::new(
                oauth,
            ))),
            oauth_state_store: crate::auth::oauth_store::DbStateStore::new(pool.clone(), backend),
            linked_repos_client: std::sync::Arc::new(
                crate::linked_repos::client::build(
                    "https://plc.directory",
                    "http://127.0.0.1:0/oauth-client-metadata.json",
                    "http://127.0.0.1:0",
                    "http://127.0.0.1:0/auth/callback".into(),
                    true,
                    vec![atrium_oauth::Scope::Known(
                        atrium_oauth::KnownScope::Atproto,
                    )],
                    crate::auth::oauth_store::DbStateStore::new(pool.clone(), backend),
                    pool.clone(),
                    backend,
                    None,
                )
                .expect("Failed to create test linked-repo OAuth client"),
            ),
            linked_repos_client_kid: None,
            cookie_key: axum_extra::extract::cookie::Key::derive_from(
                b"test-secret-for-tests-only-not-production",
            ),
            plugin_registry: std::sync::Arc::new(crate::plugin::PluginRegistry::new()),
            wasm_runtime: std::sync::Arc::new(
                crate::plugin::WasmRuntime::new().expect("wasm runtime"),
            ),
            attestation_signer: None,
            official_registry: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::plugin::official_registry::OfficialRegistryState::default(),
            )),
            official_registry_config: crate::plugin::official_registry::RegistryConfig::production(
            ),
            proxy_config: std::sync::Arc::new(arc_swap::ArcSwap::new(std::sync::Arc::new(
                crate::proxy_config::ProxyConfig::default(),
            ))),
            backfill_events_tx: tokio::sync::broadcast::channel(16).0,
            verbose_event_logging: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            client_jwks: Vec::new(),
            telemetry_counters: std::sync::Arc::new(crate::telemetry::counters::Counters::new()),
            script_limits: std::sync::Arc::new(crate::lua::limits::ScriptLimits::default()),
        };

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

    fn space_with_config(config: SpaceConfig) -> Space {
        Space {
            id: "space-id".into(),
            did: "did:plc:owner".into(),
            authority_did: "did:plc:owner".into(),
            creator_did: "did:plc:owner".into(),
            type_nsid: "com.example.forum".into(),
            skey: "main".into(),
            display_name: None,
            description: None,
            read_policy: Policy::MemberList,
            write_policy: Policy::MemberList,
            app_access: AppAccess::default(),
            config,
            revision: None,
            created_at: now_rfc3339(),
            updated_at: now_rfc3339(),
        }
    }

    #[test]
    fn check_collection_allowed_permits_listed_collection() {
        let space = space_with_config(config_with_allowed_collections(&["com.example.allowed"]));
        assert!(super::check_collection_allowed(&space, "com.example.allowed").is_ok());
    }

    #[test]
    fn check_collection_allowed_rejects_unlisted_collection() {
        let space = space_with_config(config_with_allowed_collections(&["com.example.allowed"]));
        let err = super::check_collection_allowed(&space, "com.example.denied").unwrap_err();
        match err {
            crate::error::AppError::BadRequest(msg) => {
                assert!(
                    msg.contains("not allowed"),
                    "expected 'not allowed' in message, got: {msg}"
                );
            }
            other => panic!("expected BadRequest, got: {other:?}"),
        }
    }

    #[test]
    fn check_collection_allowed_permits_any_collection_when_config_absent() {
        let space = space_with_config(SpaceConfig::default());
        assert!(super::check_collection_allowed(&space, "com.example.anything").is_ok());
    }

    #[test]
    fn check_collection_allowed_permits_any_collection_when_list_empty() {
        let space = space_with_config(config_with_allowed_collections(&[]));
        assert!(super::check_collection_allowed(&space, "com.example.anything").is_ok());
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
        let revoked = db::revoke_invite(&state.db, state.db_backend, &invite.id)
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

    #[tokio::test]
    async fn create_record_rejects_disallowed_collection() {
        require_test_db!();
        let (state, space_uri, member_did) =
            service_test_db_with_config(config_with_allowed_collections(&["com.example.allowed"]))
                .await;

        let err = super::create_record(
            &state,
            &member_did,
            None,
            &space_uri,
            "com.example.denied",
            serde_json::json!({ "text": "no" }),
        )
        .await
        .unwrap_err();
        match err {
            crate::error::AppError::BadRequest(msg) => {
                assert!(
                    msg.contains("not allowed"),
                    "expected 'not allowed' in message, got: {msg}"
                );
            }
            other => panic!("expected BadRequest, got: {other:?}"),
        }
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
    async fn put_record_rejects_disallowed_collection() {
        require_test_db!();
        let (state, space_uri, member_did) =
            service_test_db_with_config(config_with_allowed_collections(&["com.example.allowed"]))
                .await;

        let err = super::put_record(
            &state,
            &member_did,
            None,
            &space_uri,
            "com.example.denied",
            "fixedrkey-put-denied",
            serde_json::json!({ "text": "no" }),
            None,
        )
        .await
        .unwrap_err();
        match err {
            crate::error::AppError::BadRequest(msg) => {
                assert!(
                    msg.contains("not allowed"),
                    "expected 'not allowed' in message, got: {msg}"
                );
            }
            other => panic!("expected BadRequest, got: {other:?}"),
        }
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
        )
        .await
        .expect("create should succeed");
        assert_eq!(space.authority_did, creator_did);
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
        )
        .await
    }

    #[tokio::test]
    async fn creating_a_space_advertises_this_instance_as_space_host() {
        let state = crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        );
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

    async fn state_with_instance_identity() -> AppState {
        let state = crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        );
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
        state
    }

    #[tokio::test]
    async fn this_instance_is_the_authority_for_spaces_it_creates() {
        let state = state_with_instance_identity().await;
        let space = create_in_memory(&state, "did:plc:creator", "general")
            .await
            .expect("create should succeed");
        assert_eq!(space.authority_did, INSTANCE_DID);
        assert_eq!(space.did, INSTANCE_DID);
        assert_eq!(space.creator_did, "did:plc:creator");
    }

    #[tokio::test]
    async fn the_creator_administers_a_space_this_instance_is_authority_for() {
        let state = state_with_instance_identity().await;
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

    #[tokio::test]
    async fn creators_cannot_share_an_skey_under_this_instance() {
        let state = state_with_instance_identity().await;
        create_in_memory(&state, "did:plc:alice", "general")
            .await
            .unwrap();
        let err = create_in_memory(&state, "did:plc:bob", "general")
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)));
    }

    #[tokio::test]
    async fn without_an_instance_identity_the_creator_is_the_authority() {
        let state = crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        );
        let space = create_in_memory(&state, "did:plc:creator", "general")
            .await
            .unwrap();
        assert_eq!(space.authority_did, "did:plc:creator");
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
