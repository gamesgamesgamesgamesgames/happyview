use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Lexicon types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct LexiconSummary {
    pub(super) id: String,
    pub(super) revision: i32,
    pub(super) lexicon_type: String,
    pub(super) backfill: bool,
    pub(super) action: Option<String>,
    pub(super) target_collection: Option<String>,
    /// The layer answering for this id, under the name the dashboard has
    /// always read. `layer` says the same and is what new clients should read.
    pub(super) source: String,
    pub(super) layer: crate::catalog::Layer,
    /// Project-relative file path, for a project lexicon only.
    pub(super) path: Option<String>,
    /// A stored row sits under this project file and answers again when the
    /// file goes.
    pub(super) shadows_stored: bool,
    /// Listed in `happyview.toml`'s `[network_lexicons] include`, so a delete
    /// is refused: the file would restore it at the next boot.
    pub(super) declared: bool,
    pub(super) authority_did: Option<String>,
    pub(super) last_fetched_at: Option<String>,
    /// `None` for a project lexicon, which has no row.
    pub(super) created_at: Option<String>,
    pub(super) updated_at: Option<String>,
    /// For record-type lexicons: the `properties` object from `defs.main.record`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) record_schema: Option<Value>,
    pub(super) token_cost: Option<i32>,
    /// Whether the ingest watches this record lexicon's collection.
    pub(super) indexed: bool,
}

impl From<crate::catalog::LexiconListing> for LexiconSummary {
    fn from(l: crate::catalog::LexiconListing) -> Self {
        use crate::lexicon::{LexiconType, ParsedLexicon, ProcedureAction};
        let parsed = ParsedLexicon::parse(
            l.lexicon_json,
            l.revision,
            None,
            ProcedureAction::Upsert,
            None,
        );
        let lexicon_type = parsed
            .as_ref()
            .map(|p| format!("{:?}", p.lexicon_type).to_lowercase())
            .unwrap_or_else(|_| "unknown".into());
        let record_schema = parsed
            .ok()
            .filter(|p| p.lexicon_type == LexiconType::Record)
            .and_then(|p| p.record_schema);
        Self {
            id: l.id,
            revision: l.revision,
            lexicon_type,
            backfill: l.backfill,
            action: l.action,
            target_collection: l.target_collection,
            source: l.layer.as_str().to_string(),
            layer: l.layer,
            path: l.path.as_deref().map(crate::catalog::path_string),
            shadows_stored: l.shadows_stored,
            declared: false,
            authority_did: l.authority_did,
            last_fetched_at: l.last_fetched_at,
            created_at: l.created_at,
            updated_at: l.updated_at,
            record_schema,
            token_cost: l.token_cost,
            indexed: l.indexed,
        }
    }
}

#[derive(Deserialize)]
pub(super) struct UploadLexiconBody {
    pub(super) lexicon_json: Value,
    #[serde(default = "default_backfill")]
    pub(super) backfill: bool,
    pub(super) target_collection: Option<String>,
    pub(super) action: Option<String>,
    pub(super) token_cost: Option<i32>,
    #[serde(default = "default_indexed")]
    pub(super) indexed: bool,
}

fn default_indexed() -> bool {
    true
}

fn default_backfill() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Stats types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct StatsResponse {
    pub(super) total_records: i64,
    pub(super) collections: Vec<CollectionStat>,
}

#[derive(Serialize)]
pub(super) struct CollectionStat {
    pub(super) collection: String,
    pub(super) count: i64,
}

// ---------------------------------------------------------------------------
// Network lexicon types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct AddNetworkLexiconBody {
    pub(super) nsid: String,
    #[serde(default)]
    pub(super) backfill: bool,
    pub(super) target_collection: Option<String>,
    #[serde(default = "default_indexed")]
    pub(super) indexed: bool,
}

#[derive(Serialize)]
pub(super) struct NetworkLexiconSummary {
    pub(super) nsid: String,
    pub(super) authority_did: String,
    pub(super) target_collection: Option<String>,
    pub(super) last_fetched_at: Option<String>,
    pub(super) created_at: String,
    pub(super) declared: bool,
    pub(super) indexed: bool,
}

// ---------------------------------------------------------------------------
// User management types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct CreateUserBody {
    pub(super) did: String,
    pub(super) template: Option<super::permissions::Template>,
    pub(super) permissions: Option<Vec<String>>,
}

#[derive(Serialize)]
pub(super) struct UserSummary {
    pub(super) id: String,
    pub(super) did: String,
    pub(super) is_super: bool,
    pub(super) permissions: Vec<String>,
    pub(super) created_at: String,
    pub(super) last_used_at: Option<String>,
}

// ---------------------------------------------------------------------------
// API key types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct CreateApiKeyBody {
    pub(super) name: String,
    pub(super) permissions: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct ApiKeySummary {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) key_prefix: String,
    pub(super) permissions: Vec<String>,
    pub(super) created_at: String,
    pub(super) last_used_at: Option<String>,
    pub(super) revoked_at: Option<String>,
}

#[derive(Serialize)]
pub(super) struct CreateApiKeyResponse {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) key: String,
    pub(super) key_prefix: String,
    pub(super) permissions: Vec<String>,
}

// ---------------------------------------------------------------------------
// Script variable types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct ScriptVariableSummary {
    pub(super) key: String,
    pub(super) preview: String,
    pub(super) created_at: String,
    pub(super) updated_at: String,
}

#[derive(Deserialize)]
pub(super) struct UpsertScriptVariableBody {
    pub(super) key: String,
    pub(super) value: String,
}

// ---------------------------------------------------------------------------
// Labeler subscription types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct AddLabelerBody {
    pub(super) did: String,
}

#[derive(Serialize)]
pub(super) struct LabelerSummary {
    pub(super) declared: bool,
    pub(super) did: String,
    pub(super) status: String,
    pub(super) cursor: Option<i64>,
    pub(super) created_at: String,
    pub(super) updated_at: String,
}

#[derive(Deserialize)]
pub(super) struct UpdateLabelerBody {
    pub(super) status: String,
}

// ---------------------------------------------------------------------------
// Settings types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct SettingEntry {
    pub(super) key: String,
    pub(super) value: String,
    pub(super) source: String,
}

#[derive(Deserialize)]
pub(super) struct UpsertSettingBody {
    pub(super) value: String,
}

// ---------------------------------------------------------------------------
// User permission / transfer types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct UpdatePermissionsBody {
    #[serde(default)]
    pub(super) grant: Vec<String>,
    #[serde(default)]
    pub(super) revoke: Vec<String>,
}

#[derive(Deserialize)]
pub(super) struct TransferSuperBody {
    pub(super) target_user_id: String,
}

// ---------------------------------------------------------------------------
// Plugin types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct PluginsListResponse {
    pub(super) plugins: Vec<PluginSummary>,
    pub(super) encryption_configured: bool,
}

#[derive(Serialize)]
pub(super) struct PluginSummary {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) version: String,
    /// What a surface shows about a plugin before it shows what it does.
    /// Each is independently optional: a plugin may carry an icon, a banner,
    /// both, or neither.
    pub(super) description: Option<String>,
    pub(super) icon_url: Option<String>,
    pub(super) banner_url: Option<String>,
    pub(super) source: String,
    pub(super) url: Option<String>,
    pub(super) sha256: Option<String>,
    pub(super) enabled: bool,
    pub(super) auth_type: String,
    pub(super) required_secrets: Vec<SecretDefinition>,
    /// Whether all required secrets have been configured
    pub(super) secrets_configured: bool,
    pub(super) loaded_at: Option<String>,
    #[serde(default)]
    pub(super) update_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) latest_version: Option<String>,
    #[serde(default)]
    pub(super) pending_releases: Vec<crate::plugin::official_registry::ReleaseEntry>,
    /// The manifest of the catalogue entry the update gate approved for this
    /// install, which is what an update installs. Absent when no entry is
    /// offered to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) latest_manifest_url: Option<String>,
    pub(super) plugin_type: String,
    pub(super) namespace: Option<String>,
    #[serde(default)]
    pub(super) dependencies: Vec<crate::plugin::PluginDependency>,
    #[serde(default)]
    pub(super) allowed_hosts: Vec<String>,
    pub(super) capabilities: crate::plugin::capabilities::CapabilityReport,
}

/// GET /admin/plugins query params.
#[derive(Deserialize)]
pub(super) struct ListPluginsQuery {
    #[serde(rename = "type")]
    pub(super) plugin_type: Option<String>,
}

/// DELETE /admin/plugins/{id} query params.
#[derive(Deserialize, Default)]
pub(super) struct RemovePluginQuery {
    #[serde(default)]
    pub(super) force: bool,
}

#[derive(Serialize)]
pub(super) struct OfficialPluginSummary {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) description: Option<String>,
    pub(super) icon_url: Option<String>,
    pub(super) latest_version: String,
    pub(super) manifest_url: String,
    pub(super) official: bool,
    pub(super) banner_url: Option<String>,
    /// Every release the catalogue knows, newest first. A detail page lists
    /// them; `PluginSummary::pending_releases` carries only the newer ones,
    /// which cannot answer "what versions exist".
    pub(super) releases: Vec<crate::plugin::official_registry::ReleaseEntry>,
    /// The publisher's long-form prose, as published. Empty for a
    /// GitHub-sourced entry, which has no profile record behind it.
    pub(super) sections: crate::plugin::official_registry::PluginSections,
}

#[derive(Serialize)]
pub(super) struct OfficialPluginsListResponse {
    pub(super) plugins: Vec<OfficialPluginSummary>,
    pub(super) last_refreshed_at: Option<String>,
}

#[derive(Deserialize, Default)]
pub(super) struct ReloadPluginBody {
    #[serde(default)]
    pub(super) url: Option<String>,
    #[serde(default)]
    pub(super) accepted_capabilities: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub(super) struct AddPluginBody {
    pub(super) url: String,
    pub(super) sha256: Option<String>,
    #[serde(default)]
    pub(super) accepted_capabilities: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub(super) struct PreviewPluginBody {
    pub(super) url: String,
}

#[derive(Serialize)]
pub(super) struct SecretDefinition {
    pub(super) key: String,
    pub(super) name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) description: Option<String>,
}

#[derive(Serialize)]
pub(super) struct PluginPreviewResponse {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) version: String,
    pub(super) description: Option<String>,
    pub(super) icon_url: Option<String>,
    pub(super) banner_url: Option<String>,
    pub(super) auth_type: String,
    pub(super) required_secrets: Vec<SecretDefinition>,
    pub(super) manifest_url: String,
    pub(super) wasm_url: String,
    pub(super) plugin_type: String,
    pub(super) namespace: Option<String>,
    #[serde(default)]
    pub(super) dependencies: Vec<crate::plugin::PluginDependency>,
    #[serde(default)]
    pub(super) allowed_hosts: Vec<String>,
    pub(super) capabilities: crate::plugin::capabilities::CapabilityReport,
    pub(super) sha256: String,
}

#[derive(Serialize)]
pub(super) struct PluginSecretsResponse {
    pub(super) plugin_id: String,
    pub(super) secrets: std::collections::HashMap<String, String>,
}

#[derive(Deserialize)]
pub(super) struct UpdatePluginSecretsBody {
    pub(super) secrets: std::collections::HashMap<String, String>,
}

#[derive(Serialize)]
pub(super) struct PluginAllowedHostsResponse {
    pub(super) hosts: Vec<String>,
}

#[derive(Deserialize)]
pub(super) struct UpdatePluginAllowedHostsBody {
    pub(super) hosts: Vec<String>,
}

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct DomainResponse {
    pub(super) id: String,
    pub(super) url: String,
    pub(super) is_primary: bool,
    pub(super) created_at: String,
    pub(super) updated_at: String,
}

#[derive(Deserialize)]
pub(super) struct CreateDomainBody {
    pub(super) url: String,
}

// ---------------------------------------------------------------------------
// API client types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct CreateApiClientBody {
    pub(super) name: String,
    pub(super) client_id_url: String,
    pub(super) client_uri: String,
    pub(super) redirect_uris: Vec<String>,
    #[serde(default = "default_scopes")]
    pub(super) scopes: String,
    pub(super) rate_limit_capacity: Option<i32>,
    pub(super) rate_limit_refill_rate: Option<f64>,
    #[serde(default = "default_client_type")]
    pub(super) client_type: String,
    pub(super) allowed_origins: Option<Vec<String>>,
}

fn default_scopes() -> String {
    "atproto".to_string()
}

fn default_client_type() -> String {
    "confidential".to_string()
}

#[derive(Deserialize)]
pub(super) struct UpdateApiClientBody {
    pub(super) name: Option<String>,
    pub(super) client_uri: Option<String>,
    pub(super) redirect_uris: Option<Vec<String>>,
    pub(super) scopes: Option<String>,
    pub(super) allowed_origins: Option<Option<Vec<String>>>,
    pub(super) rate_limit_capacity: Option<Option<i32>>,
    pub(super) rate_limit_refill_rate: Option<Option<f64>>,
    pub(super) is_active: Option<bool>,
}

#[derive(Serialize)]
pub(super) struct ApiClientSummary {
    pub(super) id: String,
    pub(super) client_key: String,
    pub(super) name: String,
    pub(super) client_id_url: String,
    pub(super) client_uri: String,
    pub(super) redirect_uris: Vec<String>,
    pub(super) scopes: String,
    pub(super) client_type: String,
    pub(super) allowed_origins: Option<Vec<String>>,
    pub(super) rate_limit_capacity: Option<i32>,
    pub(super) rate_limit_refill_rate: Option<f64>,
    pub(super) is_active: bool,
    pub(super) created_by: String,
    pub(super) created_at: String,
    pub(super) updated_at: String,
    pub(super) parent_client_id: Option<String>,
    pub(super) owner_did: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct CreateApiClientResponse {
    pub(crate) id: String,
    pub(crate) client_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) client_secret: Option<String>,
    pub(crate) name: String,
    pub(crate) client_id_url: String,
    pub(crate) client_type: String,
}
