//! Test instances for record providers.
//!
//! With feature `testkit`: [`ProviderApp`], a booted HappyView on the test
//! database with the providers under test, and [`conformance`], the cases a
//! record provider runs to show it behaves as the built-in one does. The
//! test-database plumbing in [`app`] is always built, because HappyView's own
//! tests build their instances with it too.

pub mod app;
#[cfg(feature = "testkit")]
pub mod conformance;

#[cfg(feature = "testkit")]
pub use app::ProviderApp;
