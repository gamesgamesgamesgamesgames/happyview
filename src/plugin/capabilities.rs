//! Plugin capabilities: the trust contract a plugin declares and the host
//! enforces. One capability per host-function group. The analysis half
//! (mapping a module's imports to capabilities) lives here too.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::plugin::LoadedPlugin;

/// How much an operator is trusting a plugin with. Drives the consent UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    Low,
    Medium,
    High,
    Critical,
}

/// One entry per host-function group. Adding a host function means adding
/// (or choosing) a capability here and mapping the import in `IMPORT_REQUIREMENTS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PluginCapability {
    #[serde(rename = "secrets:read")]
    SecretsRead,
    #[serde(rename = "kv:read")]
    KvRead,
    #[serde(rename = "kv:write")]
    KvWrite,
    #[serde(rename = "records:read")]
    RecordsRead,
    #[serde(rename = "network:request")]
    NetworkRequest,
    #[serde(rename = "network:request:unrestricted")]
    NetworkRequestUnrestricted,
    #[serde(rename = "library:call")]
    LibraryCall,
    #[serde(rename = "database:read")]
    DatabaseRead,
    #[serde(rename = "database:write")]
    DatabaseWrite,
    #[serde(rename = "caller:read")]
    CallerRead,
    #[serde(rename = "caller:write")]
    CallerWrite,
    #[serde(rename = "caller:call")]
    CallerCall,
    #[serde(rename = "records:write")]
    RecordsWrite,
    #[serde(rename = "atproto:read")]
    AtprotoRead,
    #[serde(rename = "attest:sign")]
    AttestSign,
}

impl PluginCapability {
    pub fn all() -> &'static [PluginCapability] {
        use PluginCapability::*;
        &[
            SecretsRead,
            KvRead,
            KvWrite,
            RecordsRead,
            NetworkRequest,
            NetworkRequestUnrestricted,
            LibraryCall,
            DatabaseRead,
            DatabaseWrite,
            CallerRead,
            CallerWrite,
            CallerCall,
            RecordsWrite,
            AtprotoRead,
            AttestSign,
        ]
    }

    pub fn as_str(&self) -> &'static str {
        use PluginCapability::*;
        match self {
            SecretsRead => "secrets:read",
            KvRead => "kv:read",
            KvWrite => "kv:write",
            RecordsRead => "records:read",
            NetworkRequest => "network:request",
            NetworkRequestUnrestricted => "network:request:unrestricted",
            LibraryCall => "library:call",
            DatabaseRead => "database:read",
            DatabaseWrite => "database:write",
            CallerRead => "caller:read",
            CallerWrite => "caller:write",
            CallerCall => "caller:call",
            RecordsWrite => "records:write",
            AtprotoRead => "atproto:read",
            AttestSign => "attest:sign",
        }
    }

    pub fn parse_str(s: &str) -> Option<Self> {
        Self::all().iter().copied().find(|c| c.as_str() == s)
    }

    pub fn risk(&self) -> Risk {
        use PluginCapability::*;
        match self {
            SecretsRead | KvRead | KvWrite => Risk::Low,
            RecordsRead | NetworkRequest | LibraryCall | CallerRead | AtprotoRead => Risk::Medium,
            NetworkRequestUnrestricted | DatabaseRead | CallerWrite | RecordsWrite | AttestSign => {
                Risk::High
            }
            DatabaseWrite | CallerCall => Risk::Critical,
        }
    }

    /// Operator-facing consequence, shown verbatim in the consent dialog.
    pub fn description(&self) -> &'static str {
        use PluginCapability::*;
        match self {
            SecretsRead => "Read the secrets you configure for this plugin.",
            KvRead => "Read its own key-value storage.",
            KvWrite => "Write to its own key-value storage (1 MB per scope).",
            RecordsRead => "Look up indexed AT Protocol records.",
            NetworkRequest => {
                "Make HTTP requests, only to the hosts it lists. Redirects are not followed."
            }
            NetworkRequestUnrestricted => {
                "Make HTTP requests to any host on the internet, including internal services this server can reach."
            }
            LibraryCall => {
                "Call other installed library plugins, which run with their own permissions (not this plugin's)."
            }
            DatabaseRead => {
                "Run arbitrary read-only SQL against indexed records, labels, lexicons, jobs and space data. Internal auth, secret and key tables are blocked."
            }
            DatabaseWrite => {
                "Run arbitrary SQL, including INSERT, UPDATE, DELETE and DROP, against indexed records, labels, lexicons, jobs and space data. This can destroy your index."
            }
            CallerRead => "Read from the AT Protocol network as the user who ran the script.",
            CallerWrite => {
                "Create, update and delete records in the user's own repository and upload blobs to it."
            }
            CallerCall => {
                "Call any XRPC procedure as the user, including ones that change their account. A procedure this instance does not serve is forwarded to the NSID's authority without the user's credentials."
            }
            RecordsWrite => {
                "Write to this instance's record index directly, bypassing the network. This can replace the body of a record that arrived from the network while its CID and indexed time stay as the network set them."
            }
            AtprotoRead => {
                "Resolve any DID's service endpoints, download blobs from any repo on the network, look up labels applied to any URI, and verify this instance's attestation signatures."
            }
            AttestSign => {
                "Sign records with this instance's attestation key, producing a signature that asserts this instance vouches for the content."
            }
        }
    }
}

/// What a host-function import needs: any one of these capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Requirement {
    pub import: &'static str,
    pub any_of: &'static [PluginCapability],
}

/// The import → capability table. `host_log` is deliberately absent: logging
/// is free. Add a row whenever `register_host_functions` gains an import.
const IMPORT_REQUIREMENTS: &[Requirement] = &[
    Requirement {
        import: "host_get_secret",
        any_of: &[PluginCapability::SecretsRead],
    },
    Requirement {
        import: "host_http_request",
        any_of: &[
            PluginCapability::NetworkRequest,
            PluginCapability::NetworkRequestUnrestricted,
        ],
    },
    Requirement {
        import: "host_kv_get",
        any_of: &[PluginCapability::KvRead],
    },
    Requirement {
        import: "host_kv_set",
        any_of: &[PluginCapability::KvWrite],
    },
    Requirement {
        import: "host_kv_delete",
        any_of: &[PluginCapability::KvWrite],
    },
    Requirement {
        import: "host_lookup_record",
        any_of: &[PluginCapability::RecordsRead],
    },
    Requirement {
        import: "host_call_library",
        any_of: &[PluginCapability::LibraryCall],
    },
    Requirement {
        import: "host_get_api_surface",
        any_of: &[PluginCapability::LibraryCall],
    },
    Requirement {
        import: "host_db_query",
        any_of: &[
            PluginCapability::DatabaseRead,
            PluginCapability::DatabaseWrite,
        ],
    },
    Requirement {
        import: "host_db_execute",
        any_of: &[PluginCapability::DatabaseWrite],
    },
    Requirement {
        import: "host_records_query",
        any_of: &[PluginCapability::RecordsRead],
    },
    Requirement {
        import: "host_records_count",
        any_of: &[PluginCapability::RecordsRead],
    },
    Requirement {
        import: "host_records_get",
        any_of: &[PluginCapability::RecordsRead],
    },
    Requirement {
        import: "host_records_search",
        any_of: &[PluginCapability::RecordsRead],
    },
    Requirement {
        import: "host_backlinks_query",
        any_of: &[PluginCapability::RecordsRead],
    },
    Requirement {
        import: "host_table_query",
        any_of: &[
            PluginCapability::DatabaseRead,
            PluginCapability::DatabaseWrite,
        ],
    },
    Requirement {
        import: "host_caller_xrpc_query",
        any_of: &[PluginCapability::CallerRead],
    },
    Requirement {
        import: "host_caller_create_record",
        any_of: &[PluginCapability::CallerWrite],
    },
    Requirement {
        import: "host_caller_put_record",
        any_of: &[PluginCapability::CallerWrite],
    },
    Requirement {
        import: "host_caller_delete_record",
        any_of: &[PluginCapability::CallerWrite],
    },
    Requirement {
        import: "host_caller_upload_blob",
        any_of: &[PluginCapability::CallerWrite],
    },
    Requirement {
        import: "host_caller_xrpc_procedure",
        any_of: &[PluginCapability::CallerCall],
    },
    Requirement {
        import: "host_records_index_put",
        any_of: &[PluginCapability::RecordsWrite],
    },
    Requirement {
        import: "host_records_index_delete",
        any_of: &[PluginCapability::RecordsWrite],
    },
    Requirement {
        import: "host_atproto_resolve_service",
        any_of: &[PluginCapability::AtprotoRead],
    },
    Requirement {
        import: "host_atproto_blob_download",
        any_of: &[PluginCapability::AtprotoRead],
    },
    Requirement {
        import: "host_labels_get",
        any_of: &[PluginCapability::AtprotoRead],
    },
    Requirement {
        import: "host_attest_sign",
        any_of: &[PluginCapability::AttestSign],
    },
    Requirement {
        import: "host_attest_verify",
        any_of: &[PluginCapability::AtprotoRead],
    },
];

/// Imports that cost a plugin nothing to declare: logging, and reading a
/// lexicon, which is published schema rather than anybody's data.
const FREE_IMPORTS: &[&str] = &["host_log", "host_lexicon_get"];

pub fn is_free_import(name: &str) -> bool {
    FREE_IMPORTS.contains(&name)
}

pub fn requirement_for_import(name: &str) -> Option<Requirement> {
    IMPORT_REQUIREMENTS
        .iter()
        .copied()
        .find(|r| r.import == name)
}

/// Read the module's import section. Every `env.*` import must be a known
/// host function — an unknown one would trap at instantiation anyway, and
/// refusing it here gives the author a message that names it.
pub fn analyze_imports(wasm: &[u8]) -> Result<Vec<Requirement>, String> {
    use wasmparser::{Parser, Payload};
    let mut out: Vec<Requirement> = Vec::new();
    for payload in Parser::new(0).parse_all(wasm) {
        let payload = payload.map_err(|e| format!("invalid WASM: {e}"))?;
        let Payload::ImportSection(reader) = payload else {
            continue;
        };
        for import in reader {
            let import = import.map_err(|e| format!("invalid import section: {e}"))?;
            if import.module != "env" {
                return Err(format!(
                    "unsupported import module '{}' (only 'env' host functions are available)",
                    import.module
                ));
            }
            if FREE_IMPORTS.contains(&import.name) {
                continue;
            }
            let req = requirement_for_import(import.name)
                .ok_or_else(|| format!("unknown host function import '{}'", import.name))?;
            if !out.contains(&req) {
                out.push(req);
            }
        }
    }
    Ok(out)
}

/// The least-privilege set satisfying `requirements`: the first option of
/// each. Useful when a plugin's declaration is missing a capability its
/// imports need — this is what it *would* need to declare, at minimum.
pub fn minimal_set(requirements: &[Requirement]) -> Vec<PluginCapability> {
    let mut out = Vec::new();
    for req in requirements {
        if let Some(first) = req.any_of.first()
            && !out.contains(first)
        {
            out.push(*first);
        }
    }
    out
}

/// Requirements not covered by `declared`.
pub fn check_declared(
    declared: &[PluginCapability],
    requirements: &[Requirement],
) -> Result<(), Vec<Requirement>> {
    let missing: Vec<Requirement> = requirements
        .iter()
        .copied()
        .filter(|req| !req.any_of.iter().any(|c| declared.contains(c)))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CapabilityEntry {
    pub name: String,
    pub risk: Risk,
    pub description: String,
}

impl From<PluginCapability> for CapabilityEntry {
    fn from(c: PluginCapability) -> Self {
        Self {
            name: c.as_str().into(),
            risk: c.risk(),
            description: c.description().into(),
        }
    }
}

/// What preview and list show. `undeclared` is non-empty only for a plugin
/// that the loader would refuse — every plugin must declare every capability
/// its imports need, with no exceptions by plugin type.
#[derive(Debug, Clone, Serialize)]
pub struct CapabilityReport {
    pub declared: Vec<CapabilityEntry>,
    pub required_by_imports: Vec<CapabilityEntry>,
    pub undeclared: Vec<String>,
}

pub fn report(plugin: &LoadedPlugin) -> Result<CapabilityReport, String> {
    let requirements = analyze_imports(&plugin.wasm_bytes)?;
    let declared = plugin.declared_capabilities();
    let required = minimal_set(&requirements);
    let undeclared = check_declared(declared, &requirements)
        .err()
        .unwrap_or_default()
        .into_iter()
        .map(|r| r.any_of[0].as_str().to_string())
        .collect();
    Ok(CapabilityReport {
        declared: declared.iter().copied().map(Into::into).collect(),
        required_by_imports: required.into_iter().map(Into::into).collect(),
        undeclared,
    })
}

/// The set a running instance is granted: exactly what the manifest declares.
pub fn effective_set(plugin: &LoadedPlugin) -> Result<HashSet<PluginCapability>, String> {
    Ok(plugin.declared_capabilities().iter().copied().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smallest module importing the named host functions from `env`.
    fn module_importing(names: &[&str]) -> Vec<u8> {
        let imports: String = names
            .iter()
            .map(|n| format!(r#"(import "env" "{n}" (func))"#))
            .collect::<Vec<_>>()
            .join("\n");
        wat::parse_str(format!("(module {imports} (memory (export \"memory\") 1))")).unwrap()
    }

    #[test]
    fn import_requirements_cover_every_host_function() {
        for name in [
            "host_log",
            "host_get_secret",
            "host_http_request",
            "host_kv_get",
            "host_kv_set",
            "host_kv_delete",
            "host_lookup_record",
            "host_call_library",
            "host_get_api_surface",
            "host_db_query",
            "host_db_execute",
            "host_records_query",
            "host_records_count",
            "host_records_get",
            "host_records_search",
            "host_backlinks_query",
            "host_table_query",
            "host_caller_create_record",
            "host_caller_put_record",
            "host_caller_delete_record",
            "host_caller_upload_blob",
            "host_caller_xrpc_query",
            "host_caller_xrpc_procedure",
            "host_records_index_put",
            "host_records_index_delete",
            "host_lexicon_get",
            "host_atproto_resolve_service",
            "host_atproto_blob_download",
            "host_labels_get",
            "host_attest_sign",
            "host_attest_verify",
        ] {
            // `None` is only right for the free imports.
            assert_eq!(
                requirement_for_import(name).is_none(),
                is_free_import(name),
                "{name}"
            );
        }
        assert_eq!(requirement_for_import("host_teleport"), None);
    }

    #[test]
    fn caller_imports_map_to_their_capability() {
        for (import, expected) in [
            ("host_caller_xrpc_query", PluginCapability::CallerRead),
            ("host_caller_create_record", PluginCapability::CallerWrite),
            ("host_caller_put_record", PluginCapability::CallerWrite),
            ("host_caller_delete_record", PluginCapability::CallerWrite),
            ("host_caller_upload_blob", PluginCapability::CallerWrite),
            ("host_caller_xrpc_procedure", PluginCapability::CallerCall),
            ("host_records_index_put", PluginCapability::RecordsWrite),
            ("host_records_index_delete", PluginCapability::RecordsWrite),
        ] {
            let req = requirement_for_import(import).expect(import);
            assert_eq!(req.any_of, &[expected], "{import}");
        }
    }

    /// A lexicon is public schema, so reading one costs a plugin nothing —
    /// and `analyze_imports` must not demand a declaration for it.
    #[test]
    fn lexicon_get_is_free() {
        assert!(is_free_import("host_lexicon_get"));
        let reqs = analyze_imports(&module_importing(&["host_lexicon_get"])).unwrap();
        assert!(reqs.is_empty());
    }

    #[test]
    fn analyze_imports_maps_imports_to_requirements() {
        let wasm = module_importing(&["host_log", "host_http_request", "host_kv_get"]);
        let reqs = analyze_imports(&wasm).unwrap();
        assert_eq!(reqs.len(), 2);
        assert_eq!(
            minimal_set(&reqs),
            vec![PluginCapability::NetworkRequest, PluginCapability::KvRead]
        );
    }

    #[test]
    fn analyze_imports_rejects_unknown_env_imports() {
        let wasm = module_importing(&["host_teleport"]);
        let err = analyze_imports(&wasm).unwrap_err();
        assert!(err.contains("host_teleport"), "{err}");
    }

    #[test]
    fn check_declared_accepts_either_network_capability() {
        let reqs = analyze_imports(&module_importing(&["host_http_request"])).unwrap();
        assert!(check_declared(&[PluginCapability::NetworkRequest], &reqs).is_ok());
        assert!(check_declared(&[PluginCapability::NetworkRequestUnrestricted], &reqs).is_ok());
        let missing = check_declared(&[PluginCapability::KvRead], &reqs).unwrap_err();
        assert_eq!(missing.len(), 1);
    }

    #[test]
    fn report_lists_declared_required_and_undeclared() {
        use crate::plugin::{LoadedPlugin, PluginInfo, PluginManifest, PluginSource};
        let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
            "id": "x", "name": "x", "version": "1.0.0", "api_version": "2",
            "plugin_type": "library", "capabilities": ["kv:read"],
        }))
        .unwrap();
        let plugin = LoadedPlugin {
            info: PluginInfo {
                id: "x".into(),
                name: "x".into(),
                version: "1.0.0".into(),
                api_version: "2".into(),
                icon_url: None,
                required_secrets: vec![],
                auth_type: "oauth2".into(),
                config_schema: None,
            },
            source: PluginSource::File {
                path: "/tmp/x".into(),
            },
            wasm_bytes: module_importing(&["host_kv_get", "host_db_execute"]),
            manifest: Some(manifest),
        };
        let report = report(&plugin).unwrap();
        assert_eq!(report.declared.len(), 1);
        assert_eq!(report.declared[0].name, "kv:read");
        assert_eq!(report.required_by_imports.len(), 2);
        assert_eq!(report.undeclared, vec!["database:write".to_string()]);
        let critical = report
            .required_by_imports
            .iter()
            .find(|e| e.name == "database:write")
            .unwrap();
        assert_eq!(critical.risk, Risk::Critical);
    }

    #[test]
    fn effective_set_is_exactly_declared() {
        use crate::plugin::{LoadedPlugin, PluginInfo, PluginManifest, PluginSource};
        // A manifest declaring only `kv:read` on a module that also imports
        // `host_http_request` — the effective set is exactly the declared
        // set. The loader, not the executor, is what refuses this mismatch.
        let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
            "id": "x", "name": "x", "version": "1.0.0", "api_version": "2",
            "plugin_type": "library", "capabilities": ["kv:read"],
        }))
        .unwrap();
        let plugin = LoadedPlugin {
            info: PluginInfo {
                id: "x".into(),
                name: "x".into(),
                version: "1.0.0".into(),
                api_version: "2".into(),
                icon_url: None,
                required_secrets: vec![],
                auth_type: "oauth2".into(),
                config_schema: None,
            },
            source: PluginSource::File {
                path: "/tmp/x".into(),
            },
            wasm_bytes: module_importing(&["host_kv_get", "host_http_request"]),
            manifest: Some(manifest),
        };
        let set = effective_set(&plugin).unwrap();
        assert_eq!(set, [PluginCapability::KvRead].into_iter().collect());
        assert!(!set.contains(&PluginCapability::NetworkRequest));
        assert!(!set.contains(&PluginCapability::NetworkRequestUnrestricted));
    }

    #[test]
    fn every_capability_round_trips_and_has_a_description() {
        for cap in PluginCapability::all() {
            assert_eq!(PluginCapability::parse_str(cap.as_str()), Some(*cap));
            assert!(!cap.description().is_empty());
            let json = serde_json::to_string(cap).unwrap();
            assert_eq!(json, format!("\"{}\"", cap.as_str()));
        }
        assert_eq!(PluginCapability::parse_str("nope"), None);
    }

    #[test]
    fn risk_tiers_order_the_dangerous_ones_last() {
        assert!(PluginCapability::SecretsRead.risk() < PluginCapability::NetworkRequest.risk());
        assert!(
            PluginCapability::NetworkRequest.risk()
                < PluginCapability::NetworkRequestUnrestricted.risk()
        );
        assert!(PluginCapability::DatabaseRead.risk() < PluginCapability::DatabaseWrite.risk());
        assert_eq!(PluginCapability::DatabaseWrite.risk(), Risk::Critical);
        assert_eq!(
            serde_json::to_string(&Risk::Critical).unwrap(),
            "\"critical\""
        );
    }

    /// Reading as the user, writing their repo, and running any procedure as
    /// them are three different sizes of trust, and the consent dialog sorts
    /// on exactly this.
    #[test]
    fn caller_tiers_rise_with_consequence() {
        assert_eq!(PluginCapability::CallerRead.risk(), Risk::Medium);
        assert_eq!(PluginCapability::CallerWrite.risk(), Risk::High);
        assert_eq!(PluginCapability::CallerCall.risk(), Risk::Critical);
        assert_eq!(PluginCapability::RecordsWrite.risk(), Risk::High);
        assert!(PluginCapability::CallerRead.risk() < PluginCapability::CallerWrite.risk());
        assert!(PluginCapability::CallerWrite.risk() < PluginCapability::CallerCall.risk());
    }

    /// The five AT Protocol/attestation imports: all but signing read the
    /// network or the label/record tables, so they share `atproto:read`;
    /// only producing a signature needs the stronger `attest:sign`.
    #[test]
    fn atproto_imports_map_to_their_capability() {
        for (import, expected) in [
            (
                "host_atproto_resolve_service",
                PluginCapability::AtprotoRead,
            ),
            ("host_atproto_blob_download", PluginCapability::AtprotoRead),
            ("host_labels_get", PluginCapability::AtprotoRead),
            ("host_attest_sign", PluginCapability::AttestSign),
            ("host_attest_verify", PluginCapability::AtprotoRead),
        ] {
            let req = requirement_for_import(import).expect(import);
            assert_eq!(req.any_of, &[expected], "{import}");
        }
    }

    #[test]
    fn caller_capabilities_parse_by_their_wire_names() {
        for (name, cap) in [
            ("caller:read", PluginCapability::CallerRead),
            ("caller:write", PluginCapability::CallerWrite),
            ("caller:call", PluginCapability::CallerCall),
            ("records:write", PluginCapability::RecordsWrite),
        ] {
            assert_eq!(PluginCapability::parse_str(name), Some(cap));
            assert_eq!(cap.as_str(), name);
            assert_eq!(serde_json::to_string(&cap).unwrap(), format!("\"{name}\""));
        }
    }
}
