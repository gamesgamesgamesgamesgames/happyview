//! Library plugin contract: the structured API description a library exports
//! and the call envelope the host sends it. Language-agnostic by design —
//! interpreters render `ApiSurface` into their own idiom.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Nested `host_call_library` chains stop here.
pub const MAX_LIBRARY_CALL_DEPTH: u8 = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiSurface {
    pub namespace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub exports: Vec<ApiExport>,
    /// Named types referenced by exports. Opaque here; interpreters read them.
    #[serde(default)]
    pub types: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiExport {
    pub name: String,
    /// `"function"` (default), `"constant"`, or anything a future interpreter agrees on.
    #[serde(default = "default_export_kind")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub params: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns: Option<Value>,
    /// Everything else the library said about the export, preserved for interpreters.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

fn default_export_kind() -> String {
    "function".to_string()
}

impl ApiExport {
    pub fn is_function(&self) -> bool {
        self.kind == "function"
    }
}

/// Who a library call acts as. Threaded from the script runner through every
/// nested `host_call_library`, so a library can never widen it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryCallContext {
    #[serde(default)]
    pub caller_did: Option<String>,
    #[serde(default)]
    pub has_pds_auth: bool,
}

/// Wire shape of the `call` export's input.
#[derive(Serialize)]
pub struct LibraryCallInput<'a> {
    pub function: &'a str,
    pub args: &'a [Value],
    pub context: &'a LibraryCallContext,
}

/// An installed library, resolved for import.
#[derive(Debug, Clone)]
pub struct LibraryEntry {
    pub id: String,
    pub namespace: String,
    pub surface: std::sync::Arc<ApiSurface>,
}
