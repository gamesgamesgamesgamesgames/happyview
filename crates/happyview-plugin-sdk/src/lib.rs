//! Guest-side SDK for HappyView WASM plugins.
//!
//! A plugin describes itself and its exports; the SDK owns everything between
//! that and the host — the allocator, the packed-`i64` calling convention, the
//! JSON envelope, and the `env` host imports. Nothing in a plugin crate should
//! need `extern "C"`, a raw pointer, or a `#[global_allocator]`.
//!
//! ```ignore
//! use happyview_plugin_sdk::{
//!     library_plugin, ApiSurface, CallContext, PluginError, PluginInfo, Value,
//! };
//!
//! library_plugin! {
//!     info: PluginInfo::new("echo", "Echo", "1.0.0"),
//!     surface: surface,
//!     call: dispatch,
//! }
//!
//! fn surface() -> ApiSurface { ApiSurface::new("echo") }
//!
//! fn dispatch(function: &str, args: &[Value], _ctx: &CallContext)
//!     -> Result<Value, PluginError>
//! {
//!     match function {
//!         "echo" => Ok(args.first().cloned().unwrap_or(Value::Null)),
//!         other => Err(PluginError::unknown_function(other)),
//!     }
//! }
//! ```
//!
//! An auth plugin uses [`auth_plugin!`] instead, which emits the four exports
//! the external-auth flow calls — `get_authorize_url`, `handle_callback`,
//! `refresh_tokens` and `get_profile` — each with its own typed input and
//! output.
//!
//! Native builds compile the whole SDK, so a plugin's own logic is testable
//! with `cargo test`; the host wrappers report [`host::HostError::NotWasm`]
//! there rather than calling anything.

#![cfg_attr(target_arch = "wasm32", no_std)]

extern crate alloc;

pub mod abi;
pub mod envelope;
pub mod host;
mod macros;
pub mod naming;
pub mod tid;
pub mod types;
pub mod wire;

pub use envelope::{PluginError, Response};
pub use types::{
    ApiExport, ApiMethod, ApiSurface, AtprotoBlobDownload, AtprotoResolveService, AttestSign,
    AttestVerify, AuthorizeUrlInput, BacklinksQuery, BlobData, CallContext, CallInput,
    CallbackInput, CallerBlobUpload, CallerRecordCreate, CallerRecordDelete, CallerRecordPut,
    CallerXrpcProcedure, CallerXrpcQuery, Condition, ExternalProfile, Filter, IndexDelete,
    IndexPut, JobCreate, Label, LabelsGet, LexiconGet, LinkedRepoBlobUpload, LinkedRepoCall,
    LinkedRepoInfo, LinkedRepoRecordCreate, LinkedRepoRecordDelete, LinkedRepoRecordPut,
    MethodCall, ObjectCall, Patch, PluginInfo, RecordRef, RecordsCount, RecordsPage, RecordsQuery,
    RecordsSearch, RefreshInput, Sort, SpaceDelete, SpaceInfo, SpaceInviteCreate, SpaceInviteInfo,
    SpaceMemberAdd, SpaceMemberInfo, SpaceMemberRemove, SpaceRecordDelete, SpaceRecordInfo,
    SpaceRecordPut, SpaceRecordWrite, SpaceRecordsPage, SpaceUpdate, SpacesAcceptInvite,
    SpacesAccess, SpacesCreate, SpacesInfo, SpacesMembers, SpacesQuery, Step, StrongRef,
    TableQuery, TokenInput, TokenSet,
};

/// Items the macros name in their expansion. Not a public API: a plugin crate
/// compiles with `no_std`, so the macros cannot assume `String` is in scope
/// where they land.
#[doc(hidden)]
pub mod __private {
    pub use alloc::string::String;
}

pub use serde_json;
/// Re-exported so a plugin needs only this crate as a dependency.
pub use serde_json::{json, Map, Value};
