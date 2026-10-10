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
    #[serde(rename = "network:request:defined")]
    NetworkRequestDefined,
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
    #[serde(rename = "blobs:read")]
    BlobsRead,
    #[serde(rename = "blobs:write")]
    BlobsWrite,
    #[serde(rename = "atproto:read")]
    AtprotoRead,
    #[serde(rename = "attest:sign")]
    AttestSign,
    #[serde(rename = "linked_repos:use")]
    LinkedReposUse,
    #[serde(rename = "jobs:create")]
    JobsCreate,
    #[serde(rename = "jobs:read")]
    JobsRead,
    #[serde(rename = "jobs:read_any")]
    JobsReadAny,
    #[serde(rename = "spaces:read")]
    SpacesRead,
    #[serde(rename = "spaces:write")]
    SpacesWrite,
    #[serde(rename = "wasi:clock")]
    WasiClock,
    #[serde(rename = "wasi:random")]
    WasiRandom,
    #[serde(rename = "wasi:stdio")]
    WasiStdio,
    #[serde(rename = "script:host")]
    ScriptHost,
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
            NetworkRequestDefined,
            NetworkRequestUnrestricted,
            LibraryCall,
            DatabaseRead,
            DatabaseWrite,
            CallerRead,
            CallerWrite,
            CallerCall,
            RecordsWrite,
            BlobsRead,
            BlobsWrite,
            AtprotoRead,
            AttestSign,
            LinkedReposUse,
            JobsCreate,
            JobsRead,
            JobsReadAny,
            SpacesRead,
            SpacesWrite,
            WasiClock,
            WasiRandom,
            WasiStdio,
            ScriptHost,
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
            NetworkRequestDefined => "network:request:defined",
            NetworkRequestUnrestricted => "network:request:unrestricted",
            LibraryCall => "library:call",
            DatabaseRead => "database:read",
            DatabaseWrite => "database:write",
            CallerRead => "caller:read",
            CallerWrite => "caller:write",
            CallerCall => "caller:call",
            RecordsWrite => "records:write",
            BlobsRead => "blobs:read",
            BlobsWrite => "blobs:write",
            AtprotoRead => "atproto:read",
            AttestSign => "attest:sign",
            LinkedReposUse => "linked_repos:use",
            JobsCreate => "jobs:create",
            JobsRead => "jobs:read",
            JobsReadAny => "jobs:read_any",
            SpacesRead => "spaces:read",
            SpacesWrite => "spaces:write",
            WasiClock => "wasi:clock",
            WasiRandom => "wasi:random",
            WasiStdio => "wasi:stdio",
            ScriptHost => "script:host",
        }
    }

    pub fn parse_str(s: &str) -> Option<Self> {
        Self::all().iter().copied().find(|c| c.as_str() == s)
    }

    pub fn risk(&self) -> Risk {
        use PluginCapability::*;
        match self {
            SecretsRead | KvRead | KvWrite | JobsRead | WasiClock | WasiRandom | ScriptHost => {
                Risk::Low
            }
            RecordsRead
            | BlobsRead
            | BlobsWrite
            | NetworkRequest
            | NetworkRequestDefined
            | LibraryCall
            | CallerRead
            | AtprotoRead
            | JobsCreate
            | WasiStdio => Risk::Medium,
            NetworkRequestUnrestricted
            | DatabaseRead
            | CallerWrite
            | RecordsWrite
            | AttestSign
            | LinkedReposUse
            | JobsReadAny
            | SpacesRead
            | SpacesWrite => Risk::High,
            DatabaseWrite | CallerCall => Risk::Critical,
        }
    }

    /// Operator-facing consequence, shown verbatim in the consent dialog.
    ///
    /// These sentences are also quoted, verbatim, in the Lua interpreter
    /// plugin's README in the plugins repository, where its five are listed
    /// for a script author. No test spans the two repositories, so a change
    /// here has to be carried there by hand.
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
            NetworkRequestDefined => {
                "Make HTTP requests only to the hosts you list for this plugin in its settings. Redirects are not followed."
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
            BlobsRead => {
                "Read any byte content this instance stores, and its media type and size, given its content address."
            }
            BlobsWrite => {
                "Store byte content in this instance's database, consuming disk without limit other than the instance's own. Content is addressed by its hash, so a write cannot replace or alter content already stored."
            }
            AtprotoRead => {
                "Resolve any DID's service endpoints, download blobs from any repo on the network, look up labels applied to any URI, and verify this instance's attestation signatures."
            }
            AttestSign => {
                "Sign records with this instance's attestation key, producing a signature that asserts this instance vouches for the content."
            }
            LinkedReposUse => {
                "Write records and upload blobs through any repo an admin has linked to this instance, within the scopes that admin granted, and call any XRPC method through it, which only that repo's PDS constrains."
            }
            JobsCreate => {
                "Enqueue background jobs as the user who ran the script, optionally carrying that user's PDS session into the job."
            }
            JobsRead => "Read background jobs the user who ran the script created.",
            JobsReadAny => {
                "Read every background job on this instance, including other users' job input and results."
            }
            SpacesRead => {
                "Read the members and records of every space this instance holds, regardless of each space's read policy."
            }
            SpacesWrite => {
                "Create spaces, write records into them, and manage their members and invites, as the user who ran the script and with that user's access."
            }
            WasiClock => "Read the wall clock.",
            WasiRandom => "Read secure random numbers.",
            WasiStdio => "Write to this instance's plugin log.",
            ScriptHost => {
                "Log, report progress, check for a stop request and wait on behalf of the script run it is inside; the host supplies the run's identity and job."
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
            PluginCapability::NetworkRequestDefined,
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
        import: "host_call_library_start",
        any_of: &[PluginCapability::LibraryCall],
    },
    Requirement {
        import: "host_call_library_wait_any",
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
        import: "host_blob_put",
        any_of: &[PluginCapability::BlobsWrite],
    },
    Requirement {
        import: "host_blob_get",
        any_of: &[PluginCapability::BlobsRead],
    },
    Requirement {
        import: "host_blob_stat",
        any_of: &[PluginCapability::BlobsRead],
    },
    Requirement {
        import: "host_atproto_resolve_service",
        any_of: &[PluginCapability::AtprotoRead],
    },
    Requirement {
        import: "host_atproto_resolve_identity",
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
    Requirement {
        import: "host_linked_repos_list",
        any_of: &[PluginCapability::LinkedReposUse],
    },
    Requirement {
        import: "host_linked_repo_create_record",
        any_of: &[PluginCapability::LinkedReposUse],
    },
    Requirement {
        import: "host_linked_repo_put_record",
        any_of: &[PluginCapability::LinkedReposUse],
    },
    Requirement {
        import: "host_linked_repo_delete_record",
        any_of: &[PluginCapability::LinkedReposUse],
    },
    Requirement {
        import: "host_linked_repo_upload_blob",
        any_of: &[PluginCapability::LinkedReposUse],
    },
    Requirement {
        import: "host_linked_repo_call",
        any_of: &[PluginCapability::LinkedReposUse],
    },
    Requirement {
        import: "host_jobs_create",
        any_of: &[PluginCapability::JobsCreate],
    },
    Requirement {
        import: "host_jobs_get",
        any_of: &[PluginCapability::JobsRead],
    },
    Requirement {
        import: "host_jobs_get_any",
        any_of: &[PluginCapability::JobsReadAny],
    },
    Requirement {
        import: "host_jobs_list_any",
        any_of: &[PluginCapability::JobsReadAny],
    },
    Requirement {
        import: "host_spaces_info",
        any_of: &[PluginCapability::SpacesRead],
    },
    Requirement {
        import: "host_spaces_query",
        any_of: &[PluginCapability::SpacesRead],
    },
    Requirement {
        import: "host_spaces_members",
        any_of: &[PluginCapability::SpacesRead],
    },
    Requirement {
        import: "host_spaces_access",
        any_of: &[PluginCapability::SpacesRead],
    },
    Requirement {
        import: "host_spaces_create",
        any_of: &[PluginCapability::SpacesWrite],
    },
    Requirement {
        import: "host_spaces_accept_invite",
        any_of: &[PluginCapability::SpacesWrite],
    },
    Requirement {
        import: "host_spaces_write_record",
        any_of: &[PluginCapability::SpacesWrite],
    },
    Requirement {
        import: "host_spaces_put_record",
        any_of: &[PluginCapability::SpacesWrite],
    },
    Requirement {
        import: "host_spaces_delete_record",
        any_of: &[PluginCapability::SpacesWrite],
    },
    Requirement {
        import: "host_spaces_add_member",
        any_of: &[PluginCapability::SpacesWrite],
    },
    Requirement {
        import: "host_spaces_set_member",
        any_of: &[PluginCapability::SpacesWrite],
    },
    Requirement {
        import: "host_spaces_remove_member",
        any_of: &[PluginCapability::SpacesWrite],
    },
    Requirement {
        import: "host_spaces_update",
        any_of: &[PluginCapability::SpacesWrite],
    },
    Requirement {
        import: "host_spaces_delete",
        any_of: &[PluginCapability::SpacesWrite],
    },
    Requirement {
        import: "host_spaces_create_invite",
        any_of: &[PluginCapability::SpacesWrite],
    },
    Requirement {
        import: "host_script_log",
        any_of: &[PluginCapability::ScriptHost],
    },
    Requirement {
        import: "host_job_progress",
        any_of: &[PluginCapability::ScriptHost],
    },
    Requirement {
        import: "host_job_should_stop",
        any_of: &[PluginCapability::ScriptHost],
    },
    Requirement {
        import: "host_job_wait",
        any_of: &[PluginCapability::ScriptHost],
    },
];

/// The preview-1 imports a plugin may link, each behind the capability that
/// names what it does. Everything else preview 1 offers — files, sockets,
/// `fd_read` — has no row, so no manifest can declare its way to one.
const WASI_IMPORT_REQUIREMENTS: &[Requirement] = &[
    Requirement {
        import: "clock_time_get",
        any_of: &[PluginCapability::WasiClock],
    },
    Requirement {
        import: "clock_res_get",
        any_of: &[PluginCapability::WasiClock],
    },
    Requirement {
        import: "random_get",
        any_of: &[PluginCapability::WasiRandom],
    },
    Requirement {
        import: "fd_write",
        any_of: &[PluginCapability::WasiStdio],
    },
];

/// Preview-1 imports a libc build links but that reach nothing against a
/// context with no arguments, environment or preopens: they answer from
/// empty tables, so declaring them costs a plugin nothing.
const WASI_FREE_IMPORTS: &[&str] = &[
    "proc_exit",
    "sched_yield",
    "fd_fdstat_get",
    "fd_prestat_get",
    "fd_prestat_dir_name",
    "args_get",
    "args_sizes_get",
    "environ_get",
    "environ_sizes_get",
];

pub const WASI_MODULE: &str = "wasi_snapshot_preview1";

pub fn is_free_wasi_import(name: &str) -> bool {
    WASI_FREE_IMPORTS.contains(&name)
}

pub fn requirement_for_wasi_import(name: &str) -> Option<Requirement> {
    WASI_IMPORT_REQUIREMENTS
        .iter()
        .copied()
        .find(|r| r.import == name)
}

/// Imports that cost a plugin nothing to declare: logging, reading a
/// lexicon (published schema rather than anybody's data), and reading back
/// the plugin's own effective allowed-hosts list, which a plugin needs to
/// behave sensibly regardless of which of the three network capabilities (or
/// none) it holds, and which carries nothing secret.
const FREE_IMPORTS: &[&str] = &["host_log", "host_lexicon_get", "host_allowed_hosts"];

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
/// host function and every `wasi_snapshot_preview1.*` import one of the few
/// the host answers — an unknown one would trap at instantiation anyway, and
/// refusing it here gives the author a message that names it.
pub fn analyze_imports(wasm: &[u8]) -> Result<Vec<Requirement>, String> {
    use wasmparser::{Parser, Payload};
    let mut out: Vec<Requirement> = Vec::new();
    for payload in Parser::new(0).parse_all(wasm) {
        let payload = payload.map_err(|e| format!("invalid WASM: {e}"))?;
        let Payload::ImportSection(reader) = payload else {
            continue;
        };
        for group in reader {
            let group = group.map_err(|e| format!("invalid import section: {e}"))?;
            for (module, name) in import_names(group)? {
                let req = match module {
                    "env" => {
                        if FREE_IMPORTS.contains(&name) {
                            continue;
                        }
                        requirement_for_import(name)
                            .ok_or_else(|| format!("unknown host function import '{name}'"))?
                    }
                    WASI_MODULE => {
                        if is_free_wasi_import(name) {
                            continue;
                        }
                        requirement_for_wasi_import(name).ok_or_else(|| {
                            format!(
                                "unsupported WASI import '{name}': plugins get the clock, randomness and the plugin log, never files, sockets or input"
                            )
                        })?
                    }
                    other => {
                        return Err(format!(
                            "unsupported import module '{other}' (only 'env' host functions and '{WASI_MODULE}' are available)"
                        ));
                    }
                };
                if !out.contains(&req) {
                    out.push(req);
                }
            }
        }
    }
    Ok(out)
}

/// The `(module, name)` pairs one import-section entry names. The compact
/// encodings group several imports under one module (and one type), and a
/// capability check cares about each name, not how it was encoded.
fn import_names(group: wasmparser::Imports<'_>) -> Result<Vec<(&str, &str)>, String> {
    use wasmparser::Imports;
    let invalid = |e: wasmparser::BinaryReaderError| format!("invalid import section: {e}");
    Ok(match group {
        Imports::Single(_, import) => vec![(import.module, import.name)],
        Imports::Compact1 { module, items } => items
            .into_iter()
            .map(|item| item.map(|item| (module, item.name)).map_err(invalid))
            .collect::<Result<_, _>>()?,
        Imports::Compact2 { module, names, .. } => names
            .into_iter()
            .map(|name| name.map(|name| (module, name)).map_err(invalid))
            .collect::<Result<_, _>>()?,
    })
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
            "host_call_library_start",
            "host_call_library_wait_any",
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
            "host_blob_put",
            "host_blob_get",
            "host_blob_stat",
            "host_records_index_put",
            "host_records_index_delete",
            "host_lexicon_get",
            "host_atproto_resolve_service",
            "host_atproto_blob_download",
            "host_labels_get",
            "host_attest_sign",
            "host_attest_verify",
            "host_linked_repos_list",
            "host_linked_repo_create_record",
            "host_linked_repo_put_record",
            "host_linked_repo_delete_record",
            "host_linked_repo_upload_blob",
            "host_linked_repo_call",
            "host_jobs_create",
            "host_jobs_get",
            "host_jobs_get_any",
            "host_jobs_list_any",
            "host_spaces_info",
            "host_spaces_query",
            "host_spaces_members",
            "host_spaces_access",
            "host_spaces_create",
            "host_spaces_accept_invite",
            "host_spaces_write_record",
            "host_spaces_put_record",
            "host_spaces_delete_record",
            "host_spaces_add_member",
            "host_spaces_set_member",
            "host_spaces_remove_member",
            "host_spaces_update",
            "host_spaces_delete",
            "host_spaces_create_invite",
            "host_allowed_hosts",
            "host_script_log",
            "host_job_progress",
            "host_job_should_stop",
            "host_job_wait",
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
    fn job_read_imports_map_to_their_capability() {
        assert_eq!(
            requirement_for_import("host_jobs_get").unwrap().any_of,
            &[PluginCapability::JobsRead]
        );
        assert_eq!(
            requirement_for_import("host_jobs_get_any").unwrap().any_of,
            &[PluginCapability::JobsReadAny]
        );
        assert_eq!(
            requirement_for_import("host_jobs_list_any").unwrap().any_of,
            &[PluginCapability::JobsReadAny]
        );
        assert_eq!(
            PluginCapability::parse_str("jobs:read"),
            Some(PluginCapability::JobsRead)
        );
        assert_eq!(
            PluginCapability::parse_str("jobs:read_any"),
            Some(PluginCapability::JobsReadAny)
        );
        assert!(PluginCapability::JobsReadAny.risk() > PluginCapability::JobsRead.risk());
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
        assert!(check_declared(&[PluginCapability::NetworkRequestDefined], &reqs).is_ok());
        assert!(check_declared(&[PluginCapability::NetworkRequestUnrestricted], &reqs).is_ok());
        let missing = check_declared(&[PluginCapability::KvRead], &reqs).unwrap_err();
        assert_eq!(missing.len(), 1);
    }

    /// A lexicon and a plugin's own resolved allowed-hosts list are the two
    /// free imports beyond logging — neither needs a declared capability.
    #[test]
    fn allowed_hosts_is_free() {
        assert!(is_free_import("host_allowed_hosts"));
        let reqs = analyze_imports(&module_importing(&["host_allowed_hosts"])).unwrap();
        assert!(reqs.is_empty());
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
        assert_eq!(
            PluginCapability::NetworkRequestDefined.risk(),
            PluginCapability::NetworkRequest.risk()
        );
        assert!(
            PluginCapability::NetworkRequest.risk()
                < PluginCapability::NetworkRequestUnrestricted.risk()
        );
        assert!(
            PluginCapability::NetworkRequestDefined.risk()
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

    /// The six AT Protocol/attestation imports: all but signing read the
    /// network or the label/record tables, so they share `atproto:read`;
    /// only producing a signature needs the stronger `attest:sign`.
    #[test]
    fn atproto_imports_map_to_their_capability() {
        for (import, expected) in [
            (
                "host_atproto_resolve_service",
                PluginCapability::AtprotoRead,
            ),
            (
                "host_atproto_resolve_identity",
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

    /// Reading stored bytes and storing them are separate decisions: a
    /// plugin that serves content need not be able to add any, and one that
    /// ingests need not be able to read back what others stored. Existence is
    /// answered by `host_blob_stat`, so there is no fourth import.
    #[test]
    fn blob_imports_map_to_their_capability() {
        for (import, expected) in [
            ("host_blob_put", PluginCapability::BlobsWrite),
            ("host_blob_get", PluginCapability::BlobsRead),
            ("host_blob_stat", PluginCapability::BlobsRead),
        ] {
            let req = requirement_for_import(import).expect(import);
            assert_eq!(req.any_of, &[expected], "{import}");
        }
    }

    /// Storing bytes consumes the operator's disk and reading them reaches
    /// content the instance holds, so neither is low risk; neither reaches
    /// another account's data or replaces anything, so neither is critical.
    #[test]
    fn blob_capabilities_sit_between_kv_and_raw_sql() {
        for capability in [PluginCapability::BlobsRead, PluginCapability::BlobsWrite] {
            assert!(
                capability.risk() > PluginCapability::KvWrite.risk(),
                "{capability:?} should outrank the key-value store"
            );
            assert!(
                capability.risk() < PluginCapability::DatabaseWrite.risk(),
                "{capability:?} should not rank with raw SQL"
            );
        }
        // Content addressing is the reason a write cannot replace anything,
        // and the consent dialog has to say so.
        assert!(
            PluginCapability::BlobsWrite
                .description()
                .contains("cannot replace"),
            "{}",
            PluginCapability::BlobsWrite.description()
        );
    }

    /// The six linked-repo imports share one capability — each re-reads its
    /// grant by DID on every call, so there is nothing narrower to gate one
    /// import on that another doesn't also need. Enqueuing a job is a
    /// separate trust decision from writing to a linked repo, so it gets its
    /// own.
    #[test]
    fn linked_repos_and_jobs_imports_map_to_their_capability() {
        for import in [
            "host_linked_repos_list",
            "host_linked_repo_create_record",
            "host_linked_repo_put_record",
            "host_linked_repo_delete_record",
            "host_linked_repo_upload_blob",
            "host_linked_repo_call",
        ] {
            let req = requirement_for_import(import).expect(import);
            assert_eq!(req.any_of, &[PluginCapability::LinkedReposUse], "{import}");
        }

        let req = requirement_for_import("host_jobs_create").expect("host_jobs_create");
        assert_eq!(req.any_of, &[PluginCapability::JobsCreate]);
    }

    /// Four reads share `spaces:read`; the other eleven imports (creating a
    /// space, joining one, writing/managing its records, members and
    /// invites) share `spaces:write` — reads never grant a write, and no
    /// write import is reachable on a read-only declaration.
    #[test]
    fn spaces_imports_map_to_their_capability() {
        for import in [
            "host_spaces_info",
            "host_spaces_query",
            "host_spaces_members",
            "host_spaces_access",
        ] {
            let req = requirement_for_import(import).expect(import);
            assert_eq!(req.any_of, &[PluginCapability::SpacesRead], "{import}");
        }

        for import in [
            "host_spaces_create",
            "host_spaces_accept_invite",
            "host_spaces_write_record",
            "host_spaces_put_record",
            "host_spaces_delete_record",
            "host_spaces_add_member",
            "host_spaces_set_member",
            "host_spaces_remove_member",
            "host_spaces_update",
            "host_spaces_delete",
            "host_spaces_create_invite",
        ] {
            let req = requirement_for_import(import).expect(import);
            assert_eq!(req.any_of, &[PluginCapability::SpacesWrite], "{import}");
        }
    }

    /// Both spaces capabilities sit at `High`, matching `database:read` —
    /// reading every space's members and records regardless of read policy
    /// is the same order of trust as arbitrary read-only SQL, and writing as
    /// the caller with that caller's own access is High rather than
    /// Critical because it can't act as anyone else.
    #[test]
    fn spaces_capabilities_are_high_risk_with_their_exact_descriptions() {
        assert_eq!(PluginCapability::SpacesRead.risk(), Risk::High);
        assert_eq!(
            PluginCapability::SpacesRead.description(),
            "Read the members and records of every space this instance holds, regardless of each space's read policy."
        );
        assert_eq!(PluginCapability::SpacesWrite.risk(), Risk::High);
        assert_eq!(
            PluginCapability::SpacesWrite.description(),
            "Create spaces, write records into them, and manage their members and invites, as the user who ran the script and with that user's access."
        );
    }

    /// Smallest module importing the named functions from `wasi_snapshot_preview1`.
    fn module_importing_wasi(names: &[&str]) -> Vec<u8> {
        let imports: String = names
            .iter()
            .map(|n| format!(r#"(import "wasi_snapshot_preview1" "{n}" (func))"#))
            .collect::<Vec<_>>()
            .join("\n");
        wat::parse_str(format!("(module {imports} (memory (export \"memory\") 1))")).unwrap()
    }

    /// Each preview-1 import the host answers sits behind the capability
    /// naming what it does, and `analyze_imports` reads them name by name so
    /// the loader's cross-check covers them like any `env` import.
    #[test]
    fn wasi_imports_map_to_their_capability() {
        for (import, expected) in [
            ("clock_time_get", PluginCapability::WasiClock),
            ("clock_res_get", PluginCapability::WasiClock),
            ("random_get", PluginCapability::WasiRandom),
            ("fd_write", PluginCapability::WasiStdio),
        ] {
            let req = requirement_for_wasi_import(import).expect(import);
            assert_eq!(req.any_of, &[expected], "{import}");
            let reqs = analyze_imports(&module_importing_wasi(&[import])).unwrap();
            assert_eq!(minimal_set(&reqs), vec![expected], "{import}");
            assert!(check_declared(&[expected], &reqs).is_ok(), "{import}");
            assert!(check_declared(&[], &reqs).is_err(), "{import}");
        }
        let reqs = analyze_imports(&module_importing_wasi(&[
            "clock_time_get",
            "clock_res_get",
            "random_get",
            "fd_write",
        ]))
        .unwrap();
        assert_eq!(
            minimal_set(&reqs),
            vec![
                PluginCapability::WasiClock,
                PluginCapability::WasiRandom,
                PluginCapability::WasiStdio
            ]
        );
    }

    /// The lifecycle imports a libc build links answer from an empty
    /// context, so a module naming them needs nothing declared.
    #[test]
    fn wasi_lifecycle_imports_are_free() {
        let names = [
            "proc_exit",
            "sched_yield",
            "fd_fdstat_get",
            "fd_prestat_get",
            "fd_prestat_dir_name",
            "args_get",
            "args_sizes_get",
            "environ_get",
            "environ_sizes_get",
        ];
        for name in names {
            assert!(is_free_wasi_import(name), "{name}");
            assert!(requirement_for_wasi_import(name).is_none(), "{name}");
        }
        let reqs = analyze_imports(&module_importing_wasi(&names)).unwrap();
        assert!(reqs.is_empty());
    }

    /// No capability grants a file, socket or input import: the refusal
    /// names the import so the author knows which one to drop.
    #[test]
    fn wasi_filesystem_socket_and_input_imports_are_refused() {
        for name in [
            "path_open",
            "fd_read",
            "fd_close",
            "sock_accept",
            "sock_recv",
            "poll_oneoff",
            "fd_seek",
        ] {
            assert!(!is_free_wasi_import(name), "{name}");
            let err = analyze_imports(&module_importing_wasi(&[name])).unwrap_err();
            assert!(err.contains(name), "{name}: {err}");
        }
        let err =
            analyze_imports(&module_importing_wasi(&["clock_time_get", "path_open"])).unwrap_err();
        assert!(err.contains("path_open"), "{err}");
    }

    /// Any import module other than `env` and preview 1 is still refused.
    #[test]
    fn other_import_modules_are_refused() {
        let wasm = wat::parse_str(
            r#"(module (import "wasi_snapshot_preview2" "clock_time_get" (func)) (memory (export "memory") 1))"#,
        )
        .unwrap();
        let err = analyze_imports(&wasm).unwrap_err();
        assert!(err.contains("wasi_snapshot_preview2"), "{err}");
    }

    /// The four `env` imports only an interpreter reaches share one Low
    /// capability: each acts on the run it is inside, and the host supplies
    /// the identity and the job id.
    #[test]
    fn interpreter_env_imports_need_script_host() {
        for import in [
            "host_script_log",
            "host_job_progress",
            "host_job_should_stop",
            "host_job_wait",
        ] {
            let req = requirement_for_import(import).expect(import);
            assert_eq!(req.any_of, &[PluginCapability::ScriptHost], "{import}");
            let reqs = analyze_imports(&module_importing(&[import])).unwrap();
            assert!(check_declared(&[], &reqs).is_err(), "{import}");
            assert!(
                check_declared(&[PluginCapability::ScriptHost], &reqs).is_ok(),
                "{import}"
            );
        }
        assert_eq!(PluginCapability::ScriptHost.risk(), Risk::Low);
    }

    /// The consent sentence for each preview-1 capability says what the
    /// import does; the word WASI tells an operator nothing.
    #[test]
    fn wasi_capabilities_describe_the_consequence_not_the_mechanism() {
        for (cap, name, risk) in [
            (PluginCapability::WasiClock, "wasi:clock", Risk::Low),
            (PluginCapability::WasiRandom, "wasi:random", Risk::Low),
            (PluginCapability::WasiStdio, "wasi:stdio", Risk::Medium),
            (PluginCapability::ScriptHost, "script:host", Risk::Low),
        ] {
            assert_eq!(cap.as_str(), name);
            assert_eq!(PluginCapability::parse_str(name), Some(cap));
            assert_eq!(cap.risk(), risk, "{name}");
            let description = cap.description();
            assert!(
                !description.to_lowercase().contains("wasi"),
                "{name}: {description}"
            );
            assert!(description.ends_with('.'), "{name}: {description}");
            assert_eq!(description.matches(". ").count(), 0, "{name}: one sentence");
        }
        assert_eq!(
            PluginCapability::WasiClock.description(),
            "Read the wall clock."
        );
        assert_eq!(
            PluginCapability::WasiStdio.description(),
            "Write to this instance's plugin log."
        );
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
