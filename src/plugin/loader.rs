use crate::plugin::{LoadedPlugin, PluginInfo, PluginManifest, PluginSource};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Highest plugin API version this binary understands. Version 2 added
/// `plugin_type`, capabilities, dependencies, and the library/interpreter
/// exports.
pub const MAX_API_VERSION: u32 = 2;
/// Lowest plugin API version this binary accepts, for every plugin type.
/// A version 1 manifest declares no capabilities and is refused at load.
const MIN_API_VERSION: u32 = 2;

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("Failed to read plugin file: {0}")]
    ReadFile(#[from] std::io::Error),
    #[error("Failed to download plugin: {0}")]
    Download(#[from] reqwest::Error),
    #[error("SHA256 mismatch: expected {expected}, got {actual}")]
    Sha256Mismatch { expected: String, actual: String },
    #[error("Failed to parse plugin info: {0}")]
    ParseInfo(#[from] serde_json::Error),
    #[error("Plugin API version {0} not supported (max {MAX_API_VERSION})")]
    UnsupportedApiVersion(String),
    #[error(
        "{plugin_type} plugins need an api_version: \"2\" manifest declaring capabilities (got api_version \"{api_version}\"); republish the plugin with a capabilities-declaring manifest"
    )]
    ApiVersionTooOld {
        plugin_type: &'static str,
        api_version: String,
    },
    #[error("Invalid dependency: {0}")]
    InvalidDependency(String),
    #[error("Invalid capabilities: {0}")]
    InvalidCapabilities(String),
    #[error("Missing required secret: {0}")]
    MissingSecret(String),
    #[error(
        "required secret key '{key}' must start with '{expected_prefix}' (the id's derived secret prefix)"
    )]
    InvalidSecretKey {
        key: String,
        expected_prefix: String,
    },
    #[error("WASM validation failed: {0}")]
    WasmValidation(String),
    #[error("Manifest not found at {0}")]
    ManifestNotFound(String),
    #[error("Capability mismatch: {0}")]
    CapabilityMismatch(String),
    #[error("Unknown host import: {0}")]
    UnknownImport(String),
    #[error("namespace '{0}' is reserved for built-in modules")]
    ReservedNamespace(String),
}

/// Preview result with manifest and derived WASM URL
#[derive(Debug, Clone, serde::Serialize)]
pub struct PluginPreview {
    pub manifest: PluginManifest,
    pub manifest_url: String,
    pub wasm_url: String,
}

/// Fetch plugin manifest from a URL (or derive manifest URL from WASM URL)
pub async fn fetch_manifest(
    client: &reqwest::Client,
    url: &str,
) -> Result<PluginPreview, LoadError> {
    // If URL ends with .wasm, derive manifest URL from same directory
    let (manifest_url, base_url) = if url.ends_with(".wasm") {
        let base = url.rsplit_once('/').map(|(b, _)| b).unwrap_or("");
        (format!("{}/manifest.json", base), base.to_string())
    } else if url.ends_with("manifest.json") {
        let base = url.rsplit_once('/').map(|(b, _)| b).unwrap_or("");
        (url.to_string(), base.to_string())
    } else {
        // Assume it's a base directory URL
        (
            format!("{}/manifest.json", url.trim_end_matches('/')),
            url.trim_end_matches('/').to_string(),
        )
    };

    let response = client
        .get(&manifest_url)
        .send()
        .await?
        .error_for_status()
        .map_err(|_| LoadError::ManifestNotFound(manifest_url.clone()))?;

    let manifest: PluginManifest = response.json().await?;

    // Derive WASM URL from manifest
    let wasm_url = format!("{}/{}", base_url, manifest.wasm_file);

    Ok(PluginPreview {
        manifest,
        manifest_url,
        wasm_url,
    })
}

/// Load a plugin from a manifest (fetches WASM separately)
pub async fn load_from_manifest(
    client: &reqwest::Client,
    preview: &PluginPreview,
    expected_sha256: Option<&str>,
) -> Result<LoadedPlugin, LoadError> {
    validate_manifest(&preview.manifest)?;

    let response = client
        .get(&preview.wasm_url)
        .send()
        .await?
        .error_for_status()?;
    let wasm_bytes = response.bytes().await?.to_vec();

    // Verify SHA256 if provided
    if let Some(expected) = expected_sha256 {
        let mut hasher = Sha256::new();
        hasher.update(&wasm_bytes);
        let actual = hex::encode(hasher.finalize());

        if actual != expected {
            return Err(LoadError::Sha256Mismatch {
                expected: expected.to_string(),
                actual,
            });
        }
    }

    let info: PluginInfo = preview.manifest.clone().into();
    validate_api_version(&info)?;
    validate_capabilities(&preview.manifest, &wasm_bytes)?;

    Ok(LoadedPlugin {
        info,
        source: PluginSource::Url {
            url: preview.wasm_url.clone(),
            sha256: expected_sha256.map(String::from),
        },
        wasm_bytes,
        manifest: Some(preview.manifest.clone()),
    })
}

/// Load a plugin from a local directory (requires manifest.json)
pub async fn load_from_file(path: &Path) -> Result<LoadedPlugin, LoadError> {
    // Load manifest.json
    let manifest_path = path.join("manifest.json");
    let manifest_content = tokio::fs::read_to_string(&manifest_path)
        .await
        .map_err(|_| LoadError::ManifestNotFound(manifest_path.display().to_string()))?;

    let manifest: PluginManifest = serde_json::from_str(&manifest_content)?;

    validate_manifest(&manifest)?;

    // Load WASM file specified in manifest.
    let wasm_path = path.join(&manifest.wasm_file);
    let wasm_bytes = tokio::fs::read(&wasm_path).await?;

    let info: PluginInfo = manifest.clone().into();
    validate_api_version(&info)?;
    validate_capabilities(&manifest, &wasm_bytes)?;

    Ok(LoadedPlugin {
        info,
        source: PluginSource::File {
            path: path.to_path_buf(),
        },
        wasm_bytes,
        manifest: Some(manifest),
    })
}

fn validate_api_version(info: &PluginInfo) -> Result<(), LoadError> {
    // Parse as integer for comparison
    let plugin_version: u32 = info.api_version.parse().unwrap_or(0);

    if plugin_version > MAX_API_VERSION {
        return Err(LoadError::UnsupportedApiVersion(info.api_version.clone()));
    }
    if plugin_version < MIN_API_VERSION {
        return Err(LoadError::ApiVersionTooOld {
            plugin_type: "plugin",
            api_version: info.api_version.clone(),
        });
    }

    Ok(())
}

/// Every plugin must declare every capability its imports need.
pub fn validate_capabilities(
    manifest: &PluginManifest,
    wasm_bytes: &[u8],
) -> Result<(), LoadError> {
    use crate::plugin::capabilities::{analyze_imports, check_declared};
    let requirements = analyze_imports(wasm_bytes).map_err(LoadError::UnknownImport)?;
    check_declared(&manifest.capabilities, &requirements).map_err(|missing| {
        let names: Vec<String> = missing
            .iter()
            .map(|r| {
                format!(
                    "{} (needs {})",
                    r.import,
                    r.any_of
                        .iter()
                        .map(|c| c.as_str())
                        .collect::<Vec<_>>()
                        .join(" or ")
                )
            })
            .collect();
        LoadError::CapabilityMismatch(format!(
            "plugin '{}' imports {} but does not declare the capability",
            manifest.id,
            names.join(", ")
        ))
    })
}

/// A bare hostname, optionally prefixed `*.` for subdomains. No scheme, path,
/// port or whitespace; at least one label after the wildcard.
pub fn is_valid_host_pattern(pattern: &str) -> bool {
    let host = pattern.strip_prefix("*.").unwrap_or(pattern);
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
}

/// Structural checks that do not need the WASM bytes.
pub fn validate_manifest(manifest: &PluginManifest) -> Result<(), LoadError> {
    use crate::lua::builtins::BUILTIN_PREFIX;
    use crate::plugin::PluginType;
    use crate::plugin::capabilities::PluginCapability;

    let api_version: u32 = manifest.api_version.parse().unwrap_or(0);
    if api_version > MAX_API_VERSION {
        return Err(LoadError::UnsupportedApiVersion(
            manifest.api_version.clone(),
        ));
    }
    if api_version < MIN_API_VERSION {
        return Err(LoadError::ApiVersionTooOld {
            plugin_type: manifest.plugin_type.as_str(),
            api_version: manifest.api_version.clone(),
        });
    }
    if manifest.plugin_type == PluginType::Library {
        let namespace = manifest.namespace.as_deref().unwrap_or(&manifest.id);
        if namespace.starts_with(BUILTIN_PREFIX) {
            return Err(LoadError::ReservedNamespace(namespace.to_string()));
        }
    }
    if manifest.plugin_type == PluginType::Auth && !manifest.dependencies.is_empty() {
        return Err(LoadError::InvalidDependency(format!(
            "auth plugin '{}' cannot declare dependencies",
            manifest.id
        )));
    }
    for dep in &manifest.dependencies {
        if dep.id == manifest.id {
            return Err(LoadError::InvalidDependency(format!(
                "plugin '{}' cannot depend on itself",
                manifest.id
            )));
        }
        if semver::VersionReq::parse(&dep.version).is_err() {
            return Err(LoadError::InvalidDependency(format!(
                "plugin '{}' declares an invalid version requirement '{}' for '{}'",
                manifest.id, dep.version, dep.id
            )));
        }
    }

    let restricted = manifest
        .capabilities
        .contains(&PluginCapability::NetworkRequest);
    let defined = manifest
        .capabilities
        .contains(&PluginCapability::NetworkRequestDefined);
    let unrestricted = manifest
        .capabilities
        .contains(&PluginCapability::NetworkRequestUnrestricted);
    if [restricted, defined, unrestricted]
        .into_iter()
        .filter(|&declared| declared)
        .count()
        > 1
    {
        return Err(LoadError::InvalidCapabilities(
            "network:request, network:request:defined and network:request:unrestricted are mutually exclusive; declare at most one".into(),
        ));
    }
    if restricted && manifest.allowed_hosts.is_empty() {
        return Err(LoadError::InvalidCapabilities(
            "network:request requires a non-empty allowed_hosts; use network:request:unrestricted to reach any host".into(),
        ));
    }
    if unrestricted && !manifest.allowed_hosts.is_empty() {
        return Err(LoadError::InvalidCapabilities(
            "network:request:unrestricted already grants every host; allowed_hosts must be empty"
                .into(),
        ));
    }
    if defined && !manifest.allowed_hosts.is_empty() {
        return Err(LoadError::InvalidCapabilities(
            "network:request:defined takes its hosts from the plugin's settings, not the manifest; allowed_hosts must be empty".into(),
        ));
    }
    for host in &manifest.allowed_hosts {
        if !is_valid_host_pattern(host) {
            return Err(LoadError::InvalidCapabilities(format!(
                "'{host}' is not a valid allowed_hosts entry (bare hostname, optionally prefixed '*.')"
            )));
        }
    }

    let expected_prefix = crate::plugin::secrets::secret_env_prefix(&manifest.id);
    for secret in &manifest.required_secrets {
        if !secret.key.starts_with(&expected_prefix) {
            return Err(LoadError::InvalidSecretKey {
                key: secret.key.clone(),
                expected_prefix,
            });
        }
    }

    Ok(())
}

/// Validate that all required secrets are present
pub fn validate_secrets(
    info: &PluginInfo,
    available_secrets: &std::collections::HashMap<String, String>,
) -> Result<(), LoadError> {
    for secret in &info.required_secrets {
        if !available_secrets.contains_key(secret) {
            return Err(LoadError::MissingSecret(secret.clone()));
        }
    }
    Ok(())
}

/// Parse PLUGIN_URLS environment variable
/// Format: id|url|sha256:hash,id|url|sha256:hash,...
pub fn parse_plugin_urls(env_value: &str) -> Vec<(String, String, Option<String>)> {
    env_value
        .split(',')
        .filter_map(|entry| {
            let parts: Vec<&str> = entry.trim().split('|').collect();
            if parts.len() >= 2 {
                let id = parts[0].to_string();
                let url = parts[1].to_string();
                let sha256 = parts
                    .get(2)
                    .and_then(|s| s.strip_prefix("sha256:").map(String::from));
                Some((id, url, sha256))
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_plugin_urls() {
        let input =
            "steam|https://example.com/steam.wasm|sha256:abc123,gog|https://example.com/gog.wasm";
        let result = parse_plugin_urls(input);

        assert_eq!(result.len(), 2);
        assert_eq!(
            result[0],
            (
                "steam".into(),
                "https://example.com/steam.wasm".into(),
                Some("abc123".into())
            )
        );
        assert_eq!(
            result[1],
            ("gog".into(), "https://example.com/gog.wasm".into(), None)
        );
    }

    #[test]
    fn test_validate_api_version() {
        let info = PluginInfo {
            id: "test".into(),
            name: "Test".into(),
            version: "1.0.0".into(),
            api_version: "2".into(),
            icon_url: None,
            required_secrets: vec![],
            auth_type: "oauth2".into(),
            config_schema: None,
        };

        assert!(validate_api_version(&info).is_ok());

        let old_info = PluginInfo {
            api_version: "1".into(),
            ..info.clone()
        };
        assert!(matches!(
            validate_api_version(&old_info),
            Err(LoadError::ApiVersionTooOld { .. })
        ));

        let future_info = PluginInfo {
            api_version: "99".into(),
            ..info
        };

        assert!(matches!(
            validate_api_version(&future_info),
            Err(LoadError::UnsupportedApiVersion(_))
        ));
    }

    fn manifest(json: &str) -> PluginManifest {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn validate_manifest_accepts_api_version_2_library() {
        let m = manifest(
            r#"{"id":"http","name":"HTTP","version":"1.0.0","api_version":"2","plugin_type":"library",
                "capabilities":["network:request:unrestricted"]}"#,
        );
        assert!(validate_manifest(&m).is_ok());
    }

    #[test]
    fn validate_manifest_rejects_library_on_api_version_1() {
        let m = manifest(
            r#"{"id":"http","name":"HTTP","version":"1.0.0","api_version":"1","plugin_type":"library"}"#,
        );
        assert!(matches!(
            validate_manifest(&m),
            Err(LoadError::ApiVersionTooOld { .. })
        ));
    }

    #[test]
    fn validate_manifest_rejects_future_api_version() {
        let m = manifest(r#"{"id":"x","name":"X","version":"1.0.0","api_version":"3"}"#);
        assert!(matches!(
            validate_manifest(&m),
            Err(LoadError::UnsupportedApiVersion(_))
        ));
    }

    #[test]
    fn validate_manifest_rejects_bad_dependency_requirement() {
        let m = manifest(
            r#"{"id":"record","name":"R","version":"1.0.0","api_version":"2","plugin_type":"library",
                "dependencies":[{"id":"db","version":"not a range"}]}"#,
        );
        let err = validate_manifest(&m).unwrap_err();
        assert!(matches!(err, LoadError::InvalidDependency(_)), "{err}");
        assert!(err.to_string().contains("db"));
    }

    #[test]
    fn validate_manifest_rejects_self_dependency() {
        let m = manifest(
            r#"{"id":"db","name":"DB","version":"1.0.0","api_version":"2","plugin_type":"library",
                "dependencies":[{"id":"db"}]}"#,
        );
        assert!(matches!(
            validate_manifest(&m),
            Err(LoadError::InvalidDependency(_))
        ));
    }

    #[test]
    fn validate_manifest_rejects_dependencies_on_auth_plugins() {
        let m = manifest(
            r#"{"id":"steam","name":"S","version":"1.0.0","api_version":"2",
                "dependencies":[{"id":"http"}]}"#,
        );
        assert!(matches!(
            validate_manifest(&m),
            Err(LoadError::InvalidDependency(_))
        ));
    }

    #[test]
    fn validate_manifest_rejects_api_version_1_for_auth_plugins() {
        let m = manifest(r#"{"id":"steam","name":"Steam","version":"1.0.0","api_version":"1"}"#);
        let err = validate_manifest(&m).unwrap_err();
        assert!(matches!(err, LoadError::ApiVersionTooOld { .. }), "{err}");
        let msg = err.to_string();
        assert!(msg.contains("api_version: \"2\""), "{msg}");
        assert!(msg.contains("capabilities"), "{msg}");
    }

    #[test]
    fn validate_manifest_network_request_needs_hosts() {
        let m = manifest(
            r#"{"id":"x","name":"X","version":"1.0.0","api_version":"2","plugin_type":"library",
                "capabilities":["network:request"]}"#,
        );
        let err = validate_manifest(&m).unwrap_err();
        assert!(matches!(err, LoadError::InvalidCapabilities(_)), "{err}");
        assert!(err.to_string().contains("allowed_hosts"));
    }

    #[test]
    fn validate_manifest_unrestricted_network_forbids_hosts() {
        let m = manifest(
            r#"{"id":"x","name":"X","version":"1.0.0","api_version":"2","plugin_type":"library",
                "capabilities":["network:request:unrestricted"],"allowed_hosts":["a.example"]}"#,
        );
        assert!(matches!(
            validate_manifest(&m),
            Err(LoadError::InvalidCapabilities(_))
        ));
    }

    #[test]
    fn validate_manifest_defined_network_forbids_manifest_hosts() {
        let m = manifest(
            r#"{"id":"x","name":"X","version":"1.0.0","api_version":"2","plugin_type":"library",
                "capabilities":["network:request:defined"],"allowed_hosts":["a.example"]}"#,
        );
        let err = validate_manifest(&m).unwrap_err();
        assert!(matches!(err, LoadError::InvalidCapabilities(_)), "{err}");
        assert!(err.to_string().contains("allowed_hosts"));
    }

    #[test]
    fn validate_manifest_defined_with_network_request_refused() {
        let m = manifest(
            r#"{"id":"x","name":"X","version":"1.0.0","api_version":"2","plugin_type":"library",
                "capabilities":["network:request:defined","network:request"],"allowed_hosts":["a.example"]}"#,
        );
        assert!(matches!(
            validate_manifest(&m),
            Err(LoadError::InvalidCapabilities(_))
        ));
    }

    #[test]
    fn validate_manifest_defined_with_unrestricted_refused() {
        let m = manifest(
            r#"{"id":"x","name":"X","version":"1.0.0","api_version":"2","plugin_type":"library",
                "capabilities":["network:request:defined","network:request:unrestricted"]}"#,
        );
        assert!(matches!(
            validate_manifest(&m),
            Err(LoadError::InvalidCapabilities(_))
        ));
    }

    #[test]
    fn validate_manifest_defined_with_empty_hosts_loads() {
        let m = manifest(
            r#"{"id":"x","name":"X","version":"1.0.0","api_version":"2","plugin_type":"library",
                "capabilities":["network:request:defined"]}"#,
        );
        assert!(validate_manifest(&m).is_ok());
    }

    #[test]
    fn validate_manifest_rejects_bad_host_patterns() {
        for bad in ["https://a.example", "a.example/path", "a b", "*", "*.", ""] {
            let m = manifest(&format!(
                r#"{{"id":"x","name":"X","version":"1.0.0","api_version":"2","plugin_type":"library",
                    "capabilities":["network:request"],"allowed_hosts":["{bad}"]}}"#
            ));
            assert!(
                matches!(
                    validate_manifest(&m),
                    Err(LoadError::InvalidCapabilities(_))
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn validate_manifest_accepts_a_secret_key_under_the_plugin_s_prefix() {
        let m = manifest(
            r#"{"id":"auth-steam","name":"Steam","version":"1.0.0","api_version":"2",
                "required_secrets":[{"key":"PLUGIN_AUTH_STEAM_API_KEY","name":"API key"}]}"#,
        );
        assert!(validate_manifest(&m).is_ok());
    }

    #[test]
    fn validate_manifest_rejects_a_secret_key_outside_the_plugin_s_prefix() {
        let m = manifest(
            r#"{"id":"auth-steam","name":"Steam","version":"1.0.0","api_version":"2",
                "required_secrets":[{"key":"PLUGIN_STEAM_API_KEY","name":"API key"}]}"#,
        );
        let err = validate_manifest(&m).unwrap_err();
        assert!(matches!(err, LoadError::InvalidSecretKey { .. }), "{err}");
        let msg = err.to_string();
        assert!(msg.contains("PLUGIN_STEAM_API_KEY"), "{msg}");
        assert!(msg.contains("PLUGIN_AUTH_STEAM_"), "{msg}");
    }

    /// Smallest module importing `host_kv_get` from `env`.
    fn module_importing_kv_get() -> Vec<u8> {
        wat::parse_str(
            r#"(module (import "env" "host_kv_get" (func)) (memory (export "memory") 1))"#,
        )
        .unwrap()
    }

    #[test]
    fn validate_capabilities_rejects_undeclared_import() {
        let m = manifest(
            r#"{"id":"x","name":"X","version":"1.0.0","api_version":"2","plugin_type":"library",
                "capabilities":[]}"#,
        );
        let err = validate_capabilities(&m, &module_importing_kv_get()).unwrap_err();
        assert!(matches!(err, LoadError::CapabilityMismatch(_)), "{err}");
        assert!(err.to_string().contains("host_kv_get"), "{err}");
    }

    #[test]
    fn validate_capabilities_accepts_declared_import() {
        let m = manifest(
            r#"{"id":"x","name":"X","version":"1.0.0","api_version":"2","plugin_type":"library",
                "capabilities":["kv:read"]}"#,
        );
        assert!(validate_capabilities(&m, &module_importing_kv_get()).is_ok());
    }

    #[test]
    fn internal_namespace_is_reserved() {
        let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
            "id": "acme-time", "name": "Time", "version": "1.0.0", "api_version": "2",
            "plugin_type": "library", "namespace": "internal.time", "capabilities": [],
        }))
        .unwrap();
        let err = validate_manifest(&manifest).unwrap_err();
        assert!(
            matches!(err, LoadError::ReservedNamespace(ref ns) if ns == "internal.time"),
            "{err}"
        );
        assert!(err.to_string().contains("reserved for built-in modules"));
    }

    #[test]
    fn internal_prefix_without_dot_is_allowed() {
        let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
            "id": "internals", "name": "I", "version": "1.0.0", "api_version": "2",
            "plugin_type": "library", "namespace": "internals", "capabilities": [],
        }))
        .unwrap();
        assert!(validate_manifest(&manifest).is_ok());
    }
}
