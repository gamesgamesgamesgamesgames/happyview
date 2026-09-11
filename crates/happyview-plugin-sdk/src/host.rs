//! The host functions a plugin may import, and typed wrappers over them.
//!
//! Every import lives in the `env` module. From Rust 1.96 the wasm32 linker no
//! longer turns a bare undefined symbol into an import, so the `extern` block
//! must carry `#[link(wasm_import_module = "env")]` — without it the build
//! fails to link rather than producing a module with a missing import.
//!
//! Each wrapper names the capability the plugin's `manifest.json` must declare.
//! The loader refuses a plugin whose imports need more than it declared, so an
//! *unused* wrapper costs nothing: the linker drops the import with the code.
//!
//! Outside wasm32 every wrapper returns [`HostError::NotWasm`], which keeps a
//! plugin's pure logic unit-testable natively.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[cfg(any(target_arch = "wasm32", test))]
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[cfg(any(target_arch = "wasm32", test))]
use crate::abi::read_packed;
use crate::envelope::PluginError;
#[cfg(any(target_arch = "wasm32", test))]
use crate::envelope::Response;
use crate::types::{ApiSurface, StrongRef};

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "env")]
extern "C" {
    fn host_log(level_ptr: i32, level_len: i32, msg_ptr: i32, msg_len: i32);
    fn host_get_secret(name_ptr: i32, name_len: i32) -> i64;
    fn host_http_request(req_ptr: i32, req_len: i32) -> i64;
    fn host_kv_get(key_ptr: i32, key_len: i32) -> i64;
    fn host_kv_set(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32, ttl_secs: i32) -> i32;
    fn host_kv_delete(key_ptr: i32, key_len: i32) -> i32;
    fn host_lookup_record(req_ptr: i32, req_len: i32) -> i64;
    fn host_call_library(
        lib_ptr: i32,
        lib_len: i32,
        fn_ptr: i32,
        fn_len: i32,
        args_ptr: i32,
        args_len: i32,
    ) -> i64;
    fn host_get_api_surface(lib_ptr: i32, lib_len: i32) -> i64;
    fn host_db_query(sql_ptr: i32, sql_len: i32, params_ptr: i32, params_len: i32) -> i64;
    fn host_db_execute(sql_ptr: i32, sql_len: i32, params_ptr: i32, params_len: i32) -> i64;
}

/// Why a host call did not produce a value.
#[derive(Debug, Clone, PartialEq)]
pub enum HostError {
    /// Built for a target with no host to call. Only ever seen in native tests.
    NotWasm,
    /// The host returned a packed 0: it could not read our request, or could
    /// not write a response.
    NoResponse,
    /// The host answered with an error envelope.
    Plugin(PluginError),
}

impl From<HostError> for PluginError {
    fn from(err: HostError) -> Self {
        match err {
            HostError::NotWasm => {
                PluginError::host("host functions are unavailable outside wasm32")
            }
            HostError::NoResponse => PluginError::host("host returned no response"),
            HostError::Plugin(err) => err,
        }
    }
}

impl From<PluginError> for HostError {
    fn from(err: PluginError) -> Self {
        HostError::Plugin(err)
    }
}

impl core::fmt::Display for HostError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            HostError::NotWasm => f.write_str("host functions are unavailable outside wasm32"),
            HostError::NoResponse => f.write_str("host returned no response"),
            HostError::Plugin(err) => write!(f, "{err}"),
        }
    }
}

/// Severity for [`log`]. The host parses these exact strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    pub fn as_str(&self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

/// Write a line to the plugin's log. Needs no capability, and cannot fail —
/// a message the host cannot read is dropped.
pub fn log(level: Level, message: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        let level = level.as_str();
        // SAFETY: both slices are live for the duration of the call.
        unsafe {
            host_log(
                level.as_ptr() as i32,
                level.len() as i32,
                message.as_ptr() as i32,
                message.len() as i32,
            );
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (level, message);
    }
}

/// Log at `debug`. Needs no capability.
pub fn debug(message: &str) {
    log(Level::Debug, message);
}

/// Log at `info`. Needs no capability.
pub fn info(message: &str) {
    log(Level::Info, message);
}

/// Log at `warn`. Needs no capability.
pub fn warn(message: &str) {
    log(Level::Warn, message);
}

/// Log at `error`. Needs no capability.
pub fn error(message: &str) {
    log(Level::Error, message);
}

/// Read one of the plugin's configured secrets. `Ok(None)` means the operator
/// did not set it. Needs `secrets:read`.
pub fn get_secret(name: &str) -> Result<Option<String>, HostError> {
    #[cfg(target_arch = "wasm32")]
    {
        // SAFETY: `name` is live for the duration of the call.
        let packed = unsafe { host_get_secret(name.as_ptr() as i32, name.len() as i32) };
        decode_optional::<String>(packed)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = name;
        Err(HostError::NotWasm)
    }
}

/// An outbound HTTP request. `headers` are sent as the host expects them,
/// `[[name, value], ...]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub body: Option<String>,
}

impl HttpRequest {
    /// A request with no headers and no body.
    pub fn new(method: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            url: url.into(),
            headers: Vec::new(),
            body: None,
        }
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }
}

/// A response from [`http_request`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// The host sends this as a string when the body is valid UTF-8 and as a
    /// byte array when it is not; a non-UTF-8 body arrives lossily converted.
    #[serde(default, deserialize_with = "deserialize_body")]
    pub body: String,
}

impl HttpResponse {
    /// Look up a response header, ignoring case. The host does not normalise
    /// header names, so a case-sensitive match would miss most of them.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

fn deserialize_body<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    struct BodyVisitor;

    impl<'de> serde::de::Visitor<'de> for BodyVisitor {
        type Value = String;

        fn expecting(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
            f.write_str("a string, a byte array, or null")
        }

        fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<String, E> {
            Ok(v.to_string())
        }

        fn visit_string<E: serde::de::Error>(self, v: String) -> Result<String, E> {
            Ok(v)
        }

        fn visit_unit<E: serde::de::Error>(self) -> Result<String, E> {
            Ok(String::new())
        }

        fn visit_none<E: serde::de::Error>(self) -> Result<String, E> {
            Ok(String::new())
        }

        fn visit_some<D: serde::Deserializer<'de>>(self, d: D) -> Result<String, D::Error> {
            d.deserialize_any(BodyVisitor)
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, seq: A) -> Result<String, A::Error> {
            let bytes: Vec<u8> =
                serde::Deserialize::deserialize(serde::de::value::SeqAccessDeserializer::new(seq))?;
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        }
    }

    deserializer.deserialize_any(BodyVisitor)
}

/// Send an outbound HTTP request. Needs `network:request` (limited to the
/// manifest's `allowed_hosts`) or `network:request:unrestricted`.
pub fn http_request(request: &HttpRequest) -> Result<HttpResponse, HostError> {
    #[cfg(target_arch = "wasm32")]
    {
        let bytes = serde_json::to_vec(request).map_err(PluginError::from)?;
        // SAFETY: `bytes` is live for the duration of the call.
        let packed = unsafe { host_http_request(bytes.as_ptr() as i32, bytes.len() as i32) };
        decode_required(packed)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = request;
        Err(HostError::NotWasm)
    }
}

/// Read a value from the plugin's key/value store. `Ok(None)` means the key is
/// absent or expired. Needs `kv:read`.
pub fn kv_get(key: &str) -> Result<Option<Vec<u8>>, HostError> {
    #[cfg(target_arch = "wasm32")]
    {
        // SAFETY: `key` is live for the duration of the call.
        let packed = unsafe { host_kv_get(key.as_ptr() as i32, key.len() as i32) };
        decode_optional::<Vec<u8>>(packed)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = key;
        Err(HostError::NotWasm)
    }
}

/// Write a value to the key/value store, optionally expiring after `ttl_secs`.
/// Needs `kv:write`.
///
/// The import returns a bare `i32`, so a refusal carries no detail: a missing
/// capability and a store failure are the same `-1`.
pub fn kv_set(key: &str, value: &[u8], ttl_secs: Option<u32>) -> Result<(), HostError> {
    #[cfg(target_arch = "wasm32")]
    {
        let ttl = ttl_secs.unwrap_or(0).min(i32::MAX as u32) as i32;
        // SAFETY: both slices are live for the duration of the call.
        let code = unsafe {
            host_kv_set(
                key.as_ptr() as i32,
                key.len() as i32,
                value.as_ptr() as i32,
                value.len() as i32,
                ttl,
            )
        };
        decode_status(code, "host_kv_set")
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (key, value, ttl_secs);
        Err(HostError::NotWasm)
    }
}

/// Remove a key from the key/value store. Needs `kv:write`.
///
/// As with [`kv_set`], a refusal and a failure are indistinguishable.
pub fn kv_delete(key: &str) -> Result<(), HostError> {
    #[cfg(target_arch = "wasm32")]
    {
        // SAFETY: `key` is live for the duration of the call.
        let code = unsafe { host_kv_delete(key.as_ptr() as i32, key.len() as i32) };
        decode_status(code, "host_kv_delete")
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = key;
        Err(HostError::NotWasm)
    }
}

/// Which indexed record to look for, by a value nested inside it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LookupRequest {
    pub collection: String,
    /// Dotted path into the record, e.g. `externalIds.steam`.
    pub external_id_field: String,
    pub external_id_value: String,
}

impl LookupRequest {
    pub fn new(
        collection: impl Into<String>,
        external_id_field: impl Into<String>,
        external_id_value: impl Into<String>,
    ) -> Self {
        Self {
            collection: collection.into(),
            external_id_field: external_id_field.into(),
            external_id_value: external_id_value.into(),
        }
    }
}

/// Find an indexed record by an external id. `Ok(None)` means nothing matched.
/// Needs `records:read`.
pub fn lookup_record(request: &LookupRequest) -> Result<Option<StrongRef>, HostError> {
    #[cfg(target_arch = "wasm32")]
    {
        let bytes = serde_json::to_vec(request).map_err(PluginError::from)?;
        // SAFETY: `bytes` is live for the duration of the call.
        let packed = unsafe { host_lookup_record(bytes.as_ptr() as i32, bytes.len() as i32) };
        decode_required::<Option<StrongRef>>(packed)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = request;
        Err(HostError::NotWasm)
    }
}

/// Call an export of another library plugin. The callee's error envelope comes
/// back verbatim, so a caller can relay it unchanged. Needs `library:call`.
pub fn call_library(library: &str, function: &str, args: &[Value]) -> Result<Value, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        let args = serde_json::to_vec(args).map_err(PluginError::from)?;
        // SAFETY: every slice is live for the duration of the call.
        let packed = unsafe {
            host_call_library(
                library.as_ptr() as i32,
                library.len() as i32,
                function.as_ptr() as i32,
                function.len() as i32,
                args.as_ptr() as i32,
                args.len() as i32,
            )
        };
        decode_required::<Value>(packed).map_err(PluginError::from)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (library, function, args);
        Err(HostError::NotWasm.into())
    }
}

/// Read another library's API surface, to discover what it exports before
/// calling it. Needs `library:call`.
pub fn library_surface(library: &str) -> Result<ApiSurface, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        // SAFETY: `library` is live for the duration of the call.
        let packed = unsafe { host_get_api_surface(library.as_ptr() as i32, library.len() as i32) };
        decode_required::<ApiSurface>(packed).map_err(PluginError::from)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = library;
        Err(HostError::NotWasm.into())
    }
}

/// Run a read-only SQL query. Placeholders are backend-native: `?` on SQLite,
/// `$1` on Postgres. Needs `database:read` or `database:write`.
pub fn db_query(sql: &str, params: &[Value]) -> Result<Vec<Map<String, Value>>, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        let params = serde_json::to_vec(params).map_err(PluginError::from)?;
        // SAFETY: both slices are live for the duration of the call.
        let packed = unsafe {
            host_db_query(
                sql.as_ptr() as i32,
                sql.len() as i32,
                params.as_ptr() as i32,
                params.len() as i32,
            )
        };
        decode_required::<Vec<Map<String, Value>>>(packed).map_err(PluginError::from)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (sql, params);
        Err(HostError::NotWasm.into())
    }
}

/// Run a statement that modifies data, returning the number of rows affected.
/// Needs `database:write`.
pub fn db_execute(sql: &str, params: &[Value]) -> Result<u64, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        #[derive(Deserialize)]
        struct RowsAffected {
            rows_affected: u64,
        }

        let params = serde_json::to_vec(params).map_err(PluginError::from)?;
        // SAFETY: both slices are live for the duration of the call.
        let packed = unsafe {
            host_db_execute(
                sql.as_ptr() as i32,
                sql.len() as i32,
                params.as_ptr() as i32,
                params.len() as i32,
            )
        };
        decode_required::<RowsAffected>(packed)
            .map(|rows| rows.rows_affected)
            .map_err(PluginError::from)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (sql, params);
        Err(HostError::NotWasm.into())
    }
}

/// A packed return where the host uses 0 for "no such value".
#[cfg(any(target_arch = "wasm32", test))]
fn decode_optional<T: DeserializeOwned>(packed: i64) -> Result<Option<T>, HostError> {
    match decode(packed) {
        Some(result) => result.map(Some),
        None => Ok(None),
    }
}

/// A packed return where 0 means the host could not answer at all.
#[cfg(any(target_arch = "wasm32", test))]
fn decode_required<T: DeserializeOwned>(packed: i64) -> Result<T, HostError> {
    decode(packed).unwrap_or(Err(HostError::NoResponse))
}

#[cfg(any(target_arch = "wasm32", test))]
fn decode<T: DeserializeOwned>(packed: i64) -> Option<Result<T, HostError>> {
    let response: Response<T> = read_packed(packed)?;
    Some(response.into_result().map_err(HostError::Plugin))
}

/// The `i32`-returning imports: 0 is success, anything else a failure the host
/// could not describe.
#[cfg(any(target_arch = "wasm32", test))]
fn decode_status(code: i32, import: &str) -> Result<(), HostError> {
    if code == 0 {
        return Ok(());
    }
    Err(HostError::Plugin(PluginError::host(alloc::format!(
        "{import} failed (capability denied or store error)"
    ))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_header_lookup_ignores_case() {
        let response = HttpResponse {
            status: 200,
            headers: alloc::vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                ("X-Rate-Limit".to_string(), "60".to_string()),
            ],
            body: String::new(),
        };
        assert_eq!(response.header("content-type"), Some("application/json"));
        assert_eq!(response.header("CONTENT-TYPE"), Some("application/json"));
        assert_eq!(response.header("x-rate-limit"), Some("60"));
        assert_eq!(response.header("accept"), None);
    }

    #[test]
    fn response_body_accepts_a_string_or_a_byte_array() {
        let text: HttpResponse =
            serde_json::from_str(r#"{"status":200,"headers":[],"body":"hi"}"#).unwrap();
        assert_eq!(text.body, "hi");

        let bytes: HttpResponse =
            serde_json::from_str(r#"{"status":204,"headers":[],"body":[104,105]}"#).unwrap();
        assert_eq!(bytes.body, "hi");

        let missing: HttpResponse = serde_json::from_str(r#"{"status":204,"headers":[]}"#).unwrap();
        assert_eq!(missing.body, "");
    }

    #[test]
    fn every_wrapper_reports_not_wasm_off_target() {
        assert_eq!(get_secret("k"), Err(HostError::NotWasm));
        assert_eq!(kv_get("k"), Err(HostError::NotWasm));
        assert_eq!(kv_set("k", b"v", Some(60)), Err(HostError::NotWasm));
        assert_eq!(kv_delete("k"), Err(HostError::NotWasm));
        assert_eq!(
            http_request(&HttpRequest::new("GET", "https://example.com")),
            Err(HostError::NotWasm)
        );
        assert_eq!(
            lookup_record(&LookupRequest::new("c", "f", "v")),
            Err(HostError::NotWasm)
        );
        log(Level::Warn, "no-op off target");
    }

    #[test]
    fn not_wasm_and_no_response_become_host_errors() {
        assert_eq!(PluginError::from(HostError::NotWasm).code, "HOST_ERROR");
        assert_eq!(PluginError::from(HostError::NoResponse).code, "HOST_ERROR");
        let inner = PluginError::new("HTTP_ERROR", "boom");
        assert_eq!(PluginError::from(HostError::Plugin(inner.clone())), inner);
    }

    #[test]
    fn decoding_distinguishes_absent_from_unanswered() {
        assert_eq!(decode_optional::<String>(0), Ok(None));
        assert_eq!(decode_required::<String>(0), Err(HostError::NoResponse));
    }

    #[test]
    fn status_codes_map_to_a_named_host_error() {
        assert_eq!(decode_status(0, "host_kv_set"), Ok(()));
        let err = decode_status(-1, "host_kv_set").unwrap_err();
        let HostError::Plugin(err) = err else {
            panic!("expected a plugin error, got {err:?}");
        };
        assert_eq!(err.code, "HOST_ERROR");
        assert!(err.message.contains("host_kv_set"), "{err}");
    }
}
