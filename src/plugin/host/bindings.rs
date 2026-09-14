use std::collections::HashMap;
use std::sync::Arc;
use wasmtime::{Linker, Memory, TypedFunc};

use crate::plugin::caller::CallerSession;
use crate::plugin::capabilities::{PluginCapability, is_free_import, requirement_for_import};

/// State stored in wasmtime's Store during plugin execution
pub struct PluginState {
    pub plugin_id: String,
    pub scope: String,
    pub secrets: HashMap<String, String>,
    pub config: serde_json::Value,
    pub db: Option<sqlx::AnyPool>,
    pub db_backend: crate::db::DatabaseBackend,
    pub http_client: reqwest::Client,
    pub lexicons: Arc<crate::lexicon::LexiconRegistry>,
    pub usage: super::ResourceUsage,
    pub memory: Option<Memory>,
    pub alloc: Option<TypedFunc<u32, u32>>,
    pub dealloc: Option<TypedFunc<(u32, u32), ()>>,
    pub capabilities: std::collections::HashSet<crate::plugin::capabilities::PluginCapability>,
    pub allowed_hosts: Vec<String>,
    pub plugin_type: crate::plugin::PluginType,
    pub executor: Option<crate::plugin::PluginExecutor>,
    pub call_ctx: crate::plugin::library::LibraryCallContext,
    pub depth: u8,
    /// The credentials of the user whose script is running, when there are
    /// any. Every `host_caller_*` import refuses without it, except
    /// `host_caller_xrpc_query`, which a session-less query, record-event or
    /// label script can also reach.
    pub caller: Option<Arc<crate::plugin::caller::CallerSession>>,
    /// The full instance, carried from `PluginExecutor::app_state` so a
    /// caller-acting import can run without a `CallerSession` to source it
    /// from. `Some` for every real instantiation; `None` only for the
    /// direct-construction test and external-auth call sites that never
    /// reach such an import.
    pub app_state: Option<crate::AppState>,
}

/// Check that a memory access is within bounds
fn check_bounds(offset: usize, length: usize, mem_size: usize) -> Result<(usize, usize), ()> {
    if length == 0 {
        return Ok((offset, offset));
    }
    let end = offset.checked_add(length).ok_or(())?;
    if end > mem_size {
        return Err(());
    }
    Ok((offset, end))
}

/// Every host function except `host_log` starts here.
pub(super) fn require_capability(
    state: &PluginState,
    req: crate::plugin::capabilities::Requirement,
) -> Result<(), Vec<u8>> {
    if req.any_of.iter().any(|c| state.capabilities.contains(c)) {
        return Ok(());
    }
    let wanted: Vec<&str> = req.any_of.iter().map(|c| c.as_str()).collect();
    Err(error_envelope(
        "FORBIDDEN",
        format!(
            "plugin '{}' lacks the {} capability",
            state.plugin_id,
            wanted.join(" or ")
        ),
    ))
}

fn error_envelope(code: &str, message: impl std::fmt::Display) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "error": {"code": code, "message": message.to_string(), "retryable": false}
    }))
    .unwrap_or_default()
}

/// Register all host functions with the linker
pub fn register_host_functions(linker: &mut Linker<PluginState>) -> Result<(), wasmtime::Error> {
    // Sync functions
    linker.func_wrap("env", "host_log", host_log)?;

    // host_get_secret must be async because it calls alloc on an async store
    linker.func_wrap_async(
        "env",
        "host_get_secret",
        |mut caller: wasmtime::Caller<'_, PluginState>, (name_ptr, name_len): (i32, i32)| {
            Box::new(async move { host_get_secret_impl(&mut caller, name_ptr, name_len).await })
        },
    )?;

    // Async functions - HTTP
    linker.func_wrap_async(
        "env",
        "host_http_request",
        |mut caller: wasmtime::Caller<'_, PluginState>, (req_ptr, req_len): (i32, i32)| {
            Box::new(async move { host_http_request_impl(&mut caller, req_ptr, req_len).await })
        },
    )?;

    // Async functions - KV
    linker.func_wrap_async(
        "env",
        "host_kv_get",
        |mut caller: wasmtime::Caller<'_, PluginState>, (key_ptr, key_len): (i32, i32)| {
            Box::new(async move { host_kv_get_impl(&mut caller, key_ptr, key_len).await })
        },
    )?;

    linker.func_wrap_async(
        "env",
        "host_kv_set",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         (key_ptr, key_len, val_ptr, val_len, ttl): (i32, i32, i32, i32, i32)| {
            Box::new(async move {
                host_kv_set_impl(&mut caller, key_ptr, key_len, val_ptr, val_len, ttl).await
            })
        },
    )?;

    linker.func_wrap_async(
        "env",
        "host_kv_delete",
        |mut caller: wasmtime::Caller<'_, PluginState>, (key_ptr, key_len): (i32, i32)| {
            Box::new(async move { host_kv_delete_impl(&mut caller, key_ptr, key_len).await })
        },
    )?;

    // Async functions - Record lookup
    linker.func_wrap_async(
        "env",
        "host_lookup_record",
        |mut caller: wasmtime::Caller<'_, PluginState>, (req_ptr, req_len): (i32, i32)| {
            Box::new(async move { host_lookup_record_impl(&mut caller, req_ptr, req_len).await })
        },
    )?;

    linker.func_wrap_async(
        "env",
        "host_records_query",
        |mut caller: wasmtime::Caller<'_, PluginState>, (req_ptr, req_len): (i32, i32)| {
            Box::new(async move {
                host_spec_impl(
                    &mut caller,
                    "host_records_query",
                    req_ptr,
                    req_len,
                    |db, backend, spec| async move { super::records_query(&db, backend, spec).await },
                )
                .await
            })
        },
    )?;

    linker.func_wrap_async(
        "env",
        "host_records_count",
        |mut caller: wasmtime::Caller<'_, PluginState>, (req_ptr, req_len): (i32, i32)| {
            Box::new(async move {
                host_spec_impl(
                    &mut caller,
                    "host_records_count",
                    req_ptr,
                    req_len,
                    |db, backend, spec| async move { super::records_count(&db, backend, spec).await },
                )
                .await
            })
        },
    )?;

    linker.func_wrap_async(
        "env",
        "host_records_get",
        |mut caller: wasmtime::Caller<'_, PluginState>, (req_ptr, req_len): (i32, i32)| {
            Box::new(async move {
                host_spec_impl(
                    &mut caller,
                    "host_records_get",
                    req_ptr,
                    req_len,
                    |db, backend, spec: GetSpec| async move {
                        super::records_get(&db, backend, &spec.uri).await
                    },
                )
                .await
            })
        },
    )?;

    linker.func_wrap_async(
        "env",
        "host_records_search",
        |mut caller: wasmtime::Caller<'_, PluginState>, (req_ptr, req_len): (i32, i32)| {
            Box::new(async move {
                host_spec_impl(
                    &mut caller,
                    "host_records_search",
                    req_ptr,
                    req_len,
                    |db, backend, spec| async move { super::records_search(&db, backend, spec).await },
                )
                .await
            })
        },
    )?;

    linker.func_wrap_async(
        "env",
        "host_table_query",
        |mut caller: wasmtime::Caller<'_, PluginState>, (req_ptr, req_len): (i32, i32)| {
            Box::new(async move {
                host_spec_impl(
                    &mut caller,
                    "host_table_query",
                    req_ptr,
                    req_len,
                    |db, backend, spec| async move { super::table_query(&db, backend, spec).await },
                )
                .await
            })
        },
    )?;

    linker.func_wrap_async(
        "env",
        "host_backlinks_query",
        |mut caller: wasmtime::Caller<'_, PluginState>, (req_ptr, req_len): (i32, i32)| {
            Box::new(async move {
                host_spec_impl(
                    &mut caller,
                    "host_backlinks_query",
                    req_ptr,
                    req_len,
                    |db, backend, spec| async move { super::backlinks_query(&db, backend, spec).await },
                )
                .await
            })
        },
    )?;

    // Async functions - acting as the calling user
    linker.func_wrap_async(
        "env",
        "host_caller_create_record",
        |mut caller: wasmtime::Caller<'_, PluginState>, (req_ptr, req_len): (i32, i32)| {
            Box::new(async move {
                host_caller_impl(
                    &mut caller,
                    "host_caller_create_record",
                    req_ptr,
                    req_len,
                    |session, spec| async move { super::create_record(&session, spec).await },
                )
                .await
            })
        },
    )?;

    linker.func_wrap_async(
        "env",
        "host_caller_put_record",
        |mut caller: wasmtime::Caller<'_, PluginState>, (req_ptr, req_len): (i32, i32)| {
            Box::new(async move {
                host_caller_impl(
                    &mut caller,
                    "host_caller_put_record",
                    req_ptr,
                    req_len,
                    |session, spec| async move { super::put_record(&session, spec).await },
                )
                .await
            })
        },
    )?;

    linker.func_wrap_async(
        "env",
        "host_caller_delete_record",
        |mut caller: wasmtime::Caller<'_, PluginState>, (req_ptr, req_len): (i32, i32)| {
            Box::new(async move {
                host_caller_impl(
                    &mut caller,
                    "host_caller_delete_record",
                    req_ptr,
                    req_len,
                    |session, spec| async move { super::delete_record(&session, spec).await },
                )
                .await
            })
        },
    )?;

    linker.func_wrap_async(
        "env",
        "host_caller_upload_blob",
        |mut caller: wasmtime::Caller<'_, PluginState>, (req_ptr, req_len): (i32, i32)| {
            Box::new(async move {
                host_caller_impl(
                    &mut caller,
                    "host_caller_upload_blob",
                    req_ptr,
                    req_len,
                    |session, spec| async move { super::upload_blob(&session, spec).await },
                )
                .await
            })
        },
    )?;

    linker.func_wrap_async(
        "env",
        "host_caller_xrpc_query",
        |mut caller: wasmtime::Caller<'_, PluginState>, (req_ptr, req_len): (i32, i32)| {
            Box::new(async move { host_caller_query_impl(&mut caller, req_ptr, req_len).await })
        },
    )?;

    linker.func_wrap_async(
        "env",
        "host_caller_xrpc_procedure",
        |mut caller: wasmtime::Caller<'_, PluginState>, (req_ptr, req_len): (i32, i32)| {
            Box::new(async move {
                host_caller_impl(
                    &mut caller,
                    "host_caller_xrpc_procedure",
                    req_ptr,
                    req_len,
                    |session, spec| async move { super::xrpc_procedure(&session, spec).await },
                )
                .await
            })
        },
    )?;

    // Async functions - local index writes and lexicon reads
    linker.func_wrap_async(
        "env",
        "host_records_index_put",
        |mut caller: wasmtime::Caller<'_, PluginState>, (req_ptr, req_len): (i32, i32)| {
            Box::new(async move {
                host_spec_impl(
                    &mut caller,
                    "host_records_index_put",
                    req_ptr,
                    req_len,
                    |db, backend, spec| async move { super::index_put(&db, backend, spec).await },
                )
                .await
            })
        },
    )?;

    linker.func_wrap_async(
        "env",
        "host_records_index_delete",
        |mut caller: wasmtime::Caller<'_, PluginState>, (req_ptr, req_len): (i32, i32)| {
            Box::new(async move {
                host_spec_impl(
                    &mut caller,
                    "host_records_index_delete",
                    req_ptr,
                    req_len,
                    |db, backend, spec: happyview_plugin_sdk::wire::IndexDelete| async move {
                        super::index_delete(&db, backend, &spec.uri).await
                    },
                )
                .await
            })
        },
    )?;

    linker.func_wrap_async(
        "env",
        "host_lexicon_get",
        |mut caller: wasmtime::Caller<'_, PluginState>, (req_ptr, req_len): (i32, i32)| {
            Box::new(async move { host_lexicon_get_impl(&mut caller, req_ptr, req_len).await })
        },
    )?;

    linker.func_wrap_async(
        "env",
        "host_call_library",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         (lib_ptr, lib_len, fn_ptr, fn_len, args_ptr, args_len): (i32, i32, i32, i32, i32, i32)| {
            Box::new(async move {
                host_call_library_impl(&mut caller, lib_ptr, lib_len, fn_ptr, fn_len, args_ptr, args_len)
                    .await
            })
        },
    )?;

    linker.func_wrap_async(
        "env",
        "host_get_api_surface",
        |mut caller: wasmtime::Caller<'_, PluginState>, (lib_ptr, lib_len): (i32, i32)| {
            Box::new(async move { host_get_api_surface_impl(&mut caller, lib_ptr, lib_len).await })
        },
    )?;

    linker.func_wrap_async(
        "env",
        "host_db_query",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         (sql_ptr, sql_len, params_ptr, params_len): (i32, i32, i32, i32)| {
            Box::new(async move {
                host_db_impl(&mut caller, false, sql_ptr, sql_len, params_ptr, params_len).await
            })
        },
    )?;
    linker.func_wrap_async(
        "env",
        "host_db_execute",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         (sql_ptr, sql_len, params_ptr, params_len): (i32, i32, i32, i32)| {
            Box::new(async move {
                host_db_impl(&mut caller, true, sql_ptr, sql_len, params_ptr, params_len).await
            })
        },
    )?;

    Ok(())
}

/// Read a string from guest memory
fn read_guest_string(
    caller: &wasmtime::Caller<'_, PluginState>,
    ptr: i32,
    len: i32,
) -> Option<String> {
    let memory = caller.data().memory?;
    let mem_data = memory.data(caller);
    let (start, end) = check_bounds(ptr as usize, len as usize, mem_data.len()).ok()?;
    std::str::from_utf8(&mem_data[start..end])
        .ok()
        .map(String::from)
}

/// Read raw bytes from guest memory
fn read_guest_bytes(
    caller: &wasmtime::Caller<'_, PluginState>,
    ptr: i32,
    len: i32,
) -> Option<Vec<u8>> {
    let memory = caller.data().memory?;
    let mem_data = memory.data(caller);
    let (start, end) = check_bounds(ptr as usize, len as usize, mem_data.len()).ok()?;
    Some(mem_data[start..end].to_vec())
}

/// Write response data to guest memory, returning packed (ptr << 32) | len
async fn write_guest_response(caller: &mut wasmtime::Caller<'_, PluginState>, data: &[u8]) -> i64 {
    let memory = match caller.data().memory {
        Some(m) => m,
        None => return 0,
    };
    let alloc = match &caller.data().alloc {
        Some(a) => a.clone(),
        None => return 0,
    };

    let len = data.len() as u32;
    let ptr = match alloc.call_async(&mut *caller, len).await {
        Ok(p) if p != 0 => p,
        _ => return 0,
    };

    let mem_data = memory.data_mut(caller);
    if check_bounds(ptr as usize, len as usize, mem_data.len()).is_err() {
        return 0;
    }

    mem_data[ptr as usize..(ptr as usize + len as usize)].copy_from_slice(data);
    ((ptr as i64) << 32) | (len as i64)
}

/// Host function: log a message from the plugin
fn host_log(
    caller: wasmtime::Caller<'_, PluginState>,
    level_ptr: i32,
    level_len: i32,
    msg_ptr: i32,
    msg_len: i32,
) {
    let memory = match caller.data().memory {
        Some(m) => m,
        None => return,
    };

    let mem_data = memory.data(&caller);
    let mem_size = mem_data.len();

    let (level_start, level_end) =
        match check_bounds(level_ptr as usize, level_len as usize, mem_size) {
            Ok(bounds) => bounds,
            Err(_) => return,
        };

    let (msg_start, msg_end) = match check_bounds(msg_ptr as usize, msg_len as usize, mem_size) {
        Ok(bounds) => bounds,
        Err(_) => return,
    };

    let level = std::str::from_utf8(&mem_data[level_start..level_end]).unwrap_or("info");
    let msg = std::str::from_utf8(&mem_data[msg_start..msg_end]).unwrap_or("");

    let plugin_id = caller.data().plugin_id.clone();
    let db = caller.data().db.clone();
    let db_backend = caller.data().db_backend;
    let log_level: super::LogLevel = level.parse().unwrap_or_default();
    super::log(&plugin_id, log_level, msg, db, db_backend);
}

/// Host function: get a secret value by name
/// Returns a packed i64: (ptr << 32) | len, or 0 on error
///
/// The response is JSON-encoded as `{"ok": "<value>"}` so plugins can
/// deserialize it with the same `Response<String>` envelope they use for
/// other host calls.
async fn host_get_secret_impl(
    caller: &mut wasmtime::Caller<'_, PluginState>,
    name_ptr: i32,
    name_len: i32,
) -> i64 {
    if let Err(envelope) = require_capability(
        caller.data(),
        requirement_for_import("host_get_secret").unwrap(),
    ) {
        return write_guest_response(caller, &envelope).await;
    }

    let name = match read_guest_string(caller, name_ptr, name_len) {
        Some(n) => n,
        None => return 0,
    };

    let value = match caller.data().secrets.get(&name) {
        Some(v) => v.clone(),
        None => return 0,
    };

    let response_bytes = serde_json::to_vec(&serde_json::json!({"ok": value})).unwrap_or_default();
    write_guest_response(caller, &response_bytes).await
}

// ============================================================================
// Async host function implementations
// ============================================================================

/// Build a HostContext from PluginState, requires db to be present
fn build_host_context(state: &PluginState) -> Option<super::HostContext> {
    let db = state.db.clone()?;
    Some(super::HostContext {
        plugin_id: state.plugin_id.clone(),
        scope: state.scope.clone(),
        secrets: state.secrets.clone(),
        config: state.config.clone(),
        db,
        db_backend: state.db_backend,
        http_client: state.http_client.clone(),
        lexicons: state.lexicons.clone(),
    })
}

/// Host function: make an HTTP request
async fn host_http_request_impl(
    caller: &mut wasmtime::Caller<'_, PluginState>,
    req_ptr: i32,
    req_len: i32,
) -> i64 {
    if let Err(envelope) = require_capability(
        caller.data(),
        requirement_for_import("host_http_request").unwrap(),
    ) {
        return write_guest_response(caller, &envelope).await;
    }

    let req_bytes = match read_guest_bytes(caller, req_ptr, req_len) {
        Some(b) => b,
        None => {
            tracing::error!(
                "host_http_request: failed to read guest memory (ptr={req_ptr}, len={req_len})"
            );
            return 0;
        }
    };

    let request: super::HttpRequest = match serde_json::from_slice(&req_bytes) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("host_http_request: failed to parse request JSON: {e}");
            return 0;
        }
    };

    let url = request.url.clone();
    let method = request.method.clone();

    let unrestricted = {
        let state = caller.data();
        let unrestricted = state
            .capabilities
            .contains(&PluginCapability::NetworkRequestUnrestricted);
        if !unrestricted {
            let host = reqwest::Url::parse(&request.url)
                .ok()
                .and_then(|u| u.host_str().map(String::from));
            let ok = host
                .as_deref()
                .map(|h| super::host_allowed(&state.allowed_hosts, h))
                .unwrap_or(false);
            if !ok {
                return write_guest_response(
                    caller,
                    &error_envelope(
                        "FORBIDDEN",
                        format!("host {} is not in allowed_hosts", host.unwrap_or_default()),
                    ),
                )
                .await;
            }
        }
        unrestricted
    };

    let ctx = match build_host_context(caller.data()) {
        Some(c) => c,
        None => {
            tracing::error!("host_http_request: failed to build host context (db missing?)");
            return 0;
        }
    };

    let result = {
        let usage = &mut caller.data_mut().usage;
        super::http_request(&ctx, usage, request, unrestricted).await
    };

    let response_bytes = match result {
        Ok(resp) => serde_json::to_vec(&serde_json::json!({"ok": resp})).unwrap_or_default(),
        Err(e) => {
            tracing::warn!("host_http_request: HTTP {method} {url} failed: {e}");
            serde_json::to_vec(&serde_json::json!({
                "error": {"code": "HTTP_ERROR", "message": e.to_string(), "retryable": false}
            }))
            .unwrap_or_default()
        }
    };

    let packed = write_guest_response(caller, &response_bytes).await;
    if packed == 0 {
        tracing::error!(
            "host_http_request: write_guest_response returned 0 for {} {} (response_len={})",
            method,
            url,
            response_bytes.len()
        );
    }
    packed
}

/// Host function: get a value from KV store
async fn host_kv_get_impl(
    caller: &mut wasmtime::Caller<'_, PluginState>,
    key_ptr: i32,
    key_len: i32,
) -> i64 {
    if let Err(envelope) = require_capability(
        caller.data(),
        requirement_for_import("host_kv_get").unwrap(),
    ) {
        return write_guest_response(caller, &envelope).await;
    }

    let key = match read_guest_string(caller, key_ptr, key_len) {
        Some(k) => k,
        None => return 0,
    };

    let ctx = match build_host_context(caller.data()) {
        Some(c) => c,
        None => return 0,
    };

    let result = super::kv_get(&ctx, &key).await;

    let response_bytes = match result {
        Ok(Some(value)) => {
            serde_json::to_vec(&serde_json::json!({"ok": value})).unwrap_or_default()
        }
        Ok(None) => return 0,
        Err(e) => serde_json::to_vec(&serde_json::json!({
            "error": {"code": "KV_ERROR", "message": e.to_string(), "retryable": false}
        }))
        .unwrap_or_default(),
    };

    write_guest_response(caller, &response_bytes).await
}

/// Host function: set a value in KV store. Returns `0` on success and `-1`
/// on any failure, including a missing `kv:write` capability — the `env`
/// import signature is a bare `i32`, so there's no envelope to carry a
/// distinct "forbidden" code back to the guest.
async fn host_kv_set_impl(
    caller: &mut wasmtime::Caller<'_, PluginState>,
    key_ptr: i32,
    key_len: i32,
    val_ptr: i32,
    val_len: i32,
    ttl: i32,
) -> i32 {
    if require_capability(
        caller.data(),
        requirement_for_import("host_kv_set").unwrap(),
    )
    .is_err()
    {
        tracing::warn!(
            plugin_id = %caller.data().plugin_id,
            "host_kv_set: missing kv:write capability"
        );
        return -1;
    }

    let key = match read_guest_string(caller, key_ptr, key_len) {
        Some(k) => k,
        None => return -1,
    };
    let value = match read_guest_bytes(caller, val_ptr, val_len) {
        Some(v) => v,
        None => return -1,
    };

    let ttl_secs = if ttl > 0 { Some(ttl as u32) } else { None };

    let ctx = match build_host_context(caller.data()) {
        Some(c) => c,
        None => return -1,
    };

    let usage = &mut caller.data_mut().usage;
    match super::kv_set(&ctx, usage, &key, value, ttl_secs).await {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

/// Host function: delete a value from KV store. Returns `0` on success and
/// `-1` on any failure, including a missing `kv:write` capability — the
/// `env` import signature is a bare `i32`, so there's no envelope to carry a
/// distinct "forbidden" code back to the guest.
async fn host_kv_delete_impl(
    caller: &mut wasmtime::Caller<'_, PluginState>,
    key_ptr: i32,
    key_len: i32,
) -> i32 {
    if require_capability(
        caller.data(),
        requirement_for_import("host_kv_delete").unwrap(),
    )
    .is_err()
    {
        tracing::warn!(
            plugin_id = %caller.data().plugin_id,
            "host_kv_delete: missing kv:write capability"
        );
        return -1;
    }

    let key = match read_guest_string(caller, key_ptr, key_len) {
        Some(k) => k,
        None => return -1,
    };

    let ctx = match build_host_context(caller.data()) {
        Some(c) => c,
        None => return -1,
    };

    match super::kv_delete(&ctx, &key).await {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

/// Host function: look up an AT Protocol record
async fn host_lookup_record_impl(
    caller: &mut wasmtime::Caller<'_, PluginState>,
    req_ptr: i32,
    req_len: i32,
) -> i64 {
    if let Err(envelope) = require_capability(
        caller.data(),
        requirement_for_import("host_lookup_record").unwrap(),
    ) {
        return write_guest_response(caller, &envelope).await;
    }

    let req_bytes = match read_guest_bytes(caller, req_ptr, req_len) {
        Some(b) => b,
        None => return 0,
    };

    let request: super::LookupRequest = match serde_json::from_slice(&req_bytes) {
        Ok(r) => r,
        Err(_) => return 0,
    };

    let ctx = match build_host_context(caller.data()) {
        Some(c) => c,
        None => return 0,
    };

    let result = super::lookup_record_by_request(&ctx, request).await;

    let response_bytes = match result {
        Ok(record) => serde_json::to_vec(&serde_json::json!({"ok": record})).unwrap_or_default(),
        Err(e) => serde_json::to_vec(&serde_json::json!({
            "error": {"code": "LOOKUP_ERROR", "message": e.to_string(), "retryable": false}
        }))
        .unwrap_or_default(),
    };

    write_guest_response(caller, &response_bytes).await
}

/// `host_lexicon_get` reads the in-memory lexicon registry, not the database,
/// so it can't go through `host_spec_impl` — that refuses instances with no
/// pool, which a lexicon lookup never needed in the first place.
async fn host_lexicon_get_impl(
    caller: &mut wasmtime::Caller<'_, PluginState>,
    req_ptr: i32,
    req_len: i32,
) -> i64 {
    let Some(bytes) = read_guest_bytes(caller, req_ptr, req_len) else {
        return 0;
    };
    let spec: happyview_plugin_sdk::wire::LexiconGet = match serde_json::from_slice(&bytes) {
        Ok(s) => s,
        Err(e) => return write_guest_response(caller, &error_envelope("BAD_INPUT", e)).await,
    };
    let lexicons = caller.data().lexicons.clone();
    let value = super::lexicon_get(&lexicons, &spec.nsid).await;
    let response = serde_json::to_vec(&serde_json::json!({"ok": value})).unwrap_or_default();
    write_guest_response(caller, &response).await
}

/// One entry point for every spec-taking record/table query import: gate on
/// the import's capability, decode the spec, run it against the database, and
/// wrap the result in the standard envelope. `RecordsError::InvalidSpec`
/// becomes `INVALID_SPEC` rather than `DB_ERROR` so a plugin can tell a bad
/// query from a database failure.
async fn host_spec_impl<S, R, F, Fut>(
    caller: &mut wasmtime::Caller<'_, PluginState>,
    import: &'static str,
    req_ptr: i32,
    req_len: i32,
    run: F,
) -> i64
where
    S: serde::de::DeserializeOwned,
    R: serde::Serialize,
    F: FnOnce(sqlx::AnyPool, crate::db::DatabaseBackend, S) -> Fut,
    Fut: std::future::Future<Output = Result<R, super::RecordsError>>,
{
    // A free import has no row in the requirements table and nothing to gate
    // on; anything else missing one is a programming error, not a grant.
    if !is_free_import(import)
        && let Err(envelope) =
            require_capability(caller.data(), requirement_for_import(import).unwrap())
    {
        return write_guest_response(caller, &envelope).await;
    }
    let Some(bytes) = read_guest_bytes(caller, req_ptr, req_len) else {
        return 0;
    };
    let spec: S = match serde_json::from_slice(&bytes) {
        Ok(s) => s,
        Err(e) => return write_guest_response(caller, &error_envelope("BAD_INPUT", e)).await,
    };
    let Some(db) = caller.data().db.clone() else {
        return write_guest_response(caller, &error_envelope("HOST_ERROR", "no database")).await;
    };
    let backend = caller.data().db_backend;
    let response = match run(db, backend, spec).await {
        Ok(value) => serde_json::to_vec(&serde_json::json!({"ok": value})).unwrap_or_default(),
        Err(super::RecordsError::InvalidSpec(msg)) => error_envelope("INVALID_SPEC", msg),
        Err(e) => error_envelope("DB_ERROR", e),
    };
    write_guest_response(caller, &response).await
}

/// One entry point for every import that acts as the calling user: gate on
/// the import's capability, then on there being a user at all, then decode the
/// spec and run it. The session comes off the instance rather than the spec,
/// so a library cannot name a user it was not lent.
async fn host_caller_impl<S, R, F, Fut>(
    caller: &mut wasmtime::Caller<'_, PluginState>,
    import: &'static str,
    req_ptr: i32,
    req_len: i32,
    run: F,
) -> i64
where
    S: serde::de::DeserializeOwned,
    R: serde::Serialize,
    F: FnOnce(Arc<CallerSession>, S) -> Fut,
    Fut: std::future::Future<Output = Result<R, super::CallerError>>,
{
    if let Err(envelope) =
        require_capability(caller.data(), requirement_for_import(import).unwrap())
    {
        return write_guest_response(caller, &envelope).await;
    }
    let Some(session) = caller.data().caller.clone() else {
        return write_guest_response(
            caller,
            &error_envelope(
                "NO_SESSION",
                "this script context has no caller session, so nothing can be done as a user here",
            ),
        )
        .await;
    };
    let Some(bytes) = read_guest_bytes(caller, req_ptr, req_len) else {
        return 0;
    };
    let spec: S = match serde_json::from_slice(&bytes) {
        Ok(s) => s,
        Err(e) => return write_guest_response(caller, &error_envelope("BAD_INPUT", e)).await,
    };
    let response = match run(session, spec).await {
        Ok(value) => serde_json::to_vec(&serde_json::json!({"ok": value})).unwrap_or_default(),
        Err(e) => error_envelope(e.code(), caller_error_message(&e)),
    };
    write_guest_response(caller, &response).await
}

/// `host_caller_xrpc_query` on its own: gate on the capability, decode the
/// spec, and run it, but — unlike [`host_caller_impl`] — never refuse for
/// lacking a session. A query needs no PDS auth to run: the Lua `xrpc.query`
/// global has never required one, and a query, record-event or label script
/// has no session to lend. The session, when the runner has one, still
/// supplies its claims; otherwise the call context's `caller_did` stands in,
/// exactly as `xrpc.query` falls back to it today.
async fn host_caller_query_impl(
    caller: &mut wasmtime::Caller<'_, PluginState>,
    req_ptr: i32,
    req_len: i32,
) -> i64 {
    const IMPORT: &str = "host_caller_xrpc_query";
    if let Err(envelope) =
        require_capability(caller.data(), requirement_for_import(IMPORT).unwrap())
    {
        return write_guest_response(caller, &envelope).await;
    }
    let Some(bytes) = read_guest_bytes(caller, req_ptr, req_len) else {
        return 0;
    };
    let spec: happyview_plugin_sdk::wire::CallerXrpcQuery = match serde_json::from_slice(&bytes) {
        Ok(s) => s,
        Err(e) => return write_guest_response(caller, &error_envelope("BAD_INPUT", e)).await,
    };
    let Some(app_state) = caller.data().app_state.clone() else {
        return write_guest_response(
            caller,
            &error_envelope(
                "HOST_ERROR",
                "this instance has no app state to query against",
            ),
        )
        .await;
    };
    let session = caller.data().caller.clone();
    let caller_did = caller.data().call_ctx.caller_did.clone();
    let response = match super::xrpc_query(
        &app_state,
        session.as_deref(),
        caller_did.as_deref(),
        spec,
    )
    .await
    {
        Ok(value) => serde_json::to_vec(&serde_json::json!({"ok": value})).unwrap_or_default(),
        Err(e) => error_envelope(e.code(), caller_error_message(&e)),
    };
    write_guest_response(caller, &response).await
}

/// A dead session has to survive the trip out through the guest and the Lua
/// bridge, which flatten everything into one error string. The prefix is what
/// the script executor recognises there to answer 401 rather than 500.
fn caller_error_message(e: &super::CallerError) -> String {
    match e {
        super::CallerError::Auth(_) => {
            format!("{}{e}", crate::error::LUA_AUTH_ERROR_PREFIX)
        }
        other => other.to_string(),
    }
}

/// The host-side input to `host_records_get`: the SDK wrapper sends
/// `{"uri": ...}` rather than a bare string so it shares `host_spec_impl`'s
/// JSON-spec envelope with every other record/table import.
#[derive(serde::Deserialize)]
struct GetSpec {
    uri: String,
}

/// Gate on `library:call`, then hand back the executor.
fn library_access(
    state: &PluginState,
    import: &str,
) -> Result<crate::plugin::PluginExecutor, Vec<u8>> {
    require_capability(
        state,
        crate::plugin::capabilities::requirement_for_import(import).unwrap(),
    )?;
    state
        .executor
        .clone()
        .ok_or_else(|| error_envelope("HOST_ERROR", "no executor attached to this instance"))
}

async fn host_call_library_impl(
    caller: &mut wasmtime::Caller<'_, PluginState>,
    lib_ptr: i32,
    lib_len: i32,
    fn_ptr: i32,
    fn_len: i32,
    args_ptr: i32,
    args_len: i32,
) -> i64 {
    let (Some(lib), Some(function), Some(args_bytes)) = (
        read_guest_string(caller, lib_ptr, lib_len),
        read_guest_string(caller, fn_ptr, fn_len),
        read_guest_bytes(caller, args_ptr, args_len),
    ) else {
        return 0;
    };
    let args: Vec<serde_json::Value> = match serde_json::from_slice(&args_bytes) {
        Ok(serde_json::Value::Array(a)) => a,
        Ok(_) => {
            return write_guest_response(
                caller,
                &error_envelope("BAD_INPUT", "args must be a JSON array"),
            )
            .await;
        }
        Err(e) => return write_guest_response(caller, &error_envelope("BAD_INPUT", e)).await,
    };
    let executor = match library_access(caller.data(), "host_call_library") {
        Ok(e) => e,
        Err(envelope) => return write_guest_response(caller, &envelope).await,
    };
    let ctx = caller.data().call_ctx.clone();
    let depth = caller.data().depth + 1;
    // A library reached through another library acts as the same user, so the
    // session travels the whole chain rather than stopping at the first hop.
    let session = caller.data().caller.clone();
    let response = match executor
        .call_library_as(&lib, &function, &args, &ctx, session, depth)
        .await
    {
        Ok(value) => serde_json::to_vec(&serde_json::json!({"ok": value})).unwrap_or_default(),
        Err(crate::plugin::ExecutionError::PluginError { code, message, retryable }) => {
            serde_json::to_vec(&serde_json::json!({"error": {"code": code, "message": message, "retryable": retryable}}))
                .unwrap_or_default()
        }
        Err(e) => error_envelope("LIBRARY_ERROR", e),
    };
    write_guest_response(caller, &response).await
}

async fn host_get_api_surface_impl(
    caller: &mut wasmtime::Caller<'_, PluginState>,
    lib_ptr: i32,
    lib_len: i32,
) -> i64 {
    let Some(lib) = read_guest_string(caller, lib_ptr, lib_len) else {
        return 0;
    };
    let executor = match library_access(caller.data(), "host_get_api_surface") {
        Ok(e) => e,
        Err(envelope) => return write_guest_response(caller, &envelope).await,
    };
    let response = match executor.api_surface(&lib).await {
        Ok(surface) => {
            serde_json::to_vec(&serde_json::json!({"ok": &*surface})).unwrap_or_default()
        }
        Err(e) => error_envelope("LIBRARY_ERROR", e),
    };
    write_guest_response(caller, &response).await
}

/// Host function: run raw SQL. `execute` selects `host_db_execute` (needs
/// `database:write`) vs `host_db_query` (needs either `database:read` or
/// `database:write`, but read-only capability restricts it to read-only SQL).
/// SQL is passed through untranslated with backend-native placeholders
/// (`?` on SQLite, `$1`… on Postgres).
async fn host_db_impl(
    caller: &mut wasmtime::Caller<'_, PluginState>,
    execute: bool,
    sql_ptr: i32,
    sql_len: i32,
    params_ptr: i32,
    params_len: i32,
) -> i64 {
    let import = if execute {
        "host_db_execute"
    } else {
        "host_db_query"
    };
    if let Err(envelope) =
        require_capability(caller.data(), requirement_for_import(import).unwrap())
    {
        return write_guest_response(caller, &envelope).await;
    }
    let (Some(sql), Some(params_bytes)) = (
        read_guest_string(caller, sql_ptr, sql_len),
        read_guest_bytes(caller, params_ptr, params_len),
    ) else {
        return 0;
    };
    let params: Vec<serde_json::Value> = if params_bytes.is_empty() {
        Vec::new()
    } else {
        match serde_json::from_slice(&params_bytes) {
            Ok(serde_json::Value::Array(a)) => a,
            _ => {
                return write_guest_response(
                    caller,
                    &error_envelope("BAD_INPUT", "params must be a JSON array"),
                )
                .await;
            }
        }
    };
    // database:read alone may only run queries; database:write may run anything through either import.
    if !caller
        .data()
        .capabilities
        .contains(&PluginCapability::DatabaseWrite)
    {
        match crate::raw_sql_guard::is_read_only(&sql) {
            Ok(true) => {}
            Ok(false) => {
                return write_guest_response(
                    caller,
                    &error_envelope(
                        "FORBIDDEN",
                        "database:read permits only read-only statements; declare database:write to modify data",
                    ),
                )
                .await;
            }
            Err(e) => return write_guest_response(caller, &error_envelope("BAD_INPUT", e)).await,
        }
    }
    let Some(db) = caller.data().db.clone() else {
        return write_guest_response(caller, &error_envelope("HOST_ERROR", "no database")).await;
    };
    let response = if execute {
        match super::run_execute(&db, &sql, &params).await {
            Ok(n) => serde_json::to_vec(&serde_json::json!({"ok": {"rows_affected": n}}))
                .unwrap_or_default(),
            Err(e) => error_envelope("DB_ERROR", e),
        }
    } else {
        match super::run_query(&db, &sql, &params).await {
            Ok(rows) => serde_json::to_vec(&serde_json::json!({"ok": rows})).unwrap_or_default(),
            Err(e) => error_envelope("DB_ERROR", e),
        }
    };
    write_guest_response(caller, &response).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_state_fields_exist() {
        fn _check_fields(state: &PluginState) {
            let _ = &state.plugin_id;
            let _ = &state.scope;
            let _ = &state.secrets;
            let _ = &state.config;
            let _ = &state.usage;
            let _ = &state.memory;
            let _ = &state.alloc;
            let _ = &state.dealloc;
        }
    }

    /// The executor recovers a 401 by finding the prefix anywhere in the
    /// error string, so the envelope has to carry it out of the guest.
    #[test]
    fn an_auth_failure_leaves_with_the_prefix_the_executor_looks_for() {
        let envelope = error_envelope(
            super::super::CallerError::Auth("expired".into()).code(),
            caller_error_message(&super::super::CallerError::Auth("expired".into())),
        );
        let parsed: serde_json::Value = serde_json::from_slice(&envelope).unwrap();
        assert_eq!(parsed["error"]["code"], "AUTH_REQUIRED");
        let message = parsed["error"]["message"].as_str().unwrap();
        assert!(
            message.contains(crate::error::LUA_AUTH_ERROR_PREFIX),
            "{message}"
        );
        assert!(message.contains("expired"), "{message}");
    }

    /// Only an auth failure gets the prefix; anything else wearing it would
    /// turn an unrelated error into a spurious logout.
    #[test]
    fn other_failures_are_not_dressed_as_auth_failures() {
        let message = caller_error_message(&super::super::CallerError::Pds {
            status: 400,
            body: "InvalidSwap".into(),
        });
        assert!(
            !message.contains(crate::error::LUA_AUTH_ERROR_PREFIX),
            "{message}"
        );
    }

    #[test]
    fn test_pack_ptr_len() {
        let ptr: u32 = 0x1000;
        let len: u32 = 0x0100;
        let packed: i64 = ((ptr as i64) << 32) | (len as i64);
        let unpacked_ptr = (packed >> 32) as u32;
        let unpacked_len = (packed & 0xFFFFFFFF) as u32;
        assert_eq!(unpacked_ptr, ptr);
        assert_eq!(unpacked_len, len);
    }

    #[test]
    fn test_bounds_check_helper() {
        assert!(check_bounds(0, 10, 100).is_ok());
        assert!(check_bounds(90, 10, 100).is_ok());
        assert!(check_bounds(91, 10, 100).is_err());
        assert!(check_bounds(0, 0, 100).is_ok());
    }
}
