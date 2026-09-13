mod bindings;
mod db;
mod http;
mod kv;
mod logging;
mod lookup;
mod records;
mod secrets;

pub use bindings::{PluginState, register_host_functions};
pub use db::*;
pub use http::*;
pub use kv::*;
pub use logging::*;
pub use lookup::*;
pub use records::*;
pub use secrets::*;

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
