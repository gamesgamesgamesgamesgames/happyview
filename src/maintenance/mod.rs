//! One-off database maintenance: disk accounting and the one-time SQLite
//! vacuum that reclaims pages stranded before incremental auto-vacuum existed.

pub mod disk;
pub mod vacuum;
