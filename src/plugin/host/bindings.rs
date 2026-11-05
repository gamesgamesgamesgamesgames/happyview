use std::collections::HashMap;
use std::sync::Arc;
use wasmtime::{Linker, Memory, TypedFunc};

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

    let result = {
        let usage = &mut caller.data_mut().usage;
        super::http_request(usage, request).await
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

/// Host function: set a value in KV store
async fn host_kv_set_impl(
    caller: &mut wasmtime::Caller<'_, PluginState>,
    key_ptr: i32,
    key_len: i32,
    val_ptr: i32,
    val_len: i32,
    ttl: i32,
) -> i32 {
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

/// Host function: delete a value from KV store
async fn host_kv_delete_impl(
    caller: &mut wasmtime::Caller<'_, PluginState>,
    key_ptr: i32,
    key_len: i32,
) -> i32 {
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
