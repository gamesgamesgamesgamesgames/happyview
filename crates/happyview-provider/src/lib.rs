//! The contract between HappyView and its record sources and stores.

pub mod admin;
pub mod cid;
pub mod db;
pub mod failure;
pub mod host;
pub mod http;
pub mod limits;
pub mod source;
pub mod storage;
pub mod telemetry;
#[cfg(feature = "testkit")]
pub mod testkit;
pub mod types;
pub mod vars;

pub use admin::{AdminCaller, AdminRefusal, AdminUser, PermissionDenied};
pub use host::*;
pub use limits::{FixedLimits, Limit, LimitReached, Limits, LimitsProvider};
pub use source::RecordSource;
pub use storage::*;
pub use types::*;
