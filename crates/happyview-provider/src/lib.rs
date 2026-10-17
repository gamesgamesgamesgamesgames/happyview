//! The contract between HappyView and its record sources and stores.

pub mod admin;
pub mod cid;
pub mod db;
pub mod host;
pub mod http;
pub mod source;
pub mod storage;
pub mod types;
pub mod vars;

pub use admin::AdminCaller;
pub use host::*;
pub use source::RecordSource;
pub use storage::*;
pub use types::*;
