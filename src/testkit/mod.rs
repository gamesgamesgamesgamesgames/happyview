//! Test instances for record providers.
//!
//! With feature `testkit`: [`ProviderApp`], a booted HappyView on the test
//! database with the providers under test, and [`conformance`], the cases a
//! record provider runs to show it behaves as the built-in one does. The
//! test-database plumbing beneath them is always built, because HappyView's
//! own tests build their instances with it too; it is test-only, destructive
//! and not part of the stable API, so it is hidden from these docs.

/// Test-only and destructive: it empties tables. Not part of the stable
/// API; public so HappyView's own tests can share it.
#[doc(hidden)]
pub mod app;
#[cfg(feature = "testkit")]
pub mod conformance;

#[cfg(feature = "testkit")]
pub use app::{ADMIN_DID, LUA_PLUGIN_DIR, ProviderApp};
