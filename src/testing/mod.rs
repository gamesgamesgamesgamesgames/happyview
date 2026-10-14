//! `happyview test`: everything that exists only while a project's test files
//! run. Production code reaches into this module only where `AppState.test`
//! is set, and the serving binary never sets it.

pub mod env;
pub mod fake_pds;
pub mod harness;
pub mod identity;
pub mod mocks;
pub mod report;
pub mod runner;
pub mod seed;
pub mod triggers;

pub use fake_pds::{FakePds, FakePdsError};
pub use harness::{ForgedCaller, TestHarness, TestHostError};

/// The host every in-process request names, registered as the instance's
/// primary domain so `resolve_domain` lets it through.
pub const TEST_HOST: &str = "happyview.test";

/// Sent on every in-process XRPC request. A forged caller has no client key
/// of its own, and the XRPC handlers refuse an identified caller without one.
pub const TEST_CLIENT_KEY: &str = "happyview-test";

/// The author a record trigger uses, and the creator a job uses, when the
/// test names nobody.
pub const DEFAULT_DID: &str = "did:plc:test";
