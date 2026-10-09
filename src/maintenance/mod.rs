//! Database maintenance: disk accounting, the periodic SQLite checkpoint and
//! optimize, the one-time SQLite vacuum that reclaims pages stranded before incremental auto-vacuum existed, and
//! the startup audit for stored config the NSID consolidation tightened
//! rules around.

pub mod disk;
pub mod lexicon_ids;
pub mod nsid_audit;
pub mod sqlite;
pub mod vacuum;
