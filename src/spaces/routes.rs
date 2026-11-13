use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::AppState;
use crate::auth::XrpcClaims;
use crate::db::now_rfc3339;
use crate::error::AppError;
use crate::spaces::scope::{check_delegation_token_access, check_read_access};
use crate::spaces::service;
use crate::spaces::types::*;
use crate::spaces::{db, notifications, oplog};
use crate::tid::generate_tid;

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LatestCommitQuery {
    space: String,
    did: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetRepoQuery {
    space: String,
    did: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListRepoOpsQuery {
    space: String,
    did: String,
    limit: Option<i64>,
    cursor: Option<String>,
    exclude_values: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegisterNotifyInput {
    space: String,
    /// The subscribing service identifier, resolved to its endpoint.
    service: Option<String>,
    /// The legacy shape names a DID and a webhook URL. Accepted until v3.
    service_did: Option<String>,
    endpoint: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnregisterNotifyInput {
    space: String,
    /// The subscriber's service identifier, as passed to registerNotify.
    service: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListBlobsQuery {
    space: String,
    repo: String,
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotifyWriteInput {
    space: String,
    /// The repo that advanced, with its new `repoRev` and `data`.
    repo: Option<String>,
    repo_rev: Option<String>,
    /// The alpha lexicon's name for `repoRev`. Accepted until v3.
    rev: Option<String>,
    data: Option<LexBytes>,
    /// The name for `data` before proposal 0016. Accepted until every repo
    /// host sends `data`.
    hash: Option<LexBytes>,
    /// The legacy shape names one record instead of the repo's new state.
    /// Accepted until v3.
    did: Option<String>,
    collection: Option<String>,
    rkey: Option<String>,
    cid: Option<String>,
}

/// A lexicon `bytes` value, encoded in JSON as `{"$bytes": "<base64>"}`.
#[derive(Deserialize)]
struct LexBytes {
    #[serde(rename = "$bytes")]
    bytes: String,
}

impl LexBytes {
    fn decode(&self) -> Result<Vec<u8>, AppError> {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD_NO_PAD
            .decode(self.bytes.trim_end_matches('='))
            .map_err(|_| AppError::BadRequest("data is not valid base64".into()))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListReposQuery {
    space: String,
    limit: Option<i64>,
    /// A space revision: list only repos updated after it.
    cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotifySpaceDeletedInput {
    space: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetDelegationTokenQuery {
    space: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SpaceUriQuery {
    space: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListSpacesQuery {
    /// Whose spaces to list: the caller's own unless a super admin asks for
    /// another account's. HappyView's meaning, not the Oct 29 lexicon's, which
    /// used `did` to filter by authority.
    did: Option<String>,
    /// Lists only spaces under this authority DID (Nov 12).
    space_did: Option<String>,
    /// Lists only spaces of this type. `type` is the earlier name, accepted
    /// until v3.
    #[serde(rename = "spaceType", alias = "type")]
    space_type: Option<String>,
    limit: Option<i64>,
    cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PutRecordInput {
    space: String,
    collection: String,
    rkey: String,
    record: serde_json::Value,
    swap_record: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteRecordInput {
    space: String,
    collection: String,
    rkey: String,
    swap_record: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetRecordQuery {
    space: String,
    repo: Option<String>,
    collection: String,
    rkey: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListRecordsQuery {
    space: String,
    repo: Option<String>,
    collection: Option<String>,
    limit: Option<i64>,
    cursor: Option<String>,
    reverse: Option<bool>,
    include_values: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateInviteInput {
    space: String,
    access: Option<MemberAccess>,
    max_uses: Option<i64>,
    expires_at: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RedeemInviteInput {
    token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RevokeInviteInput {
    space: String,
    invite_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetSpaceCredentialInput {
    space: String,
    /// The P-256 did:key the credential is bound to, as the Nov 12 text names
    /// it. Every request using the credential is signed by it.
    credential_key: Option<String>,
    /// The same key under its Oct 29 name.
    credential_kid: Option<String>,
    /// Establishes the user being acted for, for spaces that gate on users.
    delegation_token: Option<String>,
    /// Identifies the app, for spaces that gate on it.
    client_attestation: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateRecordInput {
    space: String,
    collection: String,
    record: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetSpaceBlobQuery {
    space: String,
    cid: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApplyWritesInput {
    space: String,
    swap_commit: Option<String>,
    writes: Vec<WriteOp>,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "camelCase")]
enum WriteOp {
    Create {
        collection: String,
        rkey: Option<String>,
        value: serde_json::Value,
    },
    Update {
        collection: String,
        rkey: String,
        value: serde_json::Value,
        #[serde(rename = "swapRecord")]
        swap_record: Option<String>,
    },
    Delete {
        collection: String,
        rkey: String,
        #[serde(rename = "swapRecord")]
        swap_record: Option<String>,
    },
}

/// The PDS's `applyWrites` body for a batch: each write tagged by `$type`, as
/// `com.atproto.space.applyWrites` takes it, where this route takes `action`.
/// Optional fields are sent only when the caller gave them.
fn pds_apply_writes_body(
    space_uri: &str,
    repo: &str,
    swap_commit: Option<&str>,
    writes: &[WriteOp],
) -> serde_json::Value {
    let writes: Vec<serde_json::Value> = writes
        .iter()
        .map(|write| {
            let (kind, collection, rkey, value, swap_record) = match write {
                WriteOp::Create {
                    collection,
                    rkey,
                    value,
                } => ("create", collection, rkey.as_deref(), Some(value), None),
                WriteOp::Update {
                    collection,
                    rkey,
                    value,
                    swap_record,
                } => (
                    "update",
                    collection,
                    Some(rkey.as_str()),
                    Some(value),
                    swap_record.as_deref(),
                ),
                WriteOp::Delete {
                    collection,
                    rkey,
                    swap_record,
                } => (
                    "delete",
                    collection,
                    Some(rkey.as_str()),
                    None,
                    swap_record.as_deref(),
                ),
            };
            let mut op = serde_json::json!({
                "$type": format!("com.atproto.space.applyWrites#{kind}"),
                "collection": collection,
            });
            if let Some(rkey) = rkey {
                op["rkey"] = rkey.into();
            }
            if let Some(value) = value {
                op["value"] = value.clone();
            }
            if let Some(swap) = swap_record {
                op["swapRecord"] = swap.into();
            }
            op
        })
        .collect();
    let mut body = serde_json::json!({ "space": space_uri, "repo": repo, "writes": writes });
    if let Some(swap) = swap_commit {
        body["swapCommit"] = swap.into();
    }
    body
}

// ---------------------------------------------------------------------------
// Route registration
// ---------------------------------------------------------------------------

const PROTO_NS: &str = "com.atproto";
const LEGACY_NS: &str = "dev.happyview";

/// Every protocol method this build serves, paired with its handler.
///
/// `space_routes` and the capability descriptor are both derived from this, so
/// the advertisement cannot claim a method that does not route, or omit one that
/// does. Legacy `dev.happyview.*` aliases are absent: they are not
/// protocol methods and must not be advertised.
pub(crate) fn protocol_method_table() -> Vec<(String, MethodRouter<AppState>)> {
    let mut table = crate::spaces::lifecycle::lifecycle_method_table();
    table.extend(vec![
        (format!("{PROTO_NS}.space.listSpaces"), get(list_spaces)),
        (format!("{PROTO_NS}.space.getRecord"), get(get_record)),
        (format!("{PROTO_NS}.space.listRecords"), get(list_records)),
        (
            format!("{PROTO_NS}.space.getLatestCommit"),
            get(get_latest_commit),
        ),
        // Kept as a backward-compatible alias of getLatestCommit, and advertised
        // because other implementations still call it by this name.
        (
            format!("{PROTO_NS}.space.getRepoState"),
            get(get_latest_commit),
        ),
        (format!("{PROTO_NS}.space.getRepo"), get(get_repo)),
        (format!("{PROTO_NS}.space.listRepoOps"), get(list_repo_ops)),
        (format!("{PROTO_NS}.space.listRepos"), get(list_repos)),
        (
            format!("{PROTO_NS}.space.getDelegationToken"),
            get(get_delegation_token),
        ),
        (
            format!("{PROTO_NS}.space.getSpaceCredential"),
            post(get_space_credential),
        ),
        (
            format!("{PROTO_NS}.space.createRecord"),
            post(create_record),
        ),
        (format!("{PROTO_NS}.space.putRecord"), post(put_record)),
        (
            format!("{PROTO_NS}.space.deleteRecord"),
            post(delete_record),
        ),
        (format!("{PROTO_NS}.space.applyWrites"), post(apply_writes)),
        (
            format!("{PROTO_NS}.space.registerNotify"),
            post(register_notify),
        ),
        (
            format!("{PROTO_NS}.space.unregisterNotify"),
            post(unregister_notify),
        ),
        (format!("{PROTO_NS}.space.listBlobs"), get(list_blobs)),
        (format!("{PROTO_NS}.space.notifyWrite"), post(notify_write)),
        (
            format!("{PROTO_NS}.space.notifySpaceDeleted"),
            post(notify_space_deleted),
        ),
        (
            format!("{PROTO_NS}.space.notifyCredentialRevoked"),
            post(notify_credential_revoked),
        ),
        (format!("{PROTO_NS}.space.getBlob"), get(get_space_blob)),
    ]);
    table
}

pub fn space_routes() -> Router<AppState> {
    let mut router = Router::new();
    for (nsid, handler) in protocol_method_table() {
        router = router.route(&format!("/xrpc/{nsid}"), handler);
    }

    router
        // Invites (HappyView extension, no com.atproto equivalent). Not in the
        // protocol table because they are not protocol methods, so they are
        // served but never advertised.
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.createInvite"),
            post(create_invite),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.acceptInvite"),
            post(accept_invite),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.revokeInvite"),
            post(revoke_invite),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.listInvites"),
            get(list_invites),
        )
        // Backward-compatible aliases (dev.happyview.space.*) — kept until v3
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.listSpaces"),
            get(list_spaces),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.getRecord"),
            get(get_record),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.listRecords"),
            get(list_records),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.getMemberGrant"),
            get(get_delegation_token),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.createRecord"),
            post(create_record),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.putRecord"),
            post(put_record),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.deleteRecord"),
            post(delete_record),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.applyWrites"),
            post(apply_writes),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.getBlob"),
            get(get_space_blob),
        )
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Both gates on a space read: the member list, then the app's `space:` grant.
///
/// `check_read_access` answers what the *user* may do; the scope answers what
/// the *app* may do on their behalf. A space credential satisfies both, because
/// the authority issues it to grant whole-space read, so the scope gate applies
/// only to OAuth-authenticated callers.
async fn authorize_space_read(
    state: &AppState,
    xrpc_claims: &XrpcClaims,
    caller_did: &str,
    target_repo_did: &str,
    space: &crate::spaces::types::Space,
    membership: crate::spaces::types::MemberAccess,
    has_credential: bool,
) -> Result<(), AppError> {
    check_read_access(caller_did, target_repo_did, membership, has_credential)?;

    if has_credential {
        return require_audience(xrpc_claims, target_repo_did);
    }
    let Some(identity) = xrpc_claims.identity.as_ref() else {
        return Ok(());
    };

    // A caller reading only their own repo needs just `read_self`, which `read`
    // also satisfies. Asking for `read` here would reject an app granted only
    // the narrower scope.
    let target = if caller_did == target_repo_did {
        happyview_scopes::SpaceTarget::ReadSelf
    } else {
        happyview_scopes::SpaceTarget::Read
    };
    crate::spaces::scope::require_space_scope(state, identity, space, target).await
}

/// A credential-authenticated request must be addressed to the DID it is for:
/// the repo it reads, or the space authority for space-wide methods. Otherwise a
/// host serving one repo could replay the request against another.
fn require_audience(claims: &XrpcClaims, expected: &str) -> Result<(), AppError> {
    if claims.space_credential.is_none() || claims.space_audience.as_deref() == Some(expected) {
        return Ok(());
    }
    Err(AppError::XrpcError {
        status: StatusCode::UNAUTHORIZED,
        code: "BadSpaceAudience",
        message: format!("request is not addressed to {expected}"),
    })
}

fn require_auth(claims: &XrpcClaims) -> Result<&crate::auth::Claims, AppError> {
    claims
        .identity
        .as_ref()
        .ok_or_else(|| AppError::Auth("This endpoint requires authentication".into()))
}

/// Like `require_auth`, but also accepts a verified space credential as an
/// identity source, for space endpoints that serve syncers as well as accounts.
async fn require_auth_or_credential(
    state: &AppState,
    claims: &XrpcClaims,
) -> Result<String, AppError> {
    if let Some(identity) = &claims.identity {
        return Ok(identity.did().to_string());
    }

    if let Some(token) = &claims.space_credential {
        let space_uri = crate::spaces::credential::peek_credential_sub(token)
            .ok_or_else(|| AppError::Auth("invalid space credential".into()))?;
        let space = service::resolve_space(state, &space_uri).await?;
        let verified = service::verify_space_credential(state, &space, token).await?;
        return Ok(verified.sub);
    }

    Err(AppError::Auth(
        "This endpoint requires authentication".into(),
    ))
}

/// Resolve the caller's DID from an authenticated identity for the inter-service
/// notify routes: a DPoP/cookie identity or a verified service-auth JWT. (Space
/// credentials are not accepted here — their subject is a space, not the
/// authority DID these routes gate on.)
/// Refuse service auth addressed to another space's DID. A token addressed to
/// the instance, as a space created before space DIDs existed is addressed,
/// is not bound to one space.
fn require_service_audience(claims: &XrpcClaims, space: &Space) -> Result<(), AppError> {
    match claims
        .service_auth
        .as_ref()
        .and_then(|sa| sa.space_audience.as_deref())
    {
        Some(audience) if audience != space.authority_did => Err(AppError::XrpcError {
            status: StatusCode::UNAUTHORIZED,
            code: "BadServiceAudience",
            message: format!("service auth is addressed to {audience}, not this space"),
        }),
        _ => Ok(()),
    }
}

fn require_notify_caller(claims: &XrpcClaims) -> Result<String, AppError> {
    if let Some(identity) = &claims.identity {
        return Ok(identity.did().to_string());
    }
    if let Some(service_auth) = &claims.service_auth {
        return Ok(service_auth.did.clone());
    }
    Err(AppError::Auth(
        "This endpoint requires authentication".into(),
    ))
}

// ---------------------------------------------------------------------------
// Space read handlers
// ---------------------------------------------------------------------------

async fn list_spaces(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Query(query): Query<ListSpacesQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let caller = claims.did();
    let did = query.did.unwrap_or_else(|| caller.to_string());
    // Another account's memberships are its own business; only a super admin
    // may ask for them, as with `getSpace`.
    if did != caller && !service::is_super_admin(&state, caller).await? {
        return Err(AppError::Forbidden(
            "listSpaces lists only the caller's own spaces".into(),
        ));
    }
    let limit = query.limit.unwrap_or(50).min(100);

    let (views, cursor) = db::list_spaces_for_user(
        &state.db,
        state.db_backend,
        &did,
        query.space_type.as_deref(),
        query.space_did.as_deref(),
        limit,
        query.cursor.as_deref(),
    )
    .await?;

    // Each view carries its reference under both names: `uri` (Oct 29) and
    // `space` (Nov 12), and the Nov 12 repo and space revisions.
    let spaces_json: Vec<serde_json::Value> = views
        .into_iter()
        .map(|v| {
            let mut view = serde_json::json!({
                "uri": v.uri,
                "space": v.uri,
                "isOwner": v.is_owner,
            });
            if let Some(rev) = v.repo_rev {
                view["repoRev"] = rev.into();
            }
            if let Some(data) = v.repo_data {
                view["data"] = crate::spaces::commit::encode_lex_bytes(&data);
            }
            if v.is_owner
                && let Some(rev) = v.space_rev
            {
                view["spaceRev"] = rev.into();
            }
            view
        })
        .collect();

    Ok(Json(serde_json::json!({
        "spaces": spaces_json,
        "cursor": cursor,
    })))
}

// ---------------------------------------------------------------------------
// Record handlers
// ---------------------------------------------------------------------------

/// The app's `space:` grant for a record write.
///
/// Writes are attributed to the authoring user, so unlike reads they are never
/// satisfied by a space credential; the spec accepts only an OAuth credential
/// here. The service layer checks membership.
async fn authorize_space_write(
    state: &AppState,
    xrpc_claims: &XrpcClaims,
    space_ref: &str,
    collection: &str,
    action: happyview_scopes::SpaceAction,
) -> Result<(), AppError> {
    let Some(identity) = xrpc_claims.identity.as_ref() else {
        return Ok(());
    };
    let space = service::resolve_space(state, space_ref).await?;
    crate::spaces::scope::require_space_scope(
        state,
        identity,
        &space,
        happyview_scopes::SpaceTarget::Write { action, collection },
    )
    .await
}

async fn create_record(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<CreateRecordInput>,
) -> Result<Response, AppError> {
    let did = require_auth_or_credential(&state, &xrpc_claims).await?;
    authorize_space_write(
        &state,
        &xrpc_claims,
        &input.space,
        &input.collection,
        happyview_scopes::SpaceAction::Create,
    )
    .await?;
    let (uri, cid) = service::create_record(
        &state,
        &did,
        xrpc_claims.space_credential.as_deref(),
        &input.space,
        &input.collection,
        input.record,
    )
    .await?;
    let mut response = Json(serde_json::json!({ "uri": uri, "cid": cid })).into_response();
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

async fn put_record(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<PutRecordInput>,
) -> Result<Response, AppError> {
    let did = require_auth_or_credential(&state, &xrpc_claims).await?;
    // putRecord creates or updates, so it needs both.
    for action in [
        happyview_scopes::SpaceAction::Create,
        happyview_scopes::SpaceAction::Update,
    ] {
        authorize_space_write(
            &state,
            &xrpc_claims,
            &input.space,
            &input.collection,
            action,
        )
        .await?;
    }
    let (uri, cid) = service::put_record(
        &state,
        &did,
        xrpc_claims.space_credential.as_deref(),
        &input.space,
        &input.collection,
        &input.rkey,
        input.record,
        input.swap_record,
    )
    .await?;
    let mut response = Json(serde_json::json!({ "uri": uri, "cid": cid })).into_response();
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

async fn delete_record(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<DeleteRecordInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let did = claims.did().to_string();
    authorize_space_write(
        &state,
        &xrpc_claims,
        &input.space,
        &input.collection,
        happyview_scopes::SpaceAction::Delete,
    )
    .await?;
    service::delete_record(
        &state,
        &did,
        &input.space,
        &input.collection,
        &input.rkey,
        input.swap_record,
    )
    .await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

async fn apply_writes(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<ApplyWritesInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let did = require_auth_or_credential(&state, &xrpc_claims).await?;
    let space = service::resolve_space(&state, &input.space).await?;
    service::require_membership(
        &state,
        &space,
        &did,
        true,
        xrpc_claims.space_credential.as_deref(),
    )
    .await?;

    // Every op in the batch must be individually covered: a grant for one
    // collection must not carry a write to another just because they arrived
    // together.
    if let Some(identity) = xrpc_claims.identity.as_ref() {
        for write in &input.writes {
            let (collection, action) = match write {
                WriteOp::Create { collection, .. } => {
                    (collection, happyview_scopes::SpaceAction::Create)
                }
                WriteOp::Update { collection, .. } => {
                    (collection, happyview_scopes::SpaceAction::Update)
                }
                WriteOp::Delete { collection, .. } => {
                    (collection, happyview_scopes::SpaceAction::Delete)
                }
            };
            crate::spaces::scope::require_space_scope(
                &state,
                identity,
                &space,
                happyview_scopes::SpaceTarget::Write { action, collection },
            )
            .await?;
        }
    }

    // A repo hosted on its author's PDS takes the batch there, as the single
    // writes do; writing only HappyView's copy would fork the two, and the
    // next sync would drop what was written here.
    let space_uri = format!(
        "at://{}/space/{}/{}",
        space.did, space.type_nsid, space.skey
    );
    if let Some(forwarded) = service::forward_write_if_native(
        &state,
        &space,
        &did,
        "com.atproto.space.applyWrites",
        pds_apply_writes_body(
            &space_uri,
            &did,
            input.swap_commit.as_deref(),
            &input.writes,
        ),
    )
    .await?
    {
        return Ok(Json(forwarded));
    }

    if let Some(ref expected_rev) = input.swap_commit {
        match &space.revision {
            Some(current_rev) if current_rev != expected_rev => {
                return Err(AppError::Conflict("swapCommit mismatch".into()));
            }
            None if !expected_rev.is_empty() => {
                return Err(AppError::Conflict("swapCommit mismatch".into()));
            }
            _ => {}
        }
    }

    let mut results = Vec::with_capacity(input.writes.len());
    let rev = generate_tid();
    let mut ops: Vec<service::AppliedOp> = Vec::with_capacity(input.writes.len());

    // Resolve the signing key before opening the transaction: provisioning the
    // `#atproto_space` key writes, and doing that on a second connection while a
    // transaction is open deadlocks on SQLite.
    let signing_key = service::service_signing_key(&state).await?;

    let mut tx = service::begin_space_write(&state, &space.id).await?;

    for op in input.writes {
        match op {
            WriteOp::Create {
                collection,
                rkey,
                value,
            } => {
                let rkey = rkey.unwrap_or_else(generate_tid);
                let cid = service::content_cid(&value)?;
                let record_uri = format!(
                    "at://{}/space/{}/{}/{}/{}/{}",
                    space.did, space.type_nsid, space.skey, did, collection, rkey
                );
                let record = SpaceRecord {
                    uri: record_uri.clone(),
                    space_id: space.id.clone(),
                    author_did: did.clone(),
                    collection: collection.clone(),
                    rkey: rkey.clone(),
                    record: value,
                    cid: cid.clone(),
                    indexed_at: now_rfc3339(),
                };
                db::insert_space_record(&mut *tx, state.db_backend, &record).await?;
                ops.push(service::AppliedOp {
                    action: OplogAction::Create,
                    collection,
                    rkey,
                    new_cid: Some(cid.clone()),
                    old_cid: None,
                });
                results.push(serde_json::json!({
                    "uri": record_uri,
                    "cid": cid,
                }));
            }
            WriteOp::Update {
                collection,
                rkey,
                value,
                swap_record,
            } => {
                let cid = service::content_cid(&value)?;
                let record_uri = format!(
                    "at://{}/space/{}/{}/{}/{}/{}",
                    space.did, space.type_nsid, space.skey, did, collection, rkey
                );
                let record = SpaceRecord {
                    uri: record_uri.clone(),
                    space_id: space.id.clone(),
                    author_did: did.clone(),
                    collection: collection.clone(),
                    rkey: rkey.clone(),
                    record: value,
                    cid: cid.clone(),
                    indexed_at: now_rfc3339(),
                };
                let old_cid = match swap_record.as_deref() {
                    Some(swap) => Some(swap.to_string()),
                    None => db::get_space_record(&mut *tx, state.db_backend, &record_uri)
                        .await?
                        .map(|r| r.cid),
                };
                if let Some(swap_cid) = swap_record {
                    db::upsert_space_record_with_swap(
                        &mut tx,
                        state.db_backend,
                        &record,
                        &swap_cid,
                    )
                    .await?;
                } else {
                    db::upsert_space_record(&mut *tx, state.db_backend, &record).await?;
                }
                ops.push(service::AppliedOp {
                    action: if old_cid.is_some() {
                        OplogAction::Update
                    } else {
                        OplogAction::Create
                    },
                    collection,
                    rkey,
                    new_cid: Some(cid.clone()),
                    old_cid,
                });
                results.push(serde_json::json!({
                    "uri": record_uri,
                    "cid": cid,
                }));
            }
            WriteOp::Delete {
                collection,
                rkey,
                swap_record,
            } => {
                let record_uri = format!(
                    "at://{}/space/{}/{}/{}/{}/{}",
                    space.did, space.type_nsid, space.skey, did, collection, rkey
                );
                let old_cid = if let Some(swap_cid) = swap_record {
                    db::delete_space_record_with_swap(
                        &mut tx,
                        state.db_backend,
                        &record_uri,
                        &swap_cid,
                    )
                    .await?;
                    swap_cid
                } else {
                    // Mirrors `service::delete_record`'s non-swap ownership
                    // check: only the record's own author may delete it.
                    let existing =
                        db::get_space_record(&mut *tx, state.db_backend, &record_uri).await?;
                    let cid = match existing {
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
                ops.push(service::AppliedOp {
                    action: OplogAction::Delete,
                    collection,
                    rkey,
                    new_cid: None,
                    old_cid: Some(old_cid),
                });
                results.push(serde_json::json!({}));
            }
        }
    }

    let committed = service::commit_write(
        &mut tx,
        state.db_backend,
        &space,
        &did,
        &rev,
        &ops,
        &signing_key,
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("failed to commit transaction: {e}")))?;

    service::notify_ops(&state, &space, &did, &ops, &rev, committed).await;

    Ok(Json(serde_json::json!({
        "results": results,
    })))
}

async fn get_record(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Query(query): Query<GetRecordQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let did = require_auth_or_credential(&state, &xrpc_claims).await?;
    let space = service::resolve_space(&state, &query.space).await?;
    let has_credential = xrpc_claims.space_credential.is_some();
    let membership = service::require_membership(
        &state,
        &space,
        &did,
        false,
        xrpc_claims.space_credential.as_deref(),
    )
    .await?;

    // A credential is addressed to the repo it reads, and a read_self member
    // can read only their own, so either names the repo when the query does not.
    let repo = match (query.repo.as_deref(), xrpc_claims.space_audience.as_deref()) {
        (Some(repo), _) => db::RecordRepo::Author(repo),
        (None, Some(audience)) if has_credential => db::RecordRepo::Author(audience),
        (None, _) if membership.restricted_to_own_records() => db::RecordRepo::Author(&did),
        (None, _) => db::RecordRepo::Prefer(&did),
    };
    let record = db::get_space_record_by_parts(
        &state.db,
        state.db_backend,
        &space.id,
        repo,
        &query.collection,
        &query.rkey,
    )
    .await?
    .ok_or_else(|| AppError::NotFound("Record not found".into()))?;

    authorize_space_read(
        &state,
        &xrpc_claims,
        &did,
        &record.author_did,
        &space,
        membership,
        has_credential,
    )
    .await?;

    Ok(Json(serde_json::json!({
        "uri": record.uri,
        "cid": record.cid,
        "value": record.record,
    })))
}

async fn list_records(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Query(query): Query<ListRecordsQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let did = require_auth_or_credential(&state, &xrpc_claims).await?;
    let space = service::resolve_space(&state, &query.space).await?;
    let has_credential = xrpc_claims.space_credential.is_some();
    let membership = service::require_membership(
        &state,
        &space,
        &did,
        false,
        xrpc_claims.space_credential.as_deref(),
    )
    .await?;
    // Listing one repo is a repo read; listing across repos is the space host's.
    require_audience(
        &xrpc_claims,
        query.repo.as_deref().unwrap_or(&space.authority_did),
    )?;

    // read_self members may only list their own records regardless of what the caller requests
    let repo = if !has_credential && membership.restricted_to_own_records() {
        Some(did.as_str())
    } else {
        query.repo.as_deref().or(if has_credential {
            None
        } else {
            Some(did.as_str())
        })
    };
    authorize_space_read(
        &state,
        &xrpc_claims,
        &did,
        repo.unwrap_or(&space.authority_did),
        &space,
        membership,
        has_credential,
    )
    .await?;

    let limit = query.limit.unwrap_or(50).min(100);
    let reverse = query.reverse.unwrap_or(false);
    let (records, cursor) = db::list_space_records(
        &state.db,
        state.db_backend,
        &space.id,
        repo,
        query.collection.as_deref(),
        limit,
        query.cursor.as_deref(),
        reverse,
    )
    .await?;

    let include_values = query.include_values.unwrap_or(false);
    let records_json: Vec<serde_json::Value> = records
        .into_iter()
        .map(|r| {
            let mut rec = serde_json::json!({
                "collection": r.collection,
                "rkey": r.rkey,
                "cid": r.cid,
            });
            if include_values {
                rec["value"] = r.record;
            }
            rec
        })
        .collect();

    Ok(Json(serde_json::json!({
        "records": records_json,
        "cursor": cursor,
    })))
}

// ---------------------------------------------------------------------------
// Invite handlers
// ---------------------------------------------------------------------------

async fn create_invite(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<CreateInviteInput>,
) -> Result<Response, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let (invite, token) = service::create_invite(
        &state,
        claims.did(),
        &input.space,
        input.access,
        input.max_uses,
        input.expires_at,
    )
    .await?;

    let mut response = Json(serde_json::json!({
        "inviteId": invite.id,
        "token": token,
        "access": invite.access,
        "maxUses": invite.max_uses,
        "expiresAt": invite.expires_at,
    }))
    .into_response();
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

async fn accept_invite(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<RedeemInviteInput>,
) -> Result<Response, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let (space_uri, access) = service::accept_invite(&state, claims.did(), &input.token).await?;

    let mut response = Json(serde_json::json!({
        "uri": space_uri,
        "access": access,
    }))
    .into_response();
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

async fn revoke_invite(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<RevokeInviteInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let space = service::resolve_space(&state, &input.space).await?;
    service::require_space_admin(&state, &space, claims.did()).await?;

    let revoked =
        db::revoke_invite(&state.db, state.db_backend, &space.id, &input.invite_id).await?;
    if !revoked {
        return Err(AppError::NotFound("Invite not found".into()));
    }

    Ok(Json(serde_json::json!({ "success": true })))
}

async fn list_invites(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Query(query): Query<SpaceUriQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let space = service::resolve_space(&state, &query.space).await?;
    service::require_space_admin(&state, &space, claims.did()).await?;

    let invites = db::list_invites(&state.db, state.db_backend, &space.id).await?;

    let invites_json: Vec<serde_json::Value> = invites
        .into_iter()
        .map(|i| {
            serde_json::json!({
                "id": i.id,
                "access": i.access,
                "maxUses": i.max_uses,
                "uses": i.uses,
                "expiresAt": i.expires_at,
                "revoked": i.revoked,
                "createdBy": i.created_by,
                "createdAt": i.created_at,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({ "invites": invites_json })))
}

// ---------------------------------------------------------------------------
// Credential handlers
// ---------------------------------------------------------------------------

async fn get_delegation_token(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Query(params): Query<GetDelegationTokenQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let did = claims.did().to_string();
    let space = service::resolve_space(&state, &params.space).await?;

    // Membership alone, not `require_membership`: a delegation token reads no
    // space data, and is how a member asks the managing app, if any, for a
    // credential.
    let membership = service::require_on_member_list(&state, &space, &did).await?;
    check_delegation_token_access(membership, false)?;

    // A delegation token is exchanged for a credential granting whole-space
    // read, so it needs the full `read` action. Per the spec, an app holding
    // only read_self "cannot reach the rest of the space".
    crate::spaces::scope::require_space_scope(
        &state,
        claims,
        &space,
        happyview_scopes::SpaceTarget::Read,
    )
    .await?;

    let encryption_key = &state.config.keyring().ok_or_else(|| {
        AppError::Internal("TOKEN_ENCRYPTION_KEY is required for space credentials".into())
    })?;

    let signing_key =
        crate::spaces::delegation_key::load_or_create(&state.db, state.db_backend, encryption_key)
            .await?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let exp = now + crate::spaces::credential::DELEGATION_TOKEN_TTL_SECS;

    let space_uri = format!(
        "at://{}/space/{}/{}",
        space.did, space.type_nsid, space.skey
    );
    // Proposal 0016 addresses a delegation token to the space host as the
    // authority's `#atproto_pds`.
    let space_host = format!("{}#atproto_pds", space.did);
    let delegation_claims = crate::spaces::credential::DelegationTokenClaims {
        iss: did,
        sub: space_uri,
        aud: space_host,
        iat: now,
        exp,
        jti: crate::spaces::credential::make_jti(),
    };

    let grant = crate::spaces::credential::sign_delegation_token(&delegation_claims, &signing_key)?;

    let expires_at = chrono::DateTime::from_timestamp(exp as i64, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default();

    Ok(Json(serde_json::json!({
        "token": grant,
        // The earlier name for `token`. Returned until v3.
        "delegationToken": grant,
        "expiresAt": expires_at,
    })))
}

// ---------------------------------------------------------------------------
// Protocol endpoint implementations
// ---------------------------------------------------------------------------

/// The commit to serve for a repo, in whichever model its state holds.
///
/// A repo synced from a 0016 host holds only its hash: the MAC that
/// authenticated it was for that one request. Until HappyView can MAC a commit
/// itself (HAP-192), it serves such a commit in the 0016 shape with no MAC,
/// which 0016 permits under OAuth. `space` is echoed as the caller named it,
/// as the reference does.
fn repo_state_commit_json(
    repo_state: &RepoState,
    space: &str,
) -> Result<Option<serde_json::Value>, AppError> {
    let Some(h) = repo_state.hash.as_deref() else {
        return Ok(None);
    };
    // The lexicon types these as `bytes`, which atproto renders as
    // `{"$bytes": "<standard base64>"}`. Other implementations cannot parse a
    // commit that carries bare base64url strings instead.
    use crate::spaces::commit::encode_lex_bytes;
    match (
        repo_state.ikm.as_deref(),
        repo_state.sig.as_deref(),
        repo_state.mac.as_deref(),
    ) {
        (Some(ikm), Some(sig), Some(mac)) => Ok(Some(serde_json::json!({
            "ver": 1,
            "hash": encode_lex_bytes(h),
            "ikm": encode_lex_bytes(ikm),
            "sig": encode_lex_bytes(sig),
            "mac": encode_lex_bytes(mac),
            "rev": repo_state.rev,
        }))),
        (None, None, None) => Ok(Some(serde_json::json!({
            "ver": 1,
            "repo": repo_state.author_did,
            "space": space,
            "rev": repo_state.rev,
            "data": encode_lex_bytes(h),
        }))),
        _ => Err(AppError::Internal(
            "corrupt repo state: a signed commit is missing its ikm, sig or mac".into(),
        )),
    }
}

async fn get_latest_commit(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Query(params): Query<LatestCommitQuery>,
) -> Result<impl IntoResponse, AppError> {
    let did = require_auth_or_credential(&state, &claims).await?;
    let space = service::resolve_space(&state, &params.space).await?;
    let has_credential = claims.space_credential.is_some();
    let membership = service::require_membership(
        &state,
        &space,
        &did,
        false,
        claims.space_credential.as_deref(),
    )
    .await?;

    authorize_space_read(
        &state,
        &claims,
        &did,
        &params.did,
        &space,
        membership,
        has_credential,
    )
    .await?;

    let mut conn = state
        .db
        .acquire()
        .await
        .map_err(|e| AppError::Internal(format!("failed to acquire connection: {e}")))?;
    // An author who has never written here has no state, so no rev and no
    // commit: what an unwritten state would hold.
    let repo_state =
        db::read_repo_state(&mut conn, state.db_backend, &space.id, &params.did).await?;
    let commit = match &repo_state {
        Some(repo_state) => repo_state_commit_json(repo_state, &params.space)?,
        None => None,
    };

    Ok(Json(serde_json::json!({
        "rev": repo_state.and_then(|s| s.rev),
        "commit": commit,
    })))
}

async fn get_repo(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Query(params): Query<GetRepoQuery>,
) -> Result<Response, AppError> {
    let did = require_auth_or_credential(&state, &claims).await?;
    let space = service::resolve_space(&state, &params.space).await?;
    let has_credential = claims.space_credential.is_some();
    let membership = service::require_membership(
        &state,
        &space,
        &did,
        false,
        claims.space_credential.as_deref(),
    )
    .await?;

    authorize_space_read(
        &state,
        &claims,
        &did,
        &params.did,
        &space,
        membership,
        has_credential,
    )
    .await?;

    let mut conn = state
        .db
        .acquire()
        .await
        .map_err(|e| AppError::Internal(format!("failed to acquire connection: {e}")))?;
    let no_commit = || AppError::NotFound("no commit exists for this repo".into());
    let repo_state = db::read_repo_state(&mut conn, state.db_backend, &space.id, &params.did)
        .await?
        .ok_or_else(no_commit)?;
    drop(conn);
    let records =
        db::list_all_space_records(&state.db, state.db_backend, &space.id, &params.did).await?;

    let hash = repo_state.hash.as_deref().ok_or_else(no_commit)?;
    let hash: [u8; 32] = hash
        .try_into()
        .map_err(|_| AppError::Internal("corrupt repo state: hash is not 32 bytes".into()))?;
    let rev = repo_state
        .rev
        .ok_or_else(|| AppError::Internal("corrupt repo state: missing rev".into()))?;

    use crate::spaces::car::{CarCommit, serialize_repo};
    let to_32 = |field: &str, value: Option<&[u8]>| -> Result<[u8; 32], AppError> {
        value
            .ok_or_else(|| AppError::Internal(format!("corrupt repo state: missing {field}")))?
            .try_into()
            .map_err(|_| AppError::Internal(format!("corrupt repo state: {field} is not 32 bytes")))
    };
    // See `repo_state_commit_json` for why a repo may hold only its hash.
    let car_bytes =
        if repo_state.ikm.is_none() && repo_state.sig.is_none() && repo_state.mac.is_none() {
            let commit = crate::spaces::space_mac::Commit {
                ver: 1,
                repo: params.did.clone(),
                space: params.space.clone(),
                rev,
                data: hash,
            };
            serialize_repo(CarCommit::Mac(&commit), &records)?
        } else {
            let commit = crate::spaces::commit::SignedCommit {
                ver: 1,
                hash,
                ikm: to_32("ikm", repo_state.ikm.as_deref())?,
                mac: to_32("mac", repo_state.mac.as_deref())?,
                sig: repo_state
                    .sig
                    .ok_or_else(|| AppError::Internal("corrupt repo state: missing sig".into()))?,
                rev,
            };
            serialize_repo(CarCommit::Signed(&commit), &records)?
        };

    Ok((
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/vnd.ipld.car")],
        car_bytes,
    )
        .into_response())
}

async fn list_repo_ops(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Query(params): Query<ListRepoOpsQuery>,
) -> Result<impl IntoResponse, AppError> {
    let did = require_auth_or_credential(&state, &claims).await?;
    let space = service::resolve_space(&state, &params.space).await?;
    let has_credential = claims.space_credential.is_some();
    let membership = service::require_membership(
        &state,
        &space,
        &did,
        false,
        claims.space_credential.as_deref(),
    )
    .await?;

    authorize_space_read(
        &state,
        &claims,
        &did,
        &params.did,
        &space,
        membership,
        has_credential,
    )
    .await?;

    let limit = params.limit.unwrap_or(100).clamp(1, 1000);
    let exclude_values = params.exclude_values.unwrap_or(false);

    let (ops, cursor) = if exclude_values {
        oplog::list_ops(
            &state.db,
            state.db_backend,
            &space.id,
            &params.did,
            params.cursor.as_deref(),
            limit,
        )
        .await?
    } else {
        oplog::list_ops_with_values(
            &state.db,
            state.db_backend,
            &space.id,
            &params.did,
            params.cursor.as_deref(),
            limit,
        )
        .await?
    };

    let mut conn = state
        .db
        .acquire()
        .await
        .map_err(|e| AppError::Internal(format!("failed to acquire connection: {e}")))?;
    let commit =
        match db::read_repo_state(&mut conn, state.db_backend, &space.id, &params.did).await? {
            Some(repo_state) => repo_state_commit_json(&repo_state, &params.space)?,
            None => None,
        };

    Ok(Json(serde_json::json!({
        "ops": ops,
        "cursor": cursor,
        "commit": commit,
    })))
}

async fn list_repos(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Query(params): Query<ListReposQuery>,
) -> Result<impl IntoResponse, AppError> {
    let space = service::resolve_space(&state, &params.space).await?;
    require_audience(&claims, &space.authority_did)?;

    let did = require_auth_or_credential(&state, &claims).await?;
    let has_credential = claims.space_credential.is_some();
    let membership = service::require_membership(
        &state,
        &space,
        &did,
        false,
        claims.space_credential.as_deref(),
    )
    .await?;
    // Every writer's DID is a whole-space read, which read_self does not grant.
    authorize_space_read(
        &state,
        &claims,
        &did,
        &space.authority_did,
        &space,
        membership,
        has_credential,
    )
    .await?;

    let limit = params.limit.unwrap_or(100).clamp(1, 1000);
    let writers = crate::spaces::writers::list(
        &state.db,
        state.db_backend,
        &space.id,
        params.cursor.as_deref(),
        limit,
    )
    .await?;

    // The cursor is the last space revision returned, so a syncer can also
    // resume from any space revision it has already processed.
    let next_cursor = writers.last().map(|w| w.space_rev.clone());
    let repos: Vec<serde_json::Value> = writers
        .iter()
        .map(|w| {
            use base64::Engine;
            serde_json::json!({
                "did": w.repo_did,
                "repoRev": w.rev,
                // The alpha lexicon's name for `repoRev`. Sent until v3.
                "rev": w.rev,
                "data": { "$bytes": base64::engine::general_purpose::STANDARD_NO_PAD.encode(&w.hash) },
                // The name for `data` before proposal 0016. Sent until every
                // syncer reads `data`.
                "hash": { "$bytes": base64::engine::general_purpose::STANDARD_NO_PAD.encode(&w.hash) },
                "spaceRev": w.space_rev,
            })
        })
        .collect();

    let mut body = serde_json::json!({ "repos": repos });
    if let Some(cursor) = next_cursor {
        body["cursor"] = cursor.into();
    }
    Ok(Json(body))
}

async fn get_space_blob(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Query(params): Query<GetSpaceBlobQuery>,
) -> Result<impl IntoResponse, AppError> {
    let did = require_auth_or_credential(&state, &claims).await?;
    let space = service::resolve_space(&state, &params.space).await?;
    let has_credential = claims.space_credential.is_some();
    let membership = service::require_membership(
        &state,
        &space,
        &did,
        false,
        claims.space_credential.as_deref(),
    )
    .await?;

    let author_did = db::find_blob_author_did(&state.db, state.db_backend, &space.id, &params.cid)
        .await?
        .ok_or_else(|| AppError::NotFound("Blob not found in this space".into()))?;

    authorize_space_read(
        &state,
        &claims,
        &did,
        &author_did,
        &space,
        membership,
        has_credential,
    )
    .await?;

    service::fetch_space_blob(&state, &author_did, &params.cid).await
}

async fn register_notify(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Json(input): Json<RegisterNotifyInput>,
) -> Result<impl IntoResponse, AppError> {
    let did = require_auth_or_credential(&state, &claims).await?;
    let space = service::resolve_space(&state, &input.space).await?;
    require_audience(&claims, &space.authority_did)?;

    // A registration is told every writer's DID and revision, so it needs the
    // whole-space read that listing across repos does.
    let has_credential = claims.space_credential.is_some();
    let membership = service::require_membership(
        &state,
        &space,
        &did,
        false,
        claims.space_credential.as_deref(),
    )
    .await?;
    authorize_space_read(
        &state,
        &claims,
        &did,
        &space.authority_did,
        &space,
        membership,
        has_credential,
    )
    .await?;

    if let Some(service) = &input.service {
        let endpoint = crate::spaces::auth::resolve_service_identifier(
            &state.http,
            &state.config.plc_url,
            service,
        )
        .await
        .ok_or_else(|| AppError::XrpcError {
            status: StatusCode::BAD_REQUEST,
            code: "ServiceNotResolvable",
            message: format!("could not resolve a service endpoint for {service}"),
        })?;
        let (_, expires_at) = notifications::register(
            &state.db,
            state.db_backend,
            &space.id,
            service,
            &endpoint,
            &did,
            NotifyDelivery::Xrpc,
        )
        .await?;
        return Ok(Json(serde_json::json!({ "expiresAt": expires_at })));
    }

    let (Some(service_did), Some(endpoint)) = (&input.service_did, &input.endpoint) else {
        return Err(AppError::BadRequest("service is required".into()));
    };
    let (id, expires_at) = notifications::register(
        &state.db,
        state.db_backend,
        &space.id,
        service_did,
        endpoint,
        &did,
        NotifyDelivery::Webhook,
    )
    .await?;

    Ok(Json(
        serde_json::json!({ "id": id, "expiresAt": expires_at }),
    ))
}

/// Withdraw a write-notification registration.
///
/// Idempotent; see [`db::delete_notify_registrations_for_service`].
async fn unregister_notify(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Json(input): Json<UnregisterNotifyInput>,
) -> Result<impl IntoResponse, AppError> {
    let did = require_auth_or_credential(&state, &claims).await?;
    let space = service::resolve_space(&state, &input.space).await?;
    require_audience(&claims, &space.authority_did)?;

    // A registration belongs to the service that made it; anyone else removing
    // it would stop that service syncing. The space authority may also
    // withdraw, since it is the party the registration is held against.
    if did != input.service {
        service::require_space_admin(&state, &space, &did).await?;
    }

    let removed = db::delete_notify_registrations_for_service(
        &state.db,
        state.db_backend,
        &space.id,
        &input.service,
    )
    .await?;

    Ok(Json(serde_json::json!({ "removed": removed })))
}

/// List the blob CIDs a repo's records reference.
///
/// Blobs are not a table: they are discovered by walking record values for
/// `$link` refs, the same way `find_blob_author_did` locates one.
async fn list_blobs(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Query(params): Query<ListBlobsQuery>,
) -> Result<impl IntoResponse, AppError> {
    let did = require_auth_or_credential(&state, &claims).await?;
    let space = service::resolve_space(&state, &params.space).await?;
    let has_credential = claims.space_credential.is_some();
    let membership = service::require_membership(
        &state,
        &space,
        &did,
        false,
        claims.space_credential.as_deref(),
    )
    .await?;
    authorize_space_read(
        &state,
        &claims,
        &did,
        &params.repo,
        &space,
        membership,
        has_credential,
    )
    .await?;

    let records =
        db::list_all_space_records(&state.db, state.db_backend, &space.id, &params.repo).await?;

    // Sorted and de-duplicated so the cursor is stable across calls; a blob
    // referenced by several records must appear once.
    let mut cids: Vec<String> = records
        .iter()
        .flat_map(|r| crate::record_refs::extract_blob_cids(&r.record))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();

    if let Some(after) = params.cursor.as_deref() {
        cids.retain(|c| c.as_str() > after);
    }
    let limit = params.limit.unwrap_or(500).clamp(1, 1000) as usize;
    let next_cursor = if cids.len() > limit {
        cids.truncate(limit);
        cids.last().cloned()
    } else {
        None
    };

    let mut body = serde_json::json!({ "cids": cids });
    if let Some(cursor) = next_cursor {
        body["cursor"] = serde_json::Value::String(cursor);
    }
    Ok(Json(body))
}

async fn notify_write(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Json(input): Json<NotifyWriteInput>,
) -> Result<impl IntoResponse, AppError> {
    let writer = input
        .repo
        .clone()
        .or_else(|| input.did.clone())
        .ok_or_else(|| AppError::BadRequest("repo is required".into()))?;

    // A repo host notifies with service auth signed as the account that wrote,
    // so the caller must be that account. The space's creator and super admins
    // may also notify, as the legacy shape expects.
    let caller = require_notify_caller(&claims)?;
    // Named, so a repo host knows to stop retrying.
    let space = service::resolve_space(&state, &input.space)
        .await
        .map_err(|e| match e {
            AppError::NotFound(_) => AppError::XrpcError {
                status: StatusCode::BAD_REQUEST,
                code: "SpaceNotFound",
                message: "space not found".into(),
            },
            e => e,
        })?;
    require_service_audience(&claims, &space)?;
    if caller != writer {
        service::require_space_admin(&state, &space, &caller).await?;
    }

    // Spec-shaped notifications come from repo hosts, and the authority tracks
    // only the writers its write policy admits.
    if input.repo.is_some()
        && !crate::spaces::auth::writer_admitted(&state, &space, &writer).await?
    {
        return Err(AppError::Forbidden(
            "this space does not accept writes from this account".into(),
        ));
    }

    // A notification for a repo hosted on the author's own PDS is our cue to
    // pull: the write happened there, and our index has not seen it yet. This
    // is a no-op for polyfill repos, where HappyView is the source of truth and
    // there is nothing upstream.
    match crate::spaces::native_sync::sync_repo(&state, &space, &writer).await {
        Ok(crate::spaces::native_sync::SyncOutcome::Diverged { expected, ours }) => {
            // Incremental sync closed no gap. The sender did nothing wrong, so
            // the notification still succeeds, and the divergence is logged as
            // an error.
            tracing::error!(
                space_id = %space.id,
                author_did = %writer,
                expected,
                ours,
                "native repo diverged after sync; full recovery needed"
            );
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(
            space_id = %space.id,
            author_did = %writer,
            error = %e,
            "failed to sync a native repo after a write notification"
        ),
    }

    if let (Some(rev), Some(hash)) = (
        input.repo_rev.as_ref().or(input.rev.as_ref()),
        input.data.as_ref().or(input.hash.as_ref()),
    ) {
        let hash = hash.decode()?;
        let mut conn = service::begin_space_write(&state, &space.id).await?;
        let recorded = crate::spaces::writers::record(
            &mut conn,
            state.db_backend,
            &space.id,
            &writer,
            rev,
            &hash,
        )
        .await?;
        conn.commit()
            .await
            .map_err(|e| AppError::Internal(format!("failed to commit transaction: {e}")))?;

        // A repeated or out-of-order report changes nothing, so nobody hears
        // about it.
        if let crate::spaces::writers::Recorded::Advanced {
            space_rev,
            prev_space_rev,
        } = recorded
        {
            notifications::forward_repo_update(
                &state,
                &space.id,
                notifications::RepoUpdate {
                    space_uri: input.space.clone(),
                    repo: writer.clone(),
                    rev: rev.clone(),
                    hash,
                    space_rev,
                    prev_space_rev,
                },
            );
        }
    }

    if let (Some(collection), Some(rkey)) = (&input.collection, &input.rkey) {
        notifications::announce_writes(
            &state,
            &space.id,
            &writer,
            vec![notifications::WebhookWrite {
                collection: collection.clone(),
                rkey: rkey.clone(),
                cid: input.cid.clone(),
            }],
        );
    }

    Ok(Json(serde_json::json!({ "success": true })))
}

async fn notify_space_deleted(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Json(input): Json<NotifySpaceDeletedInput>,
) -> Result<impl IntoResponse, AppError> {
    // Inter-service route: only the space authority (or a super admin) may fire
    // "space deleted" events (which may cause consumers to purge cached data).
    let did = require_notify_caller(&claims)?;
    let space = service::resolve_space(&state, &input.space).await?;
    require_service_audience(&claims, &space)?;
    service::require_space_admin(&state, &space, &did).await?;

    let registrations =
        db::list_notify_registrations(&state.db, state.db_backend, &space.id).await?;
    notifications::announce_space_deleted(&state, &space, registrations);

    Ok(Json(serde_json::json!({ "success": true })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotifyCredentialRevokedInput {
    space: String,
    credentials: Vec<String>,
}

/// The most `jti`s one `notifyCredentialRevoked` call may name.
const MAX_REVOKED_PER_NOTIFICATION: usize = 100;

/// A space authority revoking credentials before they expire.
///
/// This instance accepts only credentials it issued, so a revocation marks its
/// own records, which every credential check reads. Service auth from the
/// space's authority is required. Repeating a revocation changes nothing.
async fn notify_credential_revoked(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Json(input): Json<NotifyCredentialRevokedInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let caller = claims
        .service_auth
        .as_ref()
        .map(|sa| sa.did.clone())
        .ok_or_else(|| {
            AppError::Auth("service auth from the space authority is required".into())
        })?;
    if input.credentials.is_empty()
        || input.credentials.len() > MAX_REVOKED_PER_NOTIFICATION
        || input.credentials.iter().any(|jti| jti.is_empty())
    {
        return Err(AppError::XrpcError {
            status: StatusCode::BAD_REQUEST,
            code: "InvalidRequest",
            message: format!(
                "credentials must name 1 to {MAX_REVOKED_PER_NOTIFICATION} non-empty jtis"
            ),
        });
    }
    let space = service::resolve_space(&state, &input.space)
        .await
        .map_err(|e| match e {
            AppError::NotFound(_) => AppError::XrpcError {
                status: StatusCode::BAD_REQUEST,
                code: "SpaceNotFound",
                message: "space not found".into(),
            },
            e => e,
        })?;
    require_service_audience(&claims, &space)?;
    if caller != space.authority_did {
        return Err(AppError::Forbidden(
            "only the space's authority may revoke its credentials".into(),
        ));
    }
    db::revoke_space_credentials_by_jti(&state.db, state.db_backend, &space.id, &input.credentials)
        .await?;
    Ok(Json(serde_json::json!({})))
}

/// Issue a space credential bound to the key the request names.
///
/// The request carries whichever assertions the space requires: a delegation
/// token from the user's PDS for a space that gates on users, and a client
/// attestation for one that gates on apps. Neither is an authorization header,
/// and an assertion the space does not require is still verified, never held
/// against the request.
async fn get_space_credential(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(input): Json<GetSpaceCredentialInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let refused = |code: &'static str, message: String| AppError::XrpcError {
        status: StatusCode::BAD_REQUEST,
        code,
        message,
    };
    // Charged before anything in the request is parsed or verified. A
    // credential request needs no session, every credential minted is a
    // stored row, and verifying a delegation token resolves its issuer's DID
    // before its signature is checked. Space routes are mounted beside the
    // XRPC routes, not behind their limiter, so this is their limit.
    let key = crate::xrpc::ANONYMOUS_CLIENT_KEY;
    let cost = state.rate_limiter.default_cost_for_type(key, "procedure");
    if let crate::rate_limit::CheckResult::Limited {
        retry_after,
        limit,
        reset,
    } = state.rate_limiter.check(key, cost)
    {
        return Err(AppError::RateLimited {
            retry_after,
            limit,
            reset,
        });
    }
    let (generation, credential_key) = exchange_generation(&input, &headers)?;
    let credential_key = credential_key.as_str();
    if !crate::spaces::http_signature::is_p256_did_key(credential_key) {
        return Err(refused(
            "InvalidRequest",
            format!("{} must be a P-256 did:key", generation.key_field()),
        ));
    }

    let encryption_key = &state.config.keyring().ok_or_else(|| {
        AppError::Internal("TOKEN_ENCRYPTION_KEY is required for space credentials".into())
    })?;

    // Everything the assertions are checked against comes from the address,
    // so they are verified before the space is looked up, and a request
    // carrying bad ones is answered the same whether the space exists or not.
    let address = crate::spaces::SpaceUri::parse(&input.space)?;
    let space_uri = format!(
        "at://{}/space/{}/{}",
        address.did, address.type_nsid, address.skey
    );
    // Proposal 0016 addresses both assertions to the space host, the
    // authority's `#atproto_pds`. Simplespace addressed a delegation token to
    // `#atproto_space_host`, and a client attestation to the bare DID.
    let host_aud = format!("{}#atproto_pds", address.did);
    let legacy_host_aud = format!("{}#atproto_space_host", address.did);
    let (delegation_auds, attestation_aud): (Vec<&str>, &str) = match generation {
        ExchangeGeneration::G0 => (
            vec![legacy_host_aud.as_str(), host_aud.as_str()],
            address.did.as_str(),
        ),
        _ => (vec![host_aud.as_str()], host_aud.as_str()),
    };

    // Simplespace sent the delegation token as the request's bearer token.
    let delegation_token = match generation {
        ExchangeGeneration::G0 => bearer_token(&headers),
        _ => input.delegation_token.clone(),
    };
    let subject = match &delegation_token {
        Some(token) => Some(
            verify_delegation(&state, encryption_key, token, &space_uri, &delegation_auds).await?,
        ),
        None => None,
    };

    let client_id = match &input.client_attestation {
        Some(attestation) => {
            let verified = crate::spaces::client_attestation::verify_client_attestation(
                attestation,
                attestation_aud,
                &state.http,
            )
            .await
            .map_err(|e| refused("InvalidClientAttestation", e.to_string()))?;
            if !db::consume_delegation_token(
                &state.db,
                state.db_backend,
                &verified.client_id,
                &verified.jti,
                verified.exp,
            )
            .await?
            {
                return Err(refused(
                    "InvalidClientAttestation",
                    "client attestation has already been used".into(),
                ));
            }
            Some(verified.client_id)
        }
        None => None,
    };

    let space = service::resolve_space(&state, &space_uri)
        .await
        .map_err(|e| match e {
            AppError::NotFound(_) => refused("SpaceNotFound", "space not found".into()),
            e => e,
        })?;

    // The account is settled before the app: an account the space does not
    // admit is answered `SpaceNotFound` (`refused_user`), whichever app it
    // came through. The app is checked in `issue_credential`, after the
    // space's policy admits the account.

    // A space that anyone may read needs no user. A managing app may decide
    // a read with no user, so its space asks it, with no `repo`: only an app
    // on the Nov 12 revision (`checkAccess`) can be asked that, and one on an
    // earlier revision is taken to refuse, as those revisions did. Any other
    // space has to know who it is issuing to.
    let subject = match subject {
        Some(subject) => subject,
        None if matches!(
            space.read_policy,
            crate::spaces::types::Policy::Public | crate::spaces::types::Policy::ManagingApp { .. }
        ) =>
        {
            String::new()
        }
        None => {
            return Err(generation.refused_user());
        }
    };

    let issued = crate::spaces::auth::issue_credential_as(
        &state.db,
        state.db_backend,
        &state.http,
        encryption_key,
        &state.config.public_url,
        &state.config.plc_url,
        &space,
        &subject,
        client_id.as_deref(),
        &space.authority_did,
        Some(credential_key),
        generation.signing_kid(),
    )
    .await
    .map_err(|e| match e {
        AppError::Forbidden(_) => generation.refused_user(),
        e => e,
    })?;

    Ok(Json(serde_json::json!({
        "credential": issued.token,
        "expiresAt": issued.expires_at,
    })))
}

/// Which revision of Proposal 0016 a `getSpaceCredential` request follows,
/// told by the field naming the key to bind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExchangeGeneration {
    /// Simplespace: no key field. The delegation token is the bearer token,
    /// and the key to bind is the `keyid` of an `atproto-space` signature
    /// over the request.
    G0,
    /// Oct 29: `credentialKid`.
    G1,
    /// Nov 12 (proposals #143): `credentialKey`.
    G2,
}

impl ExchangeGeneration {
    fn key_field(self) -> &'static str {
        match self {
            Self::G0 => "the signature's keyid",
            Self::G1 => "credentialKid",
            Self::G2 => "credentialKey",
        }
    }

    /// The `kid` a credential for this request is signed under, where the
    /// space is on a DID of its own. Simplespace verifiers look the key up as
    /// `#atproto_space`, which a space DID publishes beside `#atproto`.
    fn signing_kid(self) -> &'static str {
        match self {
            Self::G0 => crate::spaces::authority::LEGACY_SIGNING_KID,
            Self::G1 | Self::G2 => crate::spaces::authority::SIGNING_KID,
        }
    }

    /// The error for a user the space does not admit: `SpaceNotFound`, as the
    /// Nov 12 text specifies. The Oct 29 text named the user
    /// (`UserNotAuthorized`), and HappyView answers every generation the Nov 12
    /// way (HAP-388 H4). `SpaceNotFound` is in the Oct 29 lexicon too, so an
    /// Oct 29 client stays within it.
    fn refused_user(self) -> AppError {
        AppError::XrpcError {
            status: StatusCode::BAD_REQUEST,
            code: "SpaceNotFound",
            message: "space not found".into(),
        }
    }
}

/// The request's generation and the key it names: `credentialKey` (Nov 12),
/// `credentialKid` (Oct 29), or, when it names neither, the simplespace
/// exchange, which binds the key named by an `atproto-space` signature over
/// the request.
fn exchange_generation(
    input: &GetSpaceCredentialInput,
    headers: &axum::http::HeaderMap,
) -> Result<(ExchangeGeneration, String), AppError> {
    match (&input.credential_key, &input.credential_kid) {
        (Some(key), Some(kid)) if key != kid => Err(AppError::XrpcError {
            status: StatusCode::BAD_REQUEST,
            code: "InvalidRequest",
            message: "credentialKey and credentialKid name different keys".into(),
        }),
        (Some(key), _) => Ok((ExchangeGeneration::G2, key.clone())),
        (None, Some(kid)) => Ok((ExchangeGeneration::G1, kid.clone())),
        (None, None) => {
            let key = crate::spaces::http_signature::verify_legacy(headers, None)?;
            Ok((ExchangeGeneration::G0, key))
        }
    }
}

/// The token in a `Bearer` authorization, as simplespace sent a delegation
/// token.
fn bearer_token(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

/// Verify a delegation token for `space_uri` and record it as used, returning
/// the user it was issued by.
async fn verify_delegation(
    state: &AppState,
    encryption_key: &crate::plugin::encryption::Keyring,
    token: &str,
    space_uri: &str,
    accepted_aud: &[&str],
) -> Result<String, AppError> {
    let invalid = |message: String| AppError::XrpcError {
        status: StatusCode::BAD_REQUEST,
        code: "InvalidDelegationToken",
        message,
    };

    // A PDS that serves spaces mints delegation tokens with the account's own
    // key. For accounts on one that does not, HappyView mints them with its own
    // delegation key. A key that cannot be read leaves only the account's own
    // key to try, as a token this instance signed cannot be verified without it
    // anyway.
    let local_key =
        crate::spaces::delegation_key::load(&state.db, state.db_backend, encryption_key)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "delegation key unreadable");
                None
            });
    let local_claims = local_key.as_ref().map(|key| {
        crate::spaces::credential::verify_delegation_token(token, key.verifying_key(), accepted_aud)
    });
    let claims = match local_claims {
        Some(Ok(claims)) => claims,
        _ => crate::spaces::credential::verify_account_delegation_token(
            token,
            &state.http,
            &state.config.plc_url,
            accepted_aud,
        )
        .await
        .map_err(|e| invalid(e.to_string()))?,
    };
    if claims.sub != space_uri {
        return Err(invalid("delegation token is for a different space".into()));
    }
    if !db::consume_delegation_token(
        &state.db,
        state.db_backend,
        &claims.iss,
        &claims.jti,
        claims.exp,
    )
    .await?
    {
        return Err(invalid("delegation token has already been used".into()));
    }
    Ok(claims.iss)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_forwarded_batch_tags_each_write_by_type_and_sends_only_what_was_given() {
        let writes: Vec<WriteOp> = serde_json::from_value(json!([
            { "action": "create", "collection": "com.example.a", "value": { "n": 1 } },
            { "action": "update", "collection": "com.example.a", "rkey": "r1",
              "value": { "n": 2 }, "swapRecord": "bafyold" },
            { "action": "delete", "collection": "com.example.b", "rkey": "r2" },
        ]))
        .unwrap();
        let body = pds_apply_writes_body("at://did:web:s/space/t/k", "did:plc:me", None, &writes);
        assert_eq!(
            body,
            json!({
                "space": "at://did:web:s/space/t/k",
                "repo": "did:plc:me",
                "writes": [
                    { "$type": "com.atproto.space.applyWrites#create",
                      "collection": "com.example.a", "value": { "n": 1 } },
                    { "$type": "com.atproto.space.applyWrites#update",
                      "collection": "com.example.a", "rkey": "r1", "value": { "n": 2 },
                      "swapRecord": "bafyold" },
                    { "$type": "com.atproto.space.applyWrites#delete",
                      "collection": "com.example.b", "rkey": "r2" },
                ],
            })
        );
        let with_swap =
            pds_apply_writes_body("at://did:web:s/space/t/k", "did:plc:me", Some("3krev"), &[]);
        assert_eq!(with_swap["swapCommit"], "3krev");
    }

    #[test]
    fn content_cid_deterministic() {
        let record = json!({"text": "hello"});
        let cid1 = service::content_cid(&record).expect("encodable");
        let cid2 = service::content_cid(&record).expect("encodable");
        assert_eq!(cid1, cid2);
        assert!(cid1.starts_with("bafyrei"));
    }

    #[test]
    fn content_cid_changes_for_different_records() {
        let a = service::content_cid(&json!({"text": "hello"})).expect("encodable");
        let b = service::content_cid(&json!({"text": "world"})).expect("encodable");
        assert_ne!(a, b);
    }

    #[test]
    fn content_cid_matches_canonical_recomputation() {
        let record = json!({ "$type": "com.example.item", "text": "hello" });
        let minted = service::content_cid(&record).expect("encodable");
        assert_eq!(
            crate::cid_verify::verify_record_cid(&minted, &record),
            crate::cid_verify::CidCheck::Match,
        );
    }

    #[test]
    fn content_cid_is_independent_of_key_order() {
        let a = service::content_cid(&json!({ "aa": 2, "b": 1 })).expect("encodable");
        let b = service::content_cid(&json!({ "b": 1, "aa": 2 })).expect("encodable");
        assert_eq!(a, b);
    }

    #[test]
    fn content_cid_rejects_unencodable_record() {
        // A malformed `$link` cannot become an IPLD link.
        let bad = json!({ "ref": { "$link": "not-a-cid" } });
        assert!(matches!(
            service::content_cid(&bad),
            Err(AppError::BadRequest(_))
        ));
    }

    #[test]
    fn deserialize_create_record_input() {
        let input: CreateRecordInput = serde_json::from_value(json!({
            "space": "at://did:plc:abc/space/com.example.forum/main",
            "collection": "com.example.forum.post",
            "record": { "text": "hello" }
        }))
        .unwrap();
        assert_eq!(input.space, "at://did:plc:abc/space/com.example.forum/main");
        assert_eq!(input.collection, "com.example.forum.post");
        assert_eq!(input.record["text"], "hello");
    }

    #[test]
    fn deserialize_put_record_with_swap() {
        let input: PutRecordInput = serde_json::from_value(json!({
            "space": "at://did:plc:abc/space/com.example.forum/main",
            "collection": "com.example.forum.post",
            "rkey": "3k2abc",
            "record": { "text": "updated" },
            "swapRecord": "bafyrei123"
        }))
        .unwrap();
        assert_eq!(input.swap_record.as_deref(), Some("bafyrei123"));
    }

    #[test]
    fn deserialize_put_record_without_swap() {
        let input: PutRecordInput = serde_json::from_value(json!({
            "space": "at://did:plc:abc/space/com.example.forum/main",
            "collection": "com.example.forum.post",
            "rkey": "3k2abc",
            "record": { "text": "hello" }
        }))
        .unwrap();
        assert_eq!(input.swap_record, None);
    }

    #[test]
    fn deserialize_delete_record_with_swap() {
        let input: DeleteRecordInput = serde_json::from_value(json!({
            "space": "at://did:plc:abc/space/com.example.forum/main",
            "collection": "com.example.forum.post",
            "rkey": "3k2abc",
            "swapRecord": "bafyrei456"
        }))
        .unwrap();
        assert_eq!(input.swap_record.as_deref(), Some("bafyrei456"));
    }

    #[test]
    fn deserialize_write_op_create() {
        let op: WriteOp = serde_json::from_value(json!({
            "action": "create",
            "collection": "com.example.forum.post",
            "value": { "text": "new post" }
        }))
        .unwrap();
        match op {
            WriteOp::Create {
                collection,
                rkey,
                value,
            } => {
                assert_eq!(collection, "com.example.forum.post");
                assert_eq!(rkey, None);
                assert_eq!(value["text"], "new post");
            }
            _ => panic!("expected Create"),
        }
    }

    #[test]
    fn deserialize_write_op_create_with_rkey() {
        let op: WriteOp = serde_json::from_value(json!({
            "action": "create",
            "collection": "com.example.forum.post",
            "rkey": "custom-key",
            "value": { "text": "new post" }
        }))
        .unwrap();
        match op {
            WriteOp::Create { rkey, .. } => {
                assert_eq!(rkey.as_deref(), Some("custom-key"));
            }
            _ => panic!("expected Create"),
        }
    }

    #[test]
    fn deserialize_write_op_update() {
        let op: WriteOp = serde_json::from_value(json!({
            "action": "update",
            "collection": "com.example.forum.post",
            "rkey": "3k2abc",
            "value": { "text": "updated" },
            "swapRecord": "bafyrei789"
        }))
        .unwrap();
        match op {
            WriteOp::Update {
                collection,
                rkey,
                swap_record,
                ..
            } => {
                assert_eq!(collection, "com.example.forum.post");
                assert_eq!(rkey, "3k2abc");
                assert_eq!(swap_record.as_deref(), Some("bafyrei789"));
            }
            _ => panic!("expected Update"),
        }
    }

    #[test]
    fn deserialize_write_op_delete() {
        let op: WriteOp = serde_json::from_value(json!({
            "action": "delete",
            "collection": "com.example.forum.post",
            "rkey": "3k2abc"
        }))
        .unwrap();
        match op {
            WriteOp::Delete {
                collection,
                rkey,
                swap_record,
            } => {
                assert_eq!(collection, "com.example.forum.post");
                assert_eq!(rkey, "3k2abc");
                assert_eq!(swap_record, None);
            }
            _ => panic!("expected Delete"),
        }
    }

    #[test]
    fn deserialize_apply_writes_input() {
        let input: ApplyWritesInput = serde_json::from_value(json!({
            "space": "at://did:plc:abc/space/com.example.forum/main",
            "swapCommit": "tid123",
            "writes": [
                {
                    "action": "create",
                    "collection": "com.example.forum.post",
                    "value": { "text": "post 1" }
                },
                {
                    "action": "delete",
                    "collection": "com.example.forum.post",
                    "rkey": "old-key"
                }
            ]
        }))
        .unwrap();
        assert_eq!(input.space, "at://did:plc:abc/space/com.example.forum/main");
        assert_eq!(input.swap_commit.as_deref(), Some("tid123"));
        assert_eq!(input.writes.len(), 2);
    }

    #[test]
    fn deserialize_apply_writes_without_swap_commit() {
        let input: ApplyWritesInput = serde_json::from_value(json!({
            "space": "at://did:plc:abc/space/com.example.forum/main",
            "writes": [
                {
                    "action": "create",
                    "collection": "com.example.forum.post",
                    "value": { "text": "post" }
                }
            ]
        }))
        .unwrap();
        assert_eq!(input.swap_commit, None);
    }

    #[test]
    fn deserialize_write_op_rejects_unknown_action() {
        let result = serde_json::from_value::<WriteOp>(json!({
            "action": "unknown",
            "collection": "test",
            "rkey": "key"
        }));
        assert!(result.is_err());
    }
}
