//! The Jetstream record source: the v2 archive client, the live consumer,
//! the backfill job system and their admin routes.

//!
//! The stable API is what this page lists: [`JetstreamSource`] and its
//! builder, the types the builder takes ([`Settings`], [`ArchiveConfig`])
//! with the types their public fields hold, and the composition types a
//! provider built on this source implements or is handed
//! ([`BackfillExecutor`], [`JobHandle`], [`JobTotals`]). The modules are
//! public only so HappyView's own tests can reach the source's internals;
//! they are hidden from these docs, are not part of the stable API, and may
//! change in any release.

#[doc(hidden)]
pub mod admin;
#[doc(hidden)]
pub mod archive;
#[doc(hidden)]
pub mod backfill;
#[doc(hidden)]
pub mod compose;
#[doc(hidden)]
pub mod consumer;
#[doc(hidden)]
pub mod source;

pub use archive::pacer::{BytePacer, Quota};
pub use archive::{ArchiveConfig, SourceChoice};
pub use compose::{BackfillExecutor, CollectionSummary, JobHandle, JobTotals, SummaryState};
pub use consumer::config::{Endpoint, ProtocolChoice, Settings};
pub use source::{JetstreamSource, JetstreamSourceBuilder};
