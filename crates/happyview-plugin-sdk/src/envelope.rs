//! The JSON envelope every plugin export and every host call speaks.
//!
//! A plugin returns either `{"ok": <value>}` or
//! `{"error": {"code", "message", "retryable"}}`. Both types are defined in
//! [`crate::wire`] and re-exported here, so the host parses the very types the
//! guest builds.

pub use crate::wire::{PluginError, Response};
