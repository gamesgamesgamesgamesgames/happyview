//! One-off database maintenance: disk accounting, the one-time SQLite vacuum
//! that reclaims pages stranded before incremental auto-vacuum existed, and
//! the startup audit for stored config the NSID consolidation tightened
//! rules around.

pub mod disk;
pub mod lexicon_ids;
pub mod nsid_audit;
pub mod vacuum;
