use axum::body::Bytes;
use mlua::{Lua, LuaSerdeExt, Result as LuaResult};
use std::sync::Arc;

use crate::AppState;
use crate::plugin::host;
use happyview_plugin_sdk::wire::{AtprotoBlobDownload, AttestVerify};

/// Opaque handle to blob bytes stored on the Rust side.
/// Lua scripts receive this from `atproto.blob_download()` and pass it
/// to `atproto.blob_upload()` — the binary data never enters the Lua VM.
#[derive(Clone)]
pub(crate) struct BlobHandle {
    pub data: Bytes,
    pub mime_type: String,
}

impl mlua::UserData for BlobHandle {
    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("size", |_, this, ()| Ok(this.data.len()));
        methods.add_method("mime_type", |_, this, ()| Ok(this.mime_type.clone()));
    }
}

/// `blob_download`'s error messages named the request (`did`/`cid`), which
/// `host::atproto::AtprotoError` has no reason to carry — a WASM plugin's
/// `BLOB_ERROR` envelope doesn't need it, since the plugin already has both.
/// The Lua global keeps naming them because scripts have relied on it.
fn blob_download_lua_error(
    did: &str,
    cid: &str,
    e: crate::plugin::host::AtprotoError,
) -> mlua::Error {
    use crate::plugin::host::AtprotoError;
    match e {
        AtprotoError::Resolve(msg) => mlua::Error::runtime(format!("blob_download: {msg}")),
        AtprotoError::Blob { status, body } => mlua::Error::runtime(format!(
            "blob_download: PDS returned {status} for did={did} cid={cid}{}",
            if body.is_empty() {
                String::new()
            } else {
                format!(": {body}")
            }
        )),
        other => mlua::Error::runtime(format!("blob_download: {other}")),
    }
}

/// Register the `atproto` table with AT Protocol utility functions.
///
/// When `caller_did` is provided, the `atproto.sign(record)` function is
/// available for inline attestation signing.
///
/// `blob_download` here fetches whatever endpoint a DID document names,
/// loopback and private ranges included: scripts are operator-authored and
/// already hold an unrestricted `http` global, and a local PDS is the
/// normal development setup. The plugin import applies the endpoint guard
/// instead, since a plugin is a third party.
pub fn register_atproto_api(
    lua: &Lua,
    state: Arc<AppState>,
    caller_did: Option<&str>,
) -> LuaResult<()> {
    register_atproto_api_impl(lua, state, caller_did, true)
}

fn register_atproto_api_impl(
    lua: &Lua,
    state: Arc<AppState>,
    caller_did: Option<&str>,
    allow_local_blob_endpoints: bool,
) -> LuaResult<()> {
    let atproto_table = lua.create_table()?;

    let state_clone = state.clone();
    let resolve_fn = lua.create_async_function(move |_lua, did: String| {
        let state = state_clone.clone();
        async move {
            let endpoint = host::resolve_service(&state.http, &state.config.plc_url, &did).await;
            Ok(endpoint.unwrap_or(None))
        }
    })?;

    atproto_table.set("resolve_service_endpoint", resolve_fn)?;

    // atproto.blob_download(did, cid) -> { handle = BlobHandle, mimeType = string, size = number }
    {
        let state_clone = state.clone();
        let blob_download_fn =
            lua.create_async_function(move |lua, (did, cid): (String, String)| {
                let state = state_clone.clone();
                async move {
                    let blob = host::blob_download_with_policy(
                        &state.http,
                        &state.config.plc_url,
                        AtprotoBlobDownload {
                            did: did.clone(),
                            cid: cid.clone(),
                        },
                        allow_local_blob_endpoints,
                    )
                    .await
                    .map_err(|e| blob_download_lua_error(&did, &cid, e))?;

                    let mime_type = blob.mime_type;
                    let size = blob.size as usize;
                    let handle = BlobHandle {
                        data: Bytes::from(blob.bytes),
                        mime_type: mime_type.clone(),
                    };

                    let result = lua.create_table()?;
                    result.set("handle", lua.create_userdata(handle)?)?;
                    result.set("mimeType", mime_type)?;
                    result.set("size", size)?;

                    Ok(mlua::Value::Table(result))
                }
            })?;
        atproto_table.set("blob_download", blob_download_fn)?;
    }

    // get_labels(uri) -> array of { src, uri, val, cts }
    let state_clone = state.clone();
    let get_labels_fn = lua.create_async_function(move |lua, uri: String| {
        let state = state_clone.clone();
        async move {
            let mut by_uri =
                host::labels_get(&state.db, state.db_backend, std::slice::from_ref(&uri))
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("label query failed: {e}")))?;
            let labels = by_uri.remove(&uri).unwrap_or_default();

            let result = lua.create_table()?;
            for (idx, label) in labels.into_iter().enumerate() {
                let entry = lua.create_table()?;
                entry.set("src", label.src)?;
                entry.set("uri", label.uri)?;
                entry.set("val", label.val)?;
                entry.set("cts", label.cts)?;
                result.set(idx + 1, entry)?;
            }
            Ok(mlua::Value::Table(result))
        }
    })?;
    atproto_table.set("get_labels", get_labels_fn)?;

    // get_labels_batch(uris) -> table keyed by URI
    let state_clone = state.clone();
    let get_labels_batch_fn = lua.create_async_function(move |lua, uris: mlua::Table| {
        let state = state_clone.clone();
        async move {
            let uri_list: Vec<String> = uris
                .sequence_values::<String>()
                .collect::<Result<Vec<_>, _>>()?;

            let by_uri = host::labels_get(&state.db, state.db_backend, &uri_list)
                .await
                .map_err(|e| mlua::Error::runtime(format!("label batch query failed: {e}")))?;

            let result = lua.create_table()?;
            for uri in &uri_list {
                let uri_table = lua.create_table()?;
                if let Some(labels) = by_uri.get(uri) {
                    for (idx, label) in labels.iter().enumerate() {
                        let entry = lua.create_table()?;
                        entry.set("src", label.src.as_str())?;
                        entry.set("uri", label.uri.as_str())?;
                        entry.set("val", label.val.as_str())?;
                        entry.set("cts", label.cts.as_str())?;
                        uri_table.set(idx + 1, entry)?;
                    }
                }
                result.set(uri.as_str(), uri_table)?;
            }

            Ok(mlua::Value::Table(result))
        }
    })?;
    atproto_table.set("get_labels_batch", get_labels_batch_fn)?;

    // atproto.sign(record_table) -> inline signature object or nil
    //
    // Signs a record using the attestation signer and returns the inline
    // signature object ({ $type, key, signature: { $bytes } }).
    // Returns nil if no signer is configured.
    if let Some(signer) = &state.attestation_signer {
        let signer = signer.clone();
        let did = caller_did.unwrap_or("").to_string();
        let sign_fn = lua.create_function(move |lua, table: mlua::Value| {
            let record: serde_json::Value = lua
                .from_value(table)
                .map_err(|e| mlua::Error::runtime(format!("atproto.sign: {e}")))?;

            let sig = host::attest_sign(Some(&signer), &did, record)
                .map_err(|e| mlua::Error::runtime(format!("atproto.sign: {e}")))?;

            lua.to_value(&sig)
                .map_err(|e| mlua::Error::runtime(format!("atproto.sign: {e}")))
        })?;
        atproto_table.set("sign", sign_fn)?;
    }

    // atproto.verify_signature(record_table, sig_table, repository_did) -> boolean
    //
    // Verifies that an inline signature was produced by this HappyView instance.
    // Recomputes the CID and verifies the ECDSA signature.
    if let Some(signer) = &state.attestation_signer {
        let signer = signer.clone();
        let verify_fn = lua.create_function(
            move |lua, (record, sig, repo_did): (mlua::Value, mlua::Value, String)| {
                let record_json: serde_json::Value = lua
                    .from_value(record)
                    .map_err(|e| mlua::Error::runtime(format!("atproto.verify_signature: {e}")))?;
                let sig_json: serde_json::Value = lua
                    .from_value(sig)
                    .map_err(|e| mlua::Error::runtime(format!("atproto.verify_signature: {e}")))?;

                // `false` and an error are different facts, and only `false`
                // is a statement about the record: it means we checked and the
                // signature does not match. An error means we could not check
                // — malformed signature bytes, a missing field, a record that
                // will not encode. Collapsing the second into the first would
                // let any fault in this path present to a script as "this user
                // forged their records", with nothing in the logs to say
                // otherwise. Callers that want the old behaviour can `pcall`.
                host::attest_verify(
                    Some(&signer),
                    AttestVerify {
                        record: record_json,
                        signature: sig_json,
                        repo_did: repo_did.clone(),
                    },
                )
                .map_err(|e| {
                    tracing::warn!(
                        repository = %repo_did,
                        error = %e,
                        "atproto.verify_signature could not check the signature — \
                         this is not a statement that the record is forged"
                    );
                    mlua::Error::runtime(format!("atproto.verify_signature: {e}"))
                })
            },
        )?;
        atproto_table.set("verify_signature", verify_fn)?;
    }

    // atproto.spaces sub-table
    let spaces_table = lua.create_table()?;

    // atproto.spaces.is_member(space_uri, did) -> boolean
    //
    // The feature-flag gate is `host::spaces::require_enabled` — shared with
    // every other spaces entry point — but everything past it stays this
    // function's own lookup, so its error text (distinct per failing stage)
    // matches what a script pattern-matches on.
    let state_clone = state.clone();
    let is_member_fn =
        lua.create_async_function(move |_lua, (space_uri, did): (String, String)| {
            let state = state_clone.clone();
            async move {
                host::spaces::require_enabled(&state)
                    .await
                    .map_err(|e| mlua::Error::runtime(e.to_string()))?;
                let uri = crate::spaces::SpaceUri::parse(&space_uri)
                    .map_err(|e| mlua::Error::runtime(format!("invalid space URI: {e}")))?;
                let space = crate::spaces::db::get_space_by_address(
                    &state.db,
                    state.db_backend,
                    &uri.did,
                    &uri.type_nsid,
                    &uri.skey,
                )
                .await
                .map_err(|e| mlua::Error::runtime(format!("space lookup failed: {e}")))?;
                let space = match space {
                    Some(s) => s,
                    None => return Ok(false),
                };
                let access =
                    crate::spaces::members::is_member(&state.db, state.db_backend, &space.id, &did)
                        .await
                        .map_err(|e| {
                            mlua::Error::runtime(format!("membership check failed: {e}"))
                        })?;
                Ok(access.is_some())
            }
        })?;
    spaces_table.set("is_member", is_member_fn)?;

    // atproto.spaces.get_access(space_uri, did) -> 'read' | 'write' | 'read_self' | nil
    let state_clone = state.clone();
    let get_access_fn =
        lua.create_async_function(move |_lua, (space_uri, did): (String, String)| {
            let state = state_clone.clone();
            async move {
                host::spaces::require_enabled(&state)
                    .await
                    .map_err(|e| mlua::Error::runtime(e.to_string()))?;
                let uri = crate::spaces::SpaceUri::parse(&space_uri)
                    .map_err(|e| mlua::Error::runtime(format!("invalid space URI: {e}")))?;
                let space = crate::spaces::db::get_space_by_address(
                    &state.db,
                    state.db_backend,
                    &uri.did,
                    &uri.type_nsid,
                    &uri.skey,
                )
                .await
                .map_err(|e| mlua::Error::runtime(format!("space lookup failed: {e}")))?;
                let space = match space {
                    Some(s) => s,
                    None => return Ok(None),
                };
                let access =
                    crate::spaces::members::is_member(&state.db, state.db_backend, &space.id, &did)
                        .await
                        .map_err(|e| {
                            mlua::Error::runtime(format!("membership check failed: {e}"))
                        })?;
                Ok(access.map(|a| a.as_wire_str().to_string()))
            }
        })?;
    spaces_table.set("get_access", get_access_fn)?;

    // atproto.spaces.list_members(space_uri) -> array of { did, access }
    let state_clone = state.clone();
    let list_members_fn = lua.create_async_function(move |lua, space_uri: String| {
        let state = state_clone.clone();
        async move {
            host::spaces::require_enabled(&state)
                .await
                .map_err(|e| mlua::Error::runtime(e.to_string()))?;
            let uri = crate::spaces::SpaceUri::parse(&space_uri)
                .map_err(|e| mlua::Error::runtime(format!("invalid space URI: {e}")))?;
            let space = crate::spaces::db::get_space_by_address(
                &state.db,
                state.db_backend,
                &uri.did,
                &uri.type_nsid,
                &uri.skey,
            )
            .await
            .map_err(|e| mlua::Error::runtime(format!("space lookup failed: {e}")))?;
            let space = match space {
                Some(s) => s,
                None => {
                    return Err(mlua::Error::runtime("space not found"));
                }
            };
            let members =
                crate::spaces::members::resolve_members(&state.db, state.db_backend, &space.id)
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("member resolution failed: {e}")))?;

            let result = lua.create_table()?;
            for (i, member) in members.iter().enumerate() {
                let entry = lua.create_table()?;
                entry.set("did", member.did.as_str())?;
                entry.set("access", member.access.as_wire_str())?;
                result.set(i + 1, entry)?;
            }
            Ok(mlua::Value::Table(result))
        }
    })?;
    spaces_table.set("list_members", list_members_fn)?;

    // atproto.spaces.query({ space_uri, collection, limit, cursor }) -> { records, cursor }
    let state_clone = state.clone();
    let query_fn = lua.create_async_function(move |lua, opts: mlua::Table| {
        let state = state_clone.clone();
        async move {
            host::spaces::require_enabled(&state)
                .await
                .map_err(|e| mlua::Error::runtime(e.to_string()))?;
            let space_uri: String = opts
                .get("space_uri")
                .map_err(|_| mlua::Error::runtime("space_uri is required"))?;
            let collection: Option<String> = opts.get("collection").ok();
            let limit: i64 = opts.get("limit").unwrap_or(50);
            let cursor: Option<String> = opts.get("cursor").ok();

            let uri = crate::spaces::SpaceUri::parse(&space_uri)
                .map_err(|e| mlua::Error::runtime(format!("invalid space URI: {e}")))?;
            let space = crate::spaces::db::get_space_by_address(
                &state.db,
                state.db_backend,
                &uri.did,
                &uri.type_nsid,
                &uri.skey,
            )
            .await
            .map_err(|e| mlua::Error::runtime(format!("space lookup failed: {e}")))?;
            let space = match space {
                Some(s) => s,
                None => {
                    return Err(mlua::Error::runtime("space not found"));
                }
            };

            let (records, next_cursor) = crate::spaces::db::list_space_records(
                &state.db,
                state.db_backend,
                &space.id,
                None,
                collection.as_deref(),
                limit.min(100),
                cursor.as_deref(),
                false,
            )
            .await
            .map_err(|e| mlua::Error::runtime(format!("record query failed: {e}")))?;

            let result = lua.create_table()?;
            let records_table = lua.create_table()?;
            for (i, record) in records.iter().enumerate() {
                let entry = lua.to_value(&serde_json::json!({
                    "uri": record.uri,
                    "collection": record.collection,
                    "rkey": record.rkey,
                    "record": record.record,
                    "cid": record.cid,
                    "authorDid": record.author_did,
                }))?;
                records_table.set(i + 1, entry)?;
            }
            result.set("records", records_table)?;
            match next_cursor {
                Some(c) => result.set("cursor", c)?,
                None => result.set("cursor", mlua::Value::Nil)?,
            }

            Ok(mlua::Value::Table(result))
        }
    })?;
    spaces_table.set("query", query_fn)?;

    atproto_table.set("spaces", spaces_table)?;

    lua.globals().set("atproto", atproto_table)?;
    Ok(())
}

/// Register blob upload capability on the existing `atproto` table.
///
/// Called only in procedure execution contexts where PDS auth is
/// available. `blob_download` is registered in `register_atproto_api`
/// (available everywhere); `blob_upload` needs caller credentials to
/// write to the caller's PDS.
pub(crate) fn register_atproto_blob_api(
    lua: &Lua,
    state: Arc<AppState>,
    claims: Arc<crate::auth::Claims>,
    pds_auth: Arc<crate::repo::PdsAuth>,
) -> LuaResult<()> {
    let atproto_table: mlua::Table = lua.globals().get("atproto")?;

    let upload_fn = lua.create_async_function(
        move |lua, (handle, content_type): (mlua::AnyUserData, String)| {
            let state = state.clone();
            let claims = claims.clone();
            let pds_auth = pds_auth.clone();
            async move {
                let blob_handle = handle.borrow::<BlobHandle>().map_err(|_| {
                    mlua::Error::runtime(
                        "blob_upload: first argument must be a BlobHandle from blob_download()",
                    )
                })?;
                let blob_bytes = blob_handle.data.clone();
                drop(blob_handle);

                let result =
                    upload_blob_to_pds(&state, claims.did(), &pds_auth, &content_type, blob_bytes)
                        .await
                        .map_err(|e| match e {
                            // The PDS's body names the reason (`BlobTooLarge`, a bad MIME
                            // type), which a script needs to act on.
                            crate::error::AppError::PdsError(status, body) => {
                                mlua::Error::runtime(format!(
                                    "blob_upload: PDS uploadBlob returned {status}: {}",
                                    String::from_utf8_lossy(&body)
                                ))
                            }
                            other => mlua::Error::runtime(format!("blob_upload: {other}")),
                        })?;

                lua.to_value(&result)
            }
        },
    )?;
    atproto_table.set("blob_upload", upload_fn)?;

    Ok(())
}

pub(crate) async fn upload_blob_to_pds(
    state: &AppState,
    caller_did: &str,
    pds_auth: &crate::repo::PdsAuth,
    content_type: &str,
    blob_bytes: Bytes,
) -> Result<serde_json::Value, crate::error::AppError> {
    use crate::error::AppError;
    use crate::repo::PdsAuth;

    match pds_auth {
        PdsAuth::OAuth(session) => {
            use atrium_xrpc::{
                InputDataOrBytes, OutputDataOrBytes, XrpcClient, XrpcRequest, http::Method,
            };

            let request = XrpcRequest {
                method: Method::POST,
                nsid: "com.atproto.repo.uploadBlob".to_string(),
                parameters: None::<()>,
                input: Some(InputDataOrBytes::<()>::Bytes(blob_bytes.to_vec())),
                encoding: Some(content_type.to_string()),
            };

            let result: Result<
                OutputDataOrBytes<serde_json::Value>,
                atrium_xrpc::Error<serde_json::Value>,
            > = session.send_xrpc(&request).await;

            match result {
                Ok(OutputDataOrBytes::Data(data)) => Ok(data),
                Ok(OutputDataOrBytes::Bytes(bytes)) => serde_json::from_slice(&bytes)
                    .map_err(|e| AppError::Internal(format!("invalid uploadBlob response: {e}"))),
                Err(atrium_xrpc::Error::XrpcResponse(xrpc_err)) => {
                    let status = axum::http::StatusCode::from_u16(xrpc_err.status.as_u16())
                        .unwrap_or(axum::http::StatusCode::BAD_GATEWAY);
                    let body = xrpc_err
                        .error
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    Err(AppError::PdsError(status, Bytes::from(body)))
                }
                Err(e) => Err(AppError::Internal(format!("PDS uploadBlob failed: {e}"))),
            }
        }
        PdsAuth::Dpop {
            api_client_id,
            dpop_key_id,
            encryption_key,
        } => {
            let resp = crate::oauth::pds_write::dpop_pds_post_blob(
                &state.http,
                &state.db,
                state.db_backend,
                encryption_key,
                &state.oauth,
                &state.config.plc_url,
                api_client_id,
                caller_did,
                dpop_key_id,
                content_type,
                blob_bytes,
            )
            .await?;

            let status = resp.status();
            let body = resp
                .bytes()
                .await
                .map_err(|e| AppError::Internal(format!("failed to read upload response: {e}")))?;

            if !status.is_success() {
                let axum_status = axum::http::StatusCode::from_u16(status.as_u16())
                    .unwrap_or(axum::http::StatusCode::BAD_GATEWAY);
                return Err(AppError::PdsError(axum_status, body));
            }

            serde_json::from_slice(&body)
                .map_err(|e| AppError::Internal(format!("invalid uploadBlob response: {e}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::db::DatabaseBackend;
    use crate::lexicon::LexiconRegistry;
    use tokio::sync::watch;

    fn test_state_with_plc(plc_url: &str) -> AppState {
        let config = Config {
            host: "127.0.0.1".into(),
            port: 3000,
            database_url: String::new(),
            database_backend: crate::db::DatabaseBackend::Sqlite,
            sqlite_journal_size_limit: crate::db::DEFAULT_JOURNAL_SIZE_LIMIT,
            public_url: String::new(),
            user_agent: String::new(),
            session_secret: "test-secret".into(),
            jetstream_url: String::new(),
            relay_url: String::new(),
            plc_url: plc_url.to_string(),
            static_dir: String::new(),
            base_path: None,
            event_log_retention_days: 30,
            app_name: None,
            logo_uri: None,
            tos_uri: None,
            policy_uri: None,
            // Spaces sign every commit with the `#atproto_space` key, which
            // is stored encrypted, so space writes need this set.
            token_encryption_key: Some(crate::test_support::TEST_ENCRYPTION_KEY),
            default_rate_limit_capacity: 100,
            default_rate_limit_refill_rate: 2.0,
            telemetry_collector_url: String::new(),
        };
        let (tx, _) = watch::channel(vec![]);
        let (labeler_tx, _) = watch::channel(());
        sqlx::any::install_default_drivers();
        let test_db = sqlx::AnyPool::connect_lazy("sqlite::memory:").unwrap();
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
            state_store: crate::auth::oauth_store::DbStateStore::new(
                test_db.clone(),
                crate::db::DatabaseBackend::Sqlite,
            ),
            session_store: crate::auth::oauth_store::DbSessionStore::new(
                test_db.clone(),
                crate::db::DatabaseBackend::Sqlite,
            ),
            resolver: atrium_oauth::OAuthResolverConfig {
                did_resolver,
                handle_resolver,
                authorization_server_metadata: Default::default(),
                protected_resource_metadata: Default::default(),
            },
            http_client: crate::http_retry::HappyViewHttpClient::default(),
        })
        .expect("Failed to create test OAuth client");
        AppState {
            config,
            http: reqwest::Client::new(),
            db: test_db.clone(),
            backfill_db: test_db.clone(),
            db_backend: DatabaseBackend::Sqlite,
            domain_cache: crate::domain::DomainCache::new(),
            lexicons: LexiconRegistry::new(),
            collections_tx: tx,
            labeler_subscriptions_tx: labeler_tx,
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
            oauth_state_store: crate::auth::oauth_store::DbStateStore::new(
                test_db.clone(),
                crate::db::DatabaseBackend::Sqlite,
            ),
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
                    crate::auth::oauth_store::DbStateStore::new(
                        test_db.clone(),
                        crate::db::DatabaseBackend::Sqlite,
                    ),
                    test_db.clone(),
                    crate::db::DatabaseBackend::Sqlite,
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
        }
    }

    #[tokio::test]
    async fn resolve_service_endpoint_returns_endpoint() {
        let mock = wiremock::MockServer::start().await;

        let did_doc = serde_json::json!({
            "id": "did:plc:test123",
            "alsoKnownAs": ["at://test.example.com"],
            "service": [{
                "id": "#atproto_pds",
                "type": "AtprotoPersonalDataServer",
                "serviceEndpoint": "https://pds.example.com"
            }]
        });

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/did:plc:test123"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&did_doc))
            .mount(&mock)
            .await;

        let state = test_state_with_plc(&mock.uri());
        let lua = mlua::Lua::new();
        register_atproto_api(&lua, Arc::new(state), None).unwrap();

        let chunk = r#"return atproto.resolve_service_endpoint("did:plc:test123")"#;
        let result: String = lua.load(chunk).eval_async().await.unwrap();
        assert_eq!(result, "https://pds.example.com");
    }

    #[tokio::test]
    async fn resolve_service_endpoint_returns_nil_on_failure() {
        let mock = wiremock::MockServer::start().await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/did:plc:unknown"))
            .respond_with(wiremock::ResponseTemplate::new(404))
            .mount(&mock)
            .await;

        let state = test_state_with_plc(&mock.uri());
        let lua = mlua::Lua::new();
        register_atproto_api(&lua, Arc::new(state), None).unwrap();

        let chunk = r#"return atproto.resolve_service_endpoint("did:plc:unknown")"#;
        let result: mlua::Value = lua.load(chunk).eval_async().await.unwrap();
        assert!(matches!(result, mlua::Value::Nil));
    }

    #[tokio::test]
    async fn resolve_did_web() {
        let mock = wiremock::MockServer::start().await;

        let state = test_state_with_plc(&mock.uri());
        let lua = mlua::Lua::new();
        register_atproto_api(&lua, Arc::new(state), None).unwrap();

        let chunk = r#"return type(atproto.resolve_service_endpoint)"#;
        let result: String = lua.load(chunk).eval_async().await.unwrap();
        assert_eq!(result, "function");
    }

    fn test_state_with_signer(plc_url: &str) -> AppState {
        let mut state = test_state_with_plc(plc_url);
        state.attestation_signer = Some(Arc::new(
            crate::plugin::attestation::AttestationSigner::for_testing(
                "did:web:test.example#signing".to_string(),
                "test.signature".to_string(),
            ),
        ));
        state
    }

    #[tokio::test]
    async fn sign_returns_signature_object() {
        let state = test_state_with_signer("");
        let lua = mlua::Lua::new();
        register_atproto_api(&lua, Arc::new(state), Some("did:plc:caller")).unwrap();

        let chunk = r#"
            local record = { contributionType = "correction", changes = { name = "Test" } }
            local sig = atproto.sign(record)
            return sig.key
        "#;
        let result: String = lua.load(chunk).eval_async().await.unwrap();
        assert_eq!(result, "did:web:test.example#signing");
    }

    #[tokio::test]
    async fn sign_returns_nil_without_signer() {
        let state = test_state_with_plc("");
        let lua = mlua::Lua::new();
        register_atproto_api(&lua, Arc::new(state), Some("did:plc:caller")).unwrap();

        let chunk = r#"return atproto.sign ~= nil"#;
        let result: bool = lua.load(chunk).eval_async().await.unwrap();
        // sign should not be registered when no signer is configured
        assert!(!result);
    }

    #[tokio::test]
    async fn verify_signature_roundtrip() {
        let state = test_state_with_signer("");
        let lua = mlua::Lua::new();
        register_atproto_api(&lua, Arc::new(state), Some("did:plc:caller")).unwrap();

        let chunk = r#"
            local record = { contributionType = "correction", changes = { name = "Test" } }
            local sig = atproto.sign(record)
            return atproto.verify_signature(record, sig, "did:plc:caller")
        "#;
        let result: bool = lua.load(chunk).eval_async().await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn verify_signature_rejects_wrong_did() {
        let state = test_state_with_signer("");
        let lua = mlua::Lua::new();
        register_atproto_api(&lua, Arc::new(state), Some("did:plc:caller")).unwrap();

        let chunk = r#"
            local record = { contributionType = "correction", changes = { name = "Test" } }
            local sig = atproto.sign(record)
            return atproto.verify_signature(record, sig, "did:plc:wrong")
        "#;
        let result: bool = lua.load(chunk).eval_async().await.unwrap();
        assert!(!result);
    }

    #[tokio::test]
    async fn verify_signature_rejects_tampered_record() {
        let state = test_state_with_signer("");
        let lua = mlua::Lua::new();
        register_atproto_api(&lua, Arc::new(state), Some("did:plc:caller")).unwrap();

        let chunk = r#"
            local record = { contributionType = "correction", changes = { name = "Original" } }
            local sig = atproto.sign(record)
            record.changes.name = "Tampered"
            return atproto.verify_signature(record, sig, "did:plc:caller")
        "#;
        let result: bool = lua.load(chunk).eval_async().await.unwrap();
        assert!(!result);
    }

    /// "We checked and it does not match" and "we could not check" are
    /// different facts, and only the first is a statement about the record.
    /// A script that cannot tell them apart will accuse a user of forgery
    /// because of a decode bug.
    #[tokio::test]
    async fn verify_signature_distinguishes_unverifiable_from_invalid() {
        let state = test_state_with_signer("");
        let lua = mlua::Lua::new();
        register_atproto_api(&lua, Arc::new(state), Some("did:plc:caller")).unwrap();

        let chunk = r#"
            local record = { contributionType = "correction", changes = { name = "Original" } }
            local sig = atproto.sign(record)

            -- A well-formed signature over a different payload: genuinely invalid.
            local tampered = { contributionType = "correction", changes = { name = "Tampered" } }
            local mismatch_ok, mismatch = pcall(atproto.verify_signature, tampered, sig, "did:plc:caller")

            -- Signature bytes that are not base64 at all: unverifiable.
            sig.signature["$bytes"] = "not!valid!base64"
            local undecodable_ok, undecodable = pcall(atproto.verify_signature, record, sig, "did:plc:caller")

            return mismatch_ok, mismatch, undecodable_ok, tostring(undecodable)
        "#;
        let (mismatch_ok, mismatch, undecodable_ok, undecodable): (bool, bool, bool, String) =
            lua.load(chunk).eval_async().await.unwrap();

        assert!(mismatch_ok, "a mismatch is an answer, not a failure");
        assert!(!mismatch, "a signature over a different payload is invalid");
        assert!(
            !undecodable_ok,
            "an unverifiable signature must not be reported as invalid"
        );
        assert!(
            undecodable.contains("invalid base64"),
            "the error must name the cause, got: {undecodable}"
        );
    }

    #[tokio::test]
    async fn spaces_api_is_registered() {
        let state = test_state_with_plc("");
        let lua = mlua::Lua::new();
        register_atproto_api(&lua, Arc::new(state), None).unwrap();

        let chunk = r#"
            return type(atproto.spaces) == "table"
                and type(atproto.spaces.is_member) == "function"
                and type(atproto.spaces.get_access) == "function"
                and type(atproto.spaces.list_members) == "function"
                and type(atproto.spaces.query) == "function"
        "#;
        let result: bool = lua.load(chunk).eval_async().await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn blob_handle_exposes_size_and_mime() {
        let state = test_state_with_plc("");
        let lua = mlua::Lua::new();
        register_atproto_api(&lua, Arc::new(state), None).unwrap();

        let handle = BlobHandle {
            data: axum::body::Bytes::from_static(b"hello world"),
            mime_type: "text/plain".to_string(),
        };
        lua.globals()
            .set("test_handle", lua.create_userdata(handle).unwrap())
            .unwrap();

        let size: usize = lua
            .load("return test_handle:size()")
            .eval_async()
            .await
            .unwrap();
        assert_eq!(size, 11);

        let mime: String = lua
            .load("return test_handle:mime_type()")
            .eval_async()
            .await
            .unwrap();
        assert_eq!(mime, "text/plain");
    }

    #[tokio::test]
    async fn blob_download_returns_handle_and_metadata() {
        let mock = wiremock::MockServer::start().await;

        let did_doc = serde_json::json!({
            "id": "did:plc:blobsource",
            "service": [{
                "id": "#atproto_pds",
                "type": "AtprotoPersonalDataServer",
                "serviceEndpoint": mock.uri()
            }]
        });
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/did:plc:blobsource"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&did_doc))
            .mount(&mock)
            .await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/xrpc/com.atproto.sync.getBlob"))
            .and(wiremock::matchers::query_param("did", "did:plc:blobsource"))
            .and(wiremock::matchers::query_param("cid", "bafytest123"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(vec![0x89, 0x50, 0x4E, 0x47]),
            )
            .mount(&mock)
            .await;

        let state = test_state_with_plc(&mock.uri());
        let lua = mlua::Lua::new();
        register_atproto_api(&lua, Arc::new(state), None).unwrap();

        let chunk = r#"
            local result = atproto.blob_download("did:plc:blobsource", "bafytest123")
            return {
                size = result.handle:size(),
                mimeType = result.mimeType,
            }
        "#;
        let result: mlua::Table = lua.load(chunk).eval_async().await.unwrap();
        assert_eq!(result.get::<usize>("size").unwrap(), 4);
        assert_eq!(result.get::<String>("mimeType").unwrap(), "image/png");
    }

    #[tokio::test]
    async fn blob_download_throws_on_404() {
        let mock = wiremock::MockServer::start().await;

        let did_doc = serde_json::json!({
            "id": "did:plc:blobsource",
            "service": [{
                "id": "#atproto_pds",
                "type": "AtprotoPersonalDataServer",
                "serviceEndpoint": mock.uri()
            }]
        });
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/did:plc:blobsource"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&did_doc))
            .mount(&mock)
            .await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/xrpc/com.atproto.sync.getBlob"))
            .respond_with(wiremock::ResponseTemplate::new(404))
            .mount(&mock)
            .await;

        let state = test_state_with_plc(&mock.uri());
        let lua = mlua::Lua::new();
        register_atproto_api(&lua, Arc::new(state), None).unwrap();

        let result: Result<mlua::Value, _> = lua
            .load(r#"return atproto.blob_download("did:plc:blobsource", "bafymissing")"#)
            .eval_async()
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn blob_download_defaults_mime_to_octet_stream() {
        let mock = wiremock::MockServer::start().await;

        let did_doc = serde_json::json!({
            "id": "did:plc:blobsource",
            "service": [{
                "id": "#atproto_pds",
                "type": "AtprotoPersonalDataServer",
                "serviceEndpoint": mock.uri()
            }]
        });
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/did:plc:blobsource"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&did_doc))
            .mount(&mock)
            .await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/xrpc/com.atproto.sync.getBlob"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(vec![0xFF, 0xD8]))
            .mount(&mock)
            .await;

        let state = test_state_with_plc(&mock.uri());
        let lua = mlua::Lua::new();
        register_atproto_api(&lua, Arc::new(state), None).unwrap();

        let chunk = r#"
            local result = atproto.blob_download("did:plc:blobsource", "bafynoheader")
            return result.mimeType
        "#;
        let result: String = lua.load(chunk).eval_async().await.unwrap();
        assert_eq!(result, "application/octet-stream");
    }

    #[tokio::test]
    async fn blob_download_throws_on_429() {
        let mock = wiremock::MockServer::start().await;

        let did_doc = serde_json::json!({
            "id": "did:plc:blobsource",
            "service": [{
                "id": "#atproto_pds",
                "type": "AtprotoPersonalDataServer",
                "serviceEndpoint": mock.uri()
            }]
        });
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/did:plc:blobsource"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&did_doc))
            .mount(&mock)
            .await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/xrpc/com.atproto.sync.getBlob"))
            .respond_with(wiremock::ResponseTemplate::new(429))
            .mount(&mock)
            .await;

        let state = test_state_with_plc(&mock.uri());
        let lua = mlua::Lua::new();
        register_atproto_api(&lua, Arc::new(state), None).unwrap();

        let result: Result<mlua::Value, _> = lua
            .load(r#"return atproto.blob_download("did:plc:blobsource", "bafyratelimit")"#)
            .eval_async()
            .await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("429"),
            "error should mention 429 status: {err_msg}"
        );
    }

    #[tokio::test]
    async fn blob_download_throws_on_did_resolution_failure() {
        let mock = wiremock::MockServer::start().await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/did:plc:nonexistent"))
            .respond_with(wiremock::ResponseTemplate::new(404))
            .mount(&mock)
            .await;

        let state = test_state_with_plc(&mock.uri());
        let lua = mlua::Lua::new();
        register_atproto_api(&lua, Arc::new(state), None).unwrap();

        let result: Result<mlua::Value, _> = lua
            .load(r#"return atproto.blob_download("did:plc:nonexistent", "bafytest")"#)
            .eval_async()
            .await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("resolve PDS"),
            "error should mention PDS resolution failure: {err_msg}"
        );
    }

    #[tokio::test]
    async fn blob_upload_throws_without_auth() {
        let state = test_state_with_plc("");
        let lua = mlua::Lua::new();
        register_atproto_api(&lua, Arc::new(state), None).unwrap();

        let has_upload: bool = lua
            .load("return atproto.blob_upload ~= nil")
            .eval_async()
            .await
            .unwrap();
        assert!(!has_upload);
    }
}
