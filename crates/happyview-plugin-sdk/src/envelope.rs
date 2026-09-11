//! The JSON envelope every plugin export and every host call speaks.
//!
//! A plugin returns either `{"ok": <value>}` or
//! `{"error": {"code", "message", "retryable"}}`. The host parses exactly this
//! shape (`PluginResponse` in `src/plugin/memory.rs`), so the two must not drift.

use alloc::format;
use alloc::string::String;

use serde::{Deserialize, Serialize};

/// The wire envelope, in both directions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Response<T> {
    Ok { ok: T },
    Err { error: PluginError },
}

impl<T> Response<T> {
    /// Collapse the envelope into a `Result`.
    pub fn into_result(self) -> Result<T, PluginError> {
        match self {
            Response::Ok { ok } => Ok(ok),
            Response::Err { error } => Err(error),
        }
    }
}

/// A structured plugin error. `code` is a free-form string; the host relays it
/// verbatim to whoever called the plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
}

impl PluginError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
        }
    }

    /// Mark this error as worth retrying. The host surfaces the flag; it does
    /// not retry on the plugin's behalf.
    pub fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }

    /// Input the plugin could not make sense of. Code `BAD_INPUT`.
    pub fn bad_input(message: impl Into<String>) -> Self {
        Self::new("BAD_INPUT", message)
    }

    /// The caller named an export this plugin does not have. Code `UNKNOWN_FUNCTION`.
    pub fn unknown_function(name: &str) -> Self {
        Self::new("UNKNOWN_FUNCTION", format!("no such function: {name}"))
    }

    /// A host call failed in a way that is not the caller's fault. Code `HOST_ERROR`.
    pub fn host(message: impl Into<String>) -> Self {
        Self::new("HOST_ERROR", message)
    }
}

impl From<serde_json::Error> for PluginError {
    fn from(err: serde_json::Error) -> Self {
        Self::bad_input(format!("{err}"))
    }
}

impl core::fmt::Display for PluginError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
