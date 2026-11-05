use happyview_plugin_sdk::PluginError;

/// Why a handler did not answer. The message is what the caller is shown,
/// as an interpreter's script error message is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    message: String,
}

pub type Result<T, E = Error> = core::result::Result<T, E>;

impl Error {
    pub fn msg(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// A library's own error message, unchanged: an `AUTH_ERROR:` prefix has to
/// reach the host as the library wrote it.
impl From<PluginError> for Error {
    fn from(e: PluginError) -> Self {
        Self::msg(e.message)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::msg(e.to_string())
    }
}
