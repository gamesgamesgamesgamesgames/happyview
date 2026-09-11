use serde::{Deserialize, Serialize};

/// A required secret with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretDefinition {
    /// Environment variable name (e.g., "PLUGIN_STEAM_API_KEY")
    pub key: String,
    /// Human-friendly name (e.g., "Steam Web API Key")
    pub name: String,
    /// Description of where to get the secret
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Plugin manifest loaded from manifest.json
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
    #[serde(default)]
    pub required_secrets: Vec<SecretDefinition>,
    /// Authentication type: "oauth2", "openid", "api_key"
    #[serde(default = "default_auth_type")]
    pub auth_type: String,
    /// JSON Schema describing user-provided configuration (e.g., API keys)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<serde_json::Value>,
    /// Description of the plugin
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// WASM file name (relative to manifest location)
    #[serde(default = "default_wasm_file")]
    pub wasm_file: String,
    /// Plugin kind; defaults to `auth` when omitted from the manifest.
    #[serde(default)]
    pub plugin_type: PluginType,
    /// The trust contract. The loader checks the module's imports against it.
    #[serde(default)]
    pub capabilities: Vec<crate::plugin::capabilities::PluginCapability>,
    /// Hosts `network:request` may reach. Bare hostnames; a leading `*.` allows subdomains.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// Publisher DID, once plugins are distributed through the registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    /// Library plugins this plugin calls through `host_call_library`.
    #[serde(default)]
    pub dependencies: Vec<PluginDependency>,
    /// Library only: the name scripts import. Defaults to `id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    /// Interpreter only: value stored in `scripts.script_type`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_extension: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monaco_language: Option<String>,
    /// Interpreter only: library ids it can bridge; `["*"]` means any.
    #[serde(default)]
    pub supports_libraries: Vec<String>,
}

fn default_wasm_file() -> String {
    "plugin.wasm".to_string()
}

/// Plugin metadata returned by plugin_info() - kept for backward compatibility
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
    #[serde(default)]
    pub required_secrets: Vec<String>,
    /// Authentication type: "oauth2", "openid", "api_key"
    #[serde(default = "default_auth_type")]
    pub auth_type: String,
    /// JSON Schema describing user-provided configuration (e.g., API keys)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<serde_json::Value>,
}

impl From<PluginManifest> for PluginInfo {
    fn from(manifest: PluginManifest) -> Self {
        PluginInfo {
            id: manifest.id,
            name: manifest.name,
            version: manifest.version,
            api_version: manifest.api_version,
            icon_url: manifest.icon_url,
            // Extract just the keys from SecretDefinition for PluginInfo
            required_secrets: manifest
                .required_secrets
                .into_iter()
                .map(|s| s.key)
                .collect(),
            auth_type: manifest.auth_type,
            config_schema: manifest.config_schema,
        }
    }
}

fn default_auth_type() -> String {
    "oauth2".to_string()
}

/// What a plugin *is*. Decides which exports the host calls. A manifest
/// that omits it is an auth plugin.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginType {
    #[default]
    Auth,
    Interpreter,
    Library,
}

impl PluginType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::Interpreter => "interpreter",
            Self::Library => "library",
        }
    }

    pub fn parse_str(s: &str) -> Option<Self> {
        match s {
            "auth" => Some(Self::Auth),
            "interpreter" => Some(Self::Interpreter),
            "library" => Some(Self::Library),
            _ => None,
        }
    }
}

/// A dependency on another library plugin. `version` is a semver requirement
/// (`>=1.0.0`, `^1`, `*`); validated at load time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDependency {
    pub id: String,
    #[serde(default = "default_version_req")]
    pub version: String,
}

fn default_version_req() -> String {
    "*".to_string()
}

/// OAuth callback parameters passed to handle_callback()
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallbackParams {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, String>,
}

/// Tokens returned by handle_callback() and refresh_tokens()
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub token_type: String,
}

/// Error returned by plugin functions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginError {
    pub code: PluginErrorCode,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginErrorCode {
    UserDenied,
    InvalidToken,
    ServiceUnavailable,
    InvalidResponse,
    Unknown,
}

/// External profile returned by get_profile()
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalProfile {
    pub account_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
}

/// Record returned by sync_account() - lexicon-aware
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncRecord {
    pub collection: String,
    pub record: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dedup_key: Option<String>,
    /// Whether HappyView should add an attestation signature to this record
    #[serde(default)]
    pub sign: bool,
}

/// Strong reference to an AT Protocol record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrongRef {
    pub uri: String,
    pub cid: String,
}

/// Plugin source - file or URL
#[derive(Debug, Clone)]
pub enum PluginSource {
    File { path: std::path::PathBuf },
    Url { url: String, sha256: Option<String> },
}

/// Loaded plugin with runtime state
pub struct LoadedPlugin {
    pub info: PluginInfo,
    pub source: PluginSource,
    pub wasm_bytes: Vec<u8>,
    /// Full manifest if loaded from manifest.json (contains secret metadata)
    pub manifest: Option<PluginManifest>,
}

impl LoadedPlugin {
    /// Derived from the manifest; `PluginInfo` is what the module's own
    /// `plugin_info()` reports and carries none of this.
    pub fn plugin_type(&self) -> PluginType {
        self.manifest
            .as_ref()
            .map(|m| m.plugin_type)
            .unwrap_or_default()
    }

    pub fn dependencies(&self) -> &[PluginDependency] {
        self.manifest
            .as_ref()
            .map(|m| m.dependencies.as_slice())
            .unwrap_or(&[])
    }

    /// Import name for library plugins; `None` for every other type.
    pub fn namespace(&self) -> Option<&str> {
        let m = self.manifest.as_ref()?;
        if m.plugin_type != PluginType::Library {
            return None;
        }
        Some(m.namespace.as_deref().unwrap_or(&m.id))
    }

    /// What the manifest says. `manifest: None` declares nothing; the loader
    /// never produces such a `LoadedPlugin`, only tests do.
    pub fn declared_capabilities(&self) -> &[crate::plugin::capabilities::PluginCapability] {
        self.manifest
            .as_ref()
            .map(|m| m.capabilities.as_slice())
            .unwrap_or(&[])
    }

    pub fn allowed_hosts(&self) -> &[String] {
        self.manifest
            .as_ref()
            .map(|m| m.allowed_hosts.as_slice())
            .unwrap_or(&[])
    }

    /// `api_version` as a number; unparseable or absent reads as 1. The
    /// manifest is the source of truth when present (`info` is derived from it
    /// on the loader path, but tests construct `info` alone).
    pub fn api_version_number(&self) -> u32 {
        self.manifest
            .as_ref()
            .map(|m| m.api_version.as_str())
            .unwrap_or(&self.info.api_version)
            .parse()
            .unwrap_or(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::capabilities::PluginCapability;

    fn loaded(manifest: Option<PluginManifest>) -> LoadedPlugin {
        LoadedPlugin {
            info: PluginInfo {
                id: "x".into(),
                name: "X".into(),
                version: "1.0.0".into(),
                api_version: "1".into(),
                icon_url: None,
                required_secrets: vec![],
                auth_type: "oauth2".into(),
                config_schema: None,
            },
            source: PluginSource::File {
                path: "/tmp/x".into(),
            },
            wasm_bytes: vec![],
            manifest,
        }
    }

    #[test]
    fn manifest_without_plugin_type_is_auth() {
        let m: PluginManifest = serde_json::from_str(
            r#"{"id":"steam","name":"Steam","version":"1.0.0","api_version":"2"}"#,
        )
        .unwrap();
        assert_eq!(m.plugin_type, PluginType::Auth);
        assert!(m.dependencies.is_empty());
        assert!(m.capabilities.is_empty());
        assert_eq!(loaded(Some(m)).plugin_type(), PluginType::Auth);
        assert_eq!(loaded(None).plugin_type(), PluginType::Auth);
    }

    #[test]
    fn library_manifest_parses_v3_fields() {
        let m: PluginManifest = serde_json::from_str(
            r#"{"id":"record","name":"Record","version":"1.2.0","api_version":"2",
                "plugin_type":"library","namespace":"Record","publisher":"did:plc:abc",
                "capabilities":["database:read","library:call","network:request"],
                "allowed_hosts":["api.example.com","*.cdn.example.com"],
                "dependencies":[{"id":"db","version":">=1.0.0"},{"id":"http"}]}"#,
        )
        .unwrap();
        assert_eq!(m.plugin_type, PluginType::Library);
        assert_eq!(m.dependencies.len(), 2);
        assert_eq!(m.dependencies[0].version, ">=1.0.0");
        assert_eq!(m.dependencies[1].version, "*");
        assert_eq!(m.publisher.as_deref(), Some("did:plc:abc"));
        assert_eq!(
            m.capabilities,
            vec![
                PluginCapability::DatabaseRead,
                PluginCapability::LibraryCall,
                PluginCapability::NetworkRequest
            ]
        );
        let p = loaded(Some(m));
        assert_eq!(p.namespace(), Some("Record"));
        assert_eq!(p.dependencies().len(), 2);
        assert_eq!(p.allowed_hosts(), &["api.example.com", "*.cdn.example.com"]);
        assert_eq!(p.api_version_number(), 2);
    }

    #[test]
    fn unknown_capability_is_a_parse_error() {
        let err = serde_json::from_str::<PluginManifest>(
            r#"{"id":"x","name":"X","version":"1.0.0","api_version":"2","plugin_type":"library",
                "capabilities":["teleport"]}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("teleport"), "{err}");
    }

    #[test]
    fn library_namespace_defaults_to_id() {
        let m: PluginManifest = serde_json::from_str(
            r#"{"id":"http","name":"HTTP","version":"1.0.0","api_version":"2","plugin_type":"library"}"#,
        )
        .unwrap();
        assert_eq!(loaded(Some(m)).namespace(), Some("http"));
    }

    #[test]
    fn auth_plugins_have_no_namespace_and_no_manifest_means_version_1() {
        let p = loaded(None);
        assert_eq!(p.namespace(), None);
        assert_eq!(p.api_version_number(), 1);
        assert!(p.declared_capabilities().is_empty());
    }

    #[test]
    fn plugin_type_round_trips_through_str() {
        for t in [
            PluginType::Auth,
            PluginType::Interpreter,
            PluginType::Library,
        ] {
            assert_eq!(PluginType::parse_str(t.as_str()), Some(t));
        }
        assert_eq!(PluginType::parse_str("bogus"), None);
    }
}
