//! The pure parts of the Jetstream consumer: settings, wire events, the
//! applied watermark and zstd frame handling.

pub mod compression;
pub mod config;
pub mod event;
pub mod watermark;
