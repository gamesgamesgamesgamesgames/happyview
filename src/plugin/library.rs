//! Library plugin contract: the structured API description a library exports
//! and the call envelope the host sends it. Language-agnostic by design —
//! interpreters render `ApiSurface` into their own idiom.
//!
//! The wire types are the SDK's, under the names the host uses for them.

/// Nested `host_call_library` chains stop here.
pub const MAX_LIBRARY_CALL_DEPTH: u8 = 8;

pub use happyview_plugin_sdk::wire::{
    ApiExport, ApiSurface, CallContext as LibraryCallContext, CallInput as LibraryCallInput,
};

/// An installed library, resolved for import.
#[derive(Debug, Clone)]
pub struct LibraryEntry {
    pub id: String,
    pub namespace: String,
    pub surface: std::sync::Arc<ApiSurface>,
}
