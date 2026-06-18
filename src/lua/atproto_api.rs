use axum::body::Bytes;
use mlua::{Lua, LuaSerdeExt, Result as LuaResult};
use std::sync::Arc;

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339};
use crate::profile;

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

/// Register the `atproto` table with AT Protocol utility functions.
///
/// When `caller_did` is provided, the `atproto.sign(record)` function is
/// available for inline attestation signing.
pub fn register_atproto_api(
    lua: &Lua,
    state: Arc<AppState>,
    caller_did: Option<&str>,
) -> LuaResult<()> {
    let atproto_table = lua.create_table()?;

    let state_clone = state.clone();
    let resolve_fn = lua.create_async_function(move |_lua, did: String| {
        let state = state_clone.clone();
        async move {
            let result =
                profile::resolve_pds_endpoint(&state.http, &state.config.plc_url, &did).await;

            match result {
                Ok(endpoint) => Ok(Some(endpoint)),
                Err(_) => Ok(None),
            }
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
                    let pds_endpoint =
                        profile::resolve_pds_endpoint(&state.http, &state.config.plc_url, &did)
                            .await
                            .map_err(|e| {
                                mlua::Error::runtime(format!(
                                    "blob_download: failed to resolve PDS for {did}: {e}"
                                ))
                            })?;

                    let url = format!(
                        "{}/xrpc/com.atproto.sync.getBlob?did={}&cid={}",
                        pds_endpoint,
                        urlencoding::encode(&did),
                        urlencoding::encode(&cid),
                    );

                    let response = state.http.get(&url).send().await.map_err(|e| {
                        mlua::Error::runtime(format!("blob_download: request failed: {e}"))
                    })?;

                    let status = response.status();
                    if !status.is_success() {
                        return Err(mlua::Error::runtime(format!(
                            "blob_download: PDS returned {status} for did={did} cid={cid}"
                        )));
                    }

                    let mime_type = response
                        .headers()
                        .get("content-type")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("application/octet-stream")
                        .to_string();

                    let bytes = response.bytes().await.map_err(|e| {
                        mlua::Error::runtime(format!("blob_download: failed to read body: {e}"))
                    })?;

                    let size = bytes.len();
                    let handle = BlobHandle {
                        data: bytes,
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
            let backend = state.db_backend;
            let now = now_rfc3339();
            let sql = adapt_sql(
                "SELECT src, uri, val, cts FROM labels WHERE uri = ? AND (exp IS NULL OR exp > ?)",
                backend,
            );
            let rows: Vec<(String, String, String, String)> = sqlx::query_as(&sql)
                .bind(&uri)
                .bind(&now)
                .fetch_all(&state.db)
                .await
                .map_err(|e| mlua::Error::runtime(format!("label query failed: {e}")))?;

            let result = lua.create_table()?;
            let mut idx = 1;

            for (src, label_uri, val, cts) in &rows {
                let label = lua.create_table()?;
                label.set("src", src.as_str())?;
                label.set("uri", label_uri.as_str())?;
                label.set("val", val.as_str())?;
                label.set("cts", cts.as_str())?;
                result.set(idx, label)?;
                idx += 1;
            }

            // Check for self-labels in the record itself.
            let record_sql = adapt_sql("SELECT did, record FROM records WHERE uri = ?", backend);
            let record: Option<(String, String)> = sqlx::query_as(&record_sql)
                .bind(&uri)
                .fetch_optional(&state.db)
                .await
                .map_err(|e| mlua::Error::runtime(format!("record query failed: {e}")))?;

            if let Some((did, record_str)) = record {
                let record_val: serde_json::Value =
                    serde_json::from_str(&record_str).unwrap_or(serde_json::json!({}));
                if let Some(labels) = record_val.get("labels")
                    && let Some(values) = labels.get("values")
                    && let Some(arr) = values.as_array()
                {
                    for item in arr {
                        if let Some(val) = item.get("val").and_then(|v| v.as_str()) {
                            let label = lua.create_table()?;
                            label.set("src", did.as_str())?;
                            label.set("uri", uri.as_str())?;
                            label.set("val", val)?;
                            label.set("cts", "")?;
                            result.set(idx, label)?;
                            idx += 1;
                        }
                    }
                }
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
            let backend = state.db_backend;
            // Collect URIs from the Lua table.
            let uri_list: Vec<String> = uris
                .sequence_values::<String>()
                .collect::<Result<Vec<_>, _>>()?;

            let now = now_rfc3339();

            // Query labels for all URIs (one query per URI since AnyPool doesn't support array binding).
            let label_sql = adapt_sql(
                "SELECT src, uri, val, cts FROM labels WHERE uri = ? AND (exp IS NULL OR exp > ?)",
                backend,
            );
            let mut rows: Vec<(String, String, String, String)> = Vec::new();
            for uri in &uri_list {
                let mut uri_rows: Vec<(String, String, String, String)> =
                    sqlx::query_as(&label_sql)
                        .bind(uri)
                        .bind(&now)
                        .fetch_all(&state.db)
                        .await
                        .map_err(|e| {
                            mlua::Error::runtime(format!("label batch query failed: {e}"))
                        })?;
                rows.append(&mut uri_rows);
            }

            // Query records for self-labels.
            let record_sql = adapt_sql(
                "SELECT uri, did, record FROM records WHERE uri = ?",
                backend,
            );
            let mut records: Vec<(String, String, String)> = Vec::new();
            for uri in &uri_list {
                let mut uri_records: Vec<(String, String, String)> = sqlx::query_as(&record_sql)
                    .bind(uri)
                    .fetch_all(&state.db)
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("record batch query failed: {e}")))?;
                records.append(&mut uri_records);
            }

            // Build result table keyed by URI.
            let result = lua.create_table()?;

            // Initialize empty arrays for each URI.
            let mut counters: std::collections::HashMap<String, i32> =
                std::collections::HashMap::new();
            for uri in &uri_list {
                result.set(uri.as_str(), lua.create_table()?)?;
                counters.insert(uri.clone(), 1);
            }

            // Add external labels.
            for (src, uri, val, cts) in &rows {
                let label = lua.create_table()?;
                label.set("src", src.as_str())?;
                label.set("uri", uri.as_str())?;
                label.set("val", val.as_str())?;
                label.set("cts", cts.as_str())?;

                let uri_table: mlua::Table = result.get(uri.as_str())?;
                let idx = counters.get(uri).copied().unwrap_or(1);
                uri_table.set(idx, label)?;
                counters.insert(uri.clone(), idx + 1);
            }

            // Add self-labels from records.
            for (uri, did, record_str) in &records {
                let record_val: serde_json::Value =
                    serde_json::from_str(record_str).unwrap_or(serde_json::json!({}));
                if let Some(labels) = record_val.get("labels")
                    && let Some(values) = labels.get("values")
                    && let Some(arr) = values.as_array()
                {
                    for item in arr {
                        if let Some(val) = item.get("val").and_then(|v| v.as_str()) {
                            let label = lua.create_table()?;
                            label.set("src", did.as_str())?;
                            label.set("uri", uri.as_str())?;
                            label.set("val", val)?;
                            label.set("cts", "")?;

                            let uri_table: mlua::Table = result.get(uri.as_str())?;
                            let idx = counters.get(uri).copied().unwrap_or(1);
                            uri_table.set(idx, label)?;
                            counters.insert(uri.clone(), idx + 1);
                        }
                    }
                }
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
            let mut record: serde_json::Value = lua
                .from_value(table)
                .map_err(|e| mlua::Error::runtime(format!("atproto.sign: {e}")))?;

            signer
                .sign_record(&mut record, &did)
                .map_err(|e| mlua::Error::runtime(format!("atproto.sign: {e}")))?;

            // Extract the last signature (the one we just added)
            let sig = record
                .get("signatures")
                .and_then(|s| s.as_array())
                .and_then(|arr| arr.last())
                .cloned()
                .ok_or_else(|| mlua::Error::runtime("atproto.sign: no signature produced"))?;

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

                match signer.verify_record_signature(&record_json, &sig_json, &repo_did) {
                    Ok(valid) => Ok(valid),
                    Err(e) => {
                        tracing::debug!(error = %e, "atproto.verify_signature failed");
                        Ok(false)
                    }
                }
            },
        )?;
        atproto_table.set("verify_signature", verify_fn)?;
    }

    // atproto.spaces sub-table
    let spaces_table = lua.create_table()?;

    // atproto.spaces.is_member(space_uri, did) -> boolean
    let state_clone = state.clone();
    let is_member_fn =
        lua.create_async_function(move |_lua, (space_uri, did): (String, String)| {
            let state = state_clone.clone();
            async move {
                if !crate::feature_flags::is_enabled(
                    &state.db,
                    crate::feature_flags::FeatureFlag::SPACES_ENABLED,
                    state.db_backend,
                )
                .await
                {
                    return Err(mlua::Error::runtime("spaces feature is not enabled"));
                }
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

    // atproto.spaces.get_access(space_uri, did) -> 'read' | 'write' | nil
    let state_clone = state.clone();
    let get_access_fn =
        lua.create_async_function(move |_lua, (space_uri, did): (String, String)| {
            let state = state_clone.clone();
            async move {
                if !crate::feature_flags::is_enabled(
                    &state.db,
                    crate::feature_flags::FeatureFlag::SPACES_ENABLED,
                    state.db_backend,
                )
                .await
                {
                    return Err(mlua::Error::runtime("spaces feature is not enabled"));
                }
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
                Ok(access.map(|a| a.as_str().to_string()))
            }
        })?;
    spaces_table.set("get_access", get_access_fn)?;

    // atproto.spaces.list_members(space_uri) -> array of { did, access }
    let state_clone = state.clone();
    let list_members_fn = lua.create_async_function(move |lua, space_uri: String| {
        let state = state_clone.clone();
        async move {
            if !crate::feature_flags::is_enabled(
                &state.db,
                crate::feature_flags::FeatureFlag::SPACES_ENABLED,
                state.db_backend,
            )
            .await
            {
                return Err(mlua::Error::runtime("spaces feature is not enabled"));
            }
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
                entry.set("access", member.access.as_str())?;
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
            if !crate::feature_flags::is_enabled(
                &state.db,
                crate::feature_flags::FeatureFlag::SPACES_ENABLED,
                state.db_backend,
            )
            .await
            {
                return Err(mlua::Error::runtime("spaces feature is not enabled"));
            }
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
                        .map_err(|e| mlua::Error::runtime(format!("blob_upload: {e}")))?;

                lua.to_value(&result)
            }
        },
    )?;
    atproto_table.set("blob_upload", upload_fn)?;

    Ok(())
}

async fn upload_blob_to_pds(
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
                let body_str = String::from_utf8_lossy(&body);
                return Err(AppError::Internal(format!(
                    "PDS uploadBlob returned {status}: {body_str}"
                )));
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
            public_url: String::new(),
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
            token_encryption_key: None,
            default_rate_limit_capacity: 100,
            default_rate_limit_refill_rate: 2.0,
        };
        let (tx, _) = watch::channel(vec![]);
        let (labeler_tx, _) = watch::channel(());
        sqlx::any::install_default_drivers();
        let test_db = sqlx::AnyPool::connect_lazy("sqlite::memory:").unwrap();
        let atrium_http = std::sync::Arc::new(atrium_oauth::DefaultHttpClient::default());
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
