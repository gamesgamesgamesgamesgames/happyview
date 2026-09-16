mod atproto;
mod bindings;
mod caller;
mod db;
mod http;
mod jobs;
mod kv;
mod linked_repos;
mod logging;
mod lookup;
mod records;
mod secrets;
// `pub(crate)`, not private like `jobs`/`linked_repos`: the Lua `atproto.spaces`
// global (`src/lua/spaces_api.rs`, `src/lua/atproto_api.rs`) calls into this
// module too, so it needs to be reachable from outside `plugin::host`.
pub(crate) mod spaces;

pub use atproto::*;
pub use bindings::{PluginState, register_host_functions};
pub use caller::*;
pub use db::*;
pub use http::*;
pub use kv::*;
pub use logging::*;
pub use lookup::*;
pub use records::*;
pub use secrets::*;

// `jobs`, `linked_repos` and `spaces` are deliberately not glob-exported:
// each defines functions sharing a name with another module's (`create`,
// `create_record`, `put_record`, `delete_record`, `update`, `delete`,
// `upload_blob`...) for a different operation. `bindings.rs` reaches them by
// their full module path.

use std::collections::HashMap;
use std::sync::Arc;

/// Context passed to all host function calls
pub struct HostContext {
    pub plugin_id: String,
    pub scope: String, // user DID or OAuth state
    pub secrets: HashMap<String, String>,
    pub config: serde_json::Value,
    pub db: sqlx::AnyPool,
    pub db_backend: crate::db::DatabaseBackend,
    pub http_client: reqwest::Client,
    pub lexicons: Arc<crate::lexicon::LexiconRegistry>,
}

/// Resource usage tracking for limits
#[derive(Default)]
pub struct ResourceUsage {
    pub http_requests: u32,
    pub http_bytes_transferred: u64,
    pub kv_bytes_used: u64,
}

/// Resource limits from spec
pub const MAX_HTTP_REQUESTS: u32 = 100;
pub const MAX_HTTP_RESPONSE_SIZE: u64 = 100 * 1024 * 1024; // 100 MB
pub const MAX_HTTP_TOTAL_TRANSFER: u64 = 500 * 1024 * 1024; // 500 MB
pub const MAX_HTTP_CONCURRENT: usize = 5;
pub const MAX_KV_SIZE_PER_USER: u64 = 1024 * 1024; // 1 MB
