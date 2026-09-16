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

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

#[cfg(any(target_arch = "wasm32", test))]
use serde::de::DeserializeOwned;
#[cfg(target_arch = "wasm32")]
use serde::Deserialize;
use serde_json::{Map, Value};

#[cfg(any(target_arch = "wasm32", test))]
use crate::abi::read_packed;
#[cfg(any(target_arch = "wasm32", test))]
use crate::wire::Response;
use crate::wire::{
    ApiSurface, AtprotoBlobDownload, AttestSign, AttestVerify, BacklinksQuery, BlobData,
    CallerBlobUpload, CallerRecordCreate, CallerRecordDelete, CallerRecordPut, CallerXrpcProcedure,
    CallerXrpcQuery, IndexDelete, IndexPut, JobCreate, Label, LabelsGet, LinkedRepoBlobUpload,
    LinkedRepoCall, LinkedRepoInfo, LinkedRepoRecordCreate, LinkedRepoRecordDelete,
    LinkedRepoRecordPut, PluginError, RecordRef, RecordsCount, RecordsPage, RecordsQuery,
    RecordsSearch, SpaceDelete, SpaceInfo, SpaceInviteCreate, SpaceInviteInfo, SpaceMemberAdd,
    SpaceMemberInfo, SpaceMemberRemove, SpaceRecordDelete, SpaceRecordPut, SpaceRecordWrite,
    SpaceRecordsPage, SpaceUpdate, SpacesAcceptInvite, SpacesAccess, SpacesCreate, SpacesInfo,
    SpacesMembers, SpacesQuery, StrongRef, TableQuery,
};
#[cfg(target_arch = "wasm32")]
use crate::wire::{AtprotoResolveService, LexiconGet};

/// The wire types these wrappers send and receive. Defined in [`crate::wire`],
/// which the host imports too; re-exported here as the import path plugins use.
pub use crate::wire::{HttpRequest, HttpResponse, Level, LookupRequest, ParseLevelError};

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "env")]
extern "C" {
    fn host_log(level_ptr: i32, level_len: i32, msg_ptr: i32, msg_len: i32);
    fn host_get_secret(name_ptr: i32, name_len: i32) -> i64;
    fn host_http_request(req_ptr: i32, req_len: i32) -> i64;
    fn host_allowed_hosts(req_ptr: i32, req_len: i32) -> i64;
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
    fn host_records_query(req_ptr: i32, req_len: i32) -> i64;
    fn host_records_count(req_ptr: i32, req_len: i32) -> i64;
    fn host_records_get(req_ptr: i32, req_len: i32) -> i64;
    fn host_records_search(req_ptr: i32, req_len: i32) -> i64;
    fn host_table_query(req_ptr: i32, req_len: i32) -> i64;
    fn host_backlinks_query(req_ptr: i32, req_len: i32) -> i64;
    fn host_caller_create_record(req_ptr: i32, req_len: i32) -> i64;
    fn host_caller_put_record(req_ptr: i32, req_len: i32) -> i64;
    fn host_caller_delete_record(req_ptr: i32, req_len: i32) -> i64;
    fn host_caller_upload_blob(req_ptr: i32, req_len: i32) -> i64;
    fn host_caller_xrpc_query(req_ptr: i32, req_len: i32) -> i64;
    fn host_caller_xrpc_procedure(req_ptr: i32, req_len: i32) -> i64;
    fn host_records_index_put(req_ptr: i32, req_len: i32) -> i64;
    fn host_records_index_delete(req_ptr: i32, req_len: i32) -> i64;
    fn host_lexicon_get(req_ptr: i32, req_len: i32) -> i64;
    fn host_atproto_resolve_service(req_ptr: i32, req_len: i32) -> i64;
    fn host_atproto_blob_download(req_ptr: i32, req_len: i32) -> i64;
    fn host_labels_get(req_ptr: i32, req_len: i32) -> i64;
    fn host_attest_sign(req_ptr: i32, req_len: i32) -> i64;
    fn host_attest_verify(req_ptr: i32, req_len: i32) -> i64;
    fn host_linked_repos_list(req_ptr: i32, req_len: i32) -> i64;
    fn host_linked_repo_create_record(req_ptr: i32, req_len: i32) -> i64;
    fn host_linked_repo_put_record(req_ptr: i32, req_len: i32) -> i64;
    fn host_linked_repo_delete_record(req_ptr: i32, req_len: i32) -> i64;
    fn host_linked_repo_upload_blob(req_ptr: i32, req_len: i32) -> i64;
    fn host_linked_repo_call(req_ptr: i32, req_len: i32) -> i64;
    fn host_jobs_create(req_ptr: i32, req_len: i32) -> i64;
    fn host_spaces_info(req_ptr: i32, req_len: i32) -> i64;
    fn host_spaces_query(req_ptr: i32, req_len: i32) -> i64;
    fn host_spaces_members(req_ptr: i32, req_len: i32) -> i64;
    fn host_spaces_access(req_ptr: i32, req_len: i32) -> i64;
    fn host_spaces_create(req_ptr: i32, req_len: i32) -> i64;
    fn host_spaces_accept_invite(req_ptr: i32, req_len: i32) -> i64;
    fn host_spaces_write_record(req_ptr: i32, req_len: i32) -> i64;
    fn host_spaces_put_record(req_ptr: i32, req_len: i32) -> i64;
    fn host_spaces_delete_record(req_ptr: i32, req_len: i32) -> i64;
    fn host_spaces_add_member(req_ptr: i32, req_len: i32) -> i64;
    fn host_spaces_set_member(req_ptr: i32, req_len: i32) -> i64;
    fn host_spaces_remove_member(req_ptr: i32, req_len: i32) -> i64;
    fn host_spaces_update(req_ptr: i32, req_len: i32) -> i64;
    fn host_spaces_delete(req_ptr: i32, req_len: i32) -> i64;
    fn host_spaces_create_invite(req_ptr: i32, req_len: i32) -> i64;
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

/// The plugin's effective allowed-hosts list: the operator-configured list
/// under `network:request:defined`, the manifest's `allowed_hosts` under
/// `network:request`, or empty under `network:request:unrestricted` (which
/// needs no list) or with no network capability declared at all. Needs no
/// capability.
pub fn allowed_hosts() -> Result<Vec<String>, HostError> {
    #[cfg(target_arch = "wasm32")]
    {
        let bytes = serde_json::to_vec(&serde_json::json!({})).map_err(PluginError::from)?;
        // SAFETY: `bytes` is live for the duration of the call.
        let packed = unsafe { host_allowed_hosts(bytes.as_ptr() as i32, bytes.len() as i32) };
        decode_required(packed)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
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

/// Send a spec to a record/table query import and decode its typed result.
/// Every import here shares one envelope shape, so this is the one place that
/// serializes the request and decodes the response.
#[cfg(target_arch = "wasm32")]
fn call_spec<S: serde::Serialize, R: serde::de::DeserializeOwned>(
    import: unsafe extern "C" fn(i32, i32) -> i64,
    spec: &S,
) -> Result<R, PluginError> {
    let bytes = serde_json::to_vec(spec).map_err(PluginError::from)?;
    // SAFETY: `bytes` is live for the duration of the call.
    let packed = unsafe { import(bytes.as_ptr() as i32, bytes.len() as i32) };
    decode_required::<R>(packed).map_err(PluginError::from)
}

/// A write whose only answer is `{"ok": null}`. Whatever value does come
/// back is discarded rather than failing on a shape a future host might vary.
#[cfg(target_arch = "wasm32")]
fn call_void<S: serde::Serialize>(
    import: unsafe extern "C" fn(i32, i32) -> i64,
    spec: &S,
) -> Result<(), PluginError> {
    call_spec::<_, Option<Value>>(import, spec).map(|_| ())
}

/// Page through indexed records matching a filter. Needs `records:read`.
pub fn records_query(spec: &RecordsQuery) -> Result<RecordsPage, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_records_query, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Count indexed records matching a filter. Needs `records:read`.
pub fn records_count(spec: &RecordsCount) -> Result<i64, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_records_count, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Fetch one indexed record by its `at://` URI. `Ok(None)` means it is not
/// indexed. Needs `records:read`.
pub fn records_get(uri: &str) -> Result<Option<Value>, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_records_get, &serde_json::json!({"uri": uri}))
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = uri;
        Err(HostError::NotWasm.into())
    }
}

/// Substring-search one JSON field across a collection's records. Needs
/// `records:read`.
pub fn records_search(spec: &RecordsSearch) -> Result<Vec<Value>, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_records_search, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Run a structured query against an arbitrary table, guarded the same way as
/// `db_query`. Needs `database:read` (or `database:write`).
pub fn table_query(spec: &TableQuery) -> Result<Value, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_table_query, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Page through records that reference a given URI. Needs `records:read`.
pub fn backlinks_query(spec: &BacklinksQuery) -> Result<RecordsPage, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_backlinks_query, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Create a record on the calling script's own repo, as them. Needs
/// `caller:write`.
pub fn caller_create_record(spec: &CallerRecordCreate) -> Result<RecordRef, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_caller_create_record, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Put (create-or-update) a record on the calling script's own repo, as them.
/// Needs `caller:write`.
pub fn caller_put_record(spec: &CallerRecordPut) -> Result<RecordRef, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_caller_put_record, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Delete a record from the calling script's own repo, as them. Needs
/// `caller:write`.
pub fn caller_delete_record(spec: &CallerRecordDelete) -> Result<(), PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_void(host_caller_delete_record, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Upload a blob to the calling script's own repo, as them. Returns the PDS's
/// blob ref as-is. Needs `caller:write`.
pub fn caller_upload_blob(spec: &CallerBlobUpload) -> Result<Value, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_caller_upload_blob, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Send an XRPC query as the calling script's user. Needs `caller:read`.
pub fn caller_xrpc_query(spec: &CallerXrpcQuery) -> Result<Value, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_caller_xrpc_query, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Send an XRPC procedure as the calling script's user. Needs `caller:call`.
pub fn caller_xrpc_procedure(spec: &CallerXrpcProcedure) -> Result<Value, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_caller_xrpc_procedure, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Write a record straight into the local index, bypassing the PDS. `cid` on
/// the result is whatever the index computed. Needs `records:write`.
pub fn records_index_put(spec: &IndexPut) -> Result<RecordRef, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_records_index_put, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Remove a record from the local index, bypassing the PDS. Returns whether a
/// row was actually removed. Needs `records:write`.
pub fn records_index_delete(spec: &IndexDelete) -> Result<bool, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_records_index_delete, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Look up an uploaded lexicon's raw JSON by NSID. `Ok(None)` means no lexicon
/// is registered for it. Needs no capability.
pub fn lexicon_get(nsid: &str) -> Result<Option<Value>, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_lexicon_get, &LexiconGet { nsid: nsid.into() })
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = nsid;
        Err(HostError::NotWasm.into())
    }
}

/// Resolve the AT Protocol service a DID's document advertises (its PDS,
/// typically). `Ok(None)` means the DID does not resolve or names no such
/// service. Needs `atproto:read`.
pub fn atproto_resolve_service(did: &str) -> Result<Option<String>, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(
            host_atproto_resolve_service,
            &AtprotoResolveService { did: did.into() },
        )
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = did;
        Err(HostError::NotWasm.into())
    }
}

/// Download a blob from a repo. Needs `atproto:read`.
pub fn atproto_blob_download(spec: &AtprotoBlobDownload) -> Result<BlobData, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_atproto_blob_download, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Look up labels applied to a set of URIs, keyed by URI. Every requested URI
/// is present in the result, possibly with an empty list. Needs
/// `atproto:read`.
pub fn labels_get(spec: &LabelsGet) -> Result<BTreeMap<String, Vec<Label>>, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_labels_get, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Sign a record, returning the inline signature object to attach to it.
/// Needs `attest:sign`.
pub fn attest_sign(spec: &AttestSign) -> Result<Value, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_attest_sign, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Verify a record's attestation signature. Needs `atproto:read`.
pub fn attest_verify(spec: &AttestVerify) -> Result<bool, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_attest_verify, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// List the linked-repo grants the plugin may act through. Needs
/// `linked_repos:use`.
pub fn linked_repos_list() -> Result<Vec<LinkedRepoInfo>, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_linked_repos_list, &serde_json::json!({}))
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        Err(HostError::NotWasm.into())
    }
}

/// Create a record on a linked repo, as the DID its grant names. Needs
/// `linked_repos:use`.
pub fn linked_repo_create_record(spec: &LinkedRepoRecordCreate) -> Result<RecordRef, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_linked_repo_create_record, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Put (create-or-update) a record on a linked repo. Needs `linked_repos:use`.
pub fn linked_repo_put_record(spec: &LinkedRepoRecordPut) -> Result<RecordRef, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_linked_repo_put_record, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Delete a record from a linked repo. Needs `linked_repos:use`.
pub fn linked_repo_delete_record(spec: &LinkedRepoRecordDelete) -> Result<(), PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_void(host_linked_repo_delete_record, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Upload a blob to a linked repo. Returns the PDS's blob ref as-is. Needs
/// `linked_repos:use`.
pub fn linked_repo_upload_blob(spec: &LinkedRepoBlobUpload) -> Result<Value, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_linked_repo_upload_blob, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Send an XRPC call against a linked repo, as the DID its grant names. Needs
/// `linked_repos:use`.
pub fn linked_repo_call(spec: &LinkedRepoCall) -> Result<Value, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_linked_repo_call, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Enqueue a background job, returning its id. Needs `jobs:create`.
pub fn jobs_create(spec: &JobCreate) -> Result<String, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_jobs_create, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Look up a space's fields by URI. `Ok(None)` means no such space. Needs
/// `spaces:read`.
pub fn spaces_info(spec: &SpacesInfo) -> Result<Option<SpaceInfo>, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_spaces_info, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Page through a space's records. Needs `spaces:read`.
pub fn spaces_query(spec: &SpacesQuery) -> Result<SpaceRecordsPage, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_spaces_query, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// List a space's members. Needs `spaces:read`.
pub fn spaces_members(spec: &SpacesMembers) -> Result<Vec<SpaceMemberInfo>, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_spaces_members, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// A DID's access level on a space. `Ok(None)` means not a member, or no such
/// space. Needs `spaces:read`.
pub fn spaces_access(spec: &SpacesAccess) -> Result<Option<String>, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_spaces_access, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Create a space. Needs `spaces:write`.
pub fn spaces_create(spec: &SpacesCreate) -> Result<SpaceInfo, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_spaces_create, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Redeem an invite token, joining the space it names. Needs `spaces:write`.
pub fn spaces_accept_invite(spec: &SpacesAcceptInvite) -> Result<SpaceInfo, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_spaces_accept_invite, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Create a record in a space, as the caller. Needs `spaces:write`.
pub fn spaces_write_record(spec: &SpaceRecordWrite) -> Result<RecordRef, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_spaces_write_record, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Put (create-or-update) a record in a space, as the caller. Needs
/// `spaces:write`.
pub fn spaces_put_record(spec: &SpaceRecordPut) -> Result<RecordRef, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_spaces_put_record, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Delete a record from a space, as the caller. Needs `spaces:write`.
pub fn spaces_delete_record(spec: &SpaceRecordDelete) -> Result<(), PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_void(host_spaces_delete_record, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Add a space member. Refuses if the DID is already a member. Needs
/// `spaces:write`.
pub fn spaces_add_member(spec: &SpaceMemberAdd) -> Result<SpaceMemberInfo, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_spaces_add_member, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Add or update a space member, preserving an existing member's
/// `read_self`. Needs `spaces:write`.
pub fn spaces_set_member(spec: &SpaceMemberAdd) -> Result<SpaceMemberInfo, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_spaces_set_member, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Remove a space member. Needs `spaces:write`.
pub fn spaces_remove_member(spec: &SpaceMemberRemove) -> Result<(), PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_void(host_spaces_remove_member, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Update a space's fields. Needs `spaces:write`.
pub fn spaces_update(spec: &SpaceUpdate) -> Result<SpaceInfo, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_spaces_update, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Delete a space. Needs `spaces:write`.
pub fn spaces_delete(spec: &SpaceDelete) -> Result<(), PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_void(host_spaces_delete, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
    }
}

/// Mint an invite for a space. Needs `spaces:write`.
pub fn spaces_create_invite(spec: &SpaceInviteCreate) -> Result<SpaceInviteInfo, PluginError> {
    #[cfg(target_arch = "wasm32")]
    {
        call_spec(host_spaces_create_invite, spec)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = spec;
        Err(HostError::NotWasm.into())
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
    fn every_wrapper_reports_not_wasm_off_target() {
        assert_eq!(get_secret("k"), Err(HostError::NotWasm));
        assert_eq!(kv_get("k"), Err(HostError::NotWasm));
        assert_eq!(kv_set("k", b"v", Some(60)), Err(HostError::NotWasm));
        assert_eq!(kv_delete("k"), Err(HostError::NotWasm));
        assert_eq!(
            http_request(&HttpRequest::new("GET", "https://example.com")),
            Err(HostError::NotWasm)
        );
        assert_eq!(allowed_hosts(), Err(HostError::NotWasm));
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
