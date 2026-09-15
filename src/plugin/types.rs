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

/// What a plugin's `plugin_info()` export returns, and the SDK's definition of
/// it — the host never redefines a type that crosses the WASM boundary.
pub use happyview_plugin_sdk::wire::PluginInfo;

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

/// The tokens `handle_callback()` and `refresh_tokens()` return, and the
/// profile `get_profile()` returns. Read a token's expiry through
/// [`TokenSetExt::resolved_expires_at`], since a plugin may give either an
/// absolute instant or a duration.
pub use happyview_plugin_sdk::wire::{ExternalProfile, StrongRef, TokenSet};

/// The host-side half of [`TokenSet`]: turning what a plugin reported into an
/// absolute instant needs a clock, which a guest does not have.
pub trait TokenSetExt {
    /// `expires_at` when the plugin gave one, otherwise `now + expires_in` when
    /// it gave a duration instead, otherwise `None`. An `expires_at` that is
    /// not RFC 3339 is an error.
    fn resolved_expires_at(
        &self,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>, chrono::ParseError>;
}

impl TokenSetExt for TokenSet {
    fn resolved_expires_at(
        &self,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>, chrono::ParseError> {
        if let Some(at) = &self.expires_at {
            return Ok(Some(
                chrono::DateTime::parse_from_rfc3339(at)?.with_timezone(&chrono::Utc),
            ));
        }
        Ok(self
            .expires_in
            .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs as i64)))
    }
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

    fn token_set(expires_at: Option<&str>, expires_in: Option<u64>) -> TokenSet {
        TokenSet {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: expires_at.map(str::to_string),
            expires_in,
            token_type: "Bearer".into(),
        }
    }

    #[test]
    fn resolved_expires_at_prefers_an_explicit_timestamp_over_a_duration() {
        let ts = token_set(Some("2026-01-01T00:00:00Z"), Some(999));
        let at = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(ts.resolved_expires_at(), Ok(Some(at)));
    }

    #[test]
    fn resolved_expires_at_reads_an_offset_timestamp_as_the_instant_it_names() {
        let ts = token_set(Some("2025-12-31T19:00:00-05:00"), None);
        let at = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(ts.resolved_expires_at(), Ok(Some(at)));
    }

    /// An unparseable `expires_at` is refused here, not at deserialisation:
    /// the wire type keeps it as a string so the SDK needs no clock or parser.
    #[test]
    fn resolved_expires_at_refuses_a_timestamp_it_cannot_parse() {
        assert!(
            token_set(Some("next tuesday"), Some(3600))
                .resolved_expires_at()
                .is_err()
        );
    }

    #[test]
    fn resolved_expires_at_derives_from_a_duration_when_no_timestamp_is_given() {
        let ts = token_set(None, Some(3600));
        let resolved = ts
            .resolved_expires_at()
            .expect("a duration should parse")
            .expect("expires_in should derive an expiry");
        let now = chrono::Utc::now();
        assert!(resolved > now, "derived expiry must be in the future");
        assert!(
            resolved <= now + chrono::Duration::seconds(3600),
            "derived expiry must not exceed now + expires_in"
        );
    }

    #[test]
    fn resolved_expires_at_is_none_when_the_plugin_gave_neither() {
        let ts = token_set(None, None);
        assert_eq!(ts.resolved_expires_at(), Ok(None));
    }

    #[test]
    fn token_set_deserializes_when_expires_in_is_absent() {
        let ts: TokenSet =
            serde_json::from_str(r#"{"access_token":"a","token_type":"Bearer"}"#).unwrap();
        assert_eq!(ts.expires_in, None);
    }
}
