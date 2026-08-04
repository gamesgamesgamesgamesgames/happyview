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
    pub(super) source: String,
    pub(super) authority_did: Option<String>,
    pub(super) last_fetched_at: Option<String>,
    pub(super) created_at: String,
    pub(super) updated_at: String,
    /// For record-type lexicons: the `properties` object from `defs.main.record`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) record_schema: Option<Value>,
    pub(super) token_cost: Option<i32>,
}

#[derive(Deserialize)]
pub(super) struct UploadLexiconBody {
    pub(super) lexicon_json: Value,
    #[serde(default = "default_backfill")]
    pub(super) backfill: bool,
    pub(super) target_collection: Option<String>,
    pub(super) action: Option<String>,
    pub(super) token_cost: Option<i32>,
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
// Backfill types
// ---------------------------------------------------------------------------

#[derive(Deserialize, Clone)]
pub(super) struct CreateBackfillBody {
    pub(super) collection: Option<String>,
    pub(super) did: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct BackfillJob {
    pub(crate) id: String,
    pub(crate) collection: Option<String>,
    pub(crate) did: Option<String>,
    pub(crate) status: String,
    pub(crate) stage: String,
    pub(crate) total_repos: Option<i32>,
    pub(crate) resolved_repos: Option<i32>,
    pub(crate) processed_repos: Option<i32>,
    pub(crate) total_records: Option<i32>,
    pub(crate) error: Option<String>,
    pub(crate) started_at: Option<String>,
    pub(crate) completed_at: Option<String>,
    pub(crate) created_at: String,
}

// ---------------------------------------------------------------------------
// Backfill event types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BackfillEvent {
    RepoDiscovered {
        job_id: String,
        did: String,
    },
    RepoResolved {
        job_id: String,
        did: String,
        pds_endpoint: String,
    },
    RepoFetched {
        job_id: String,
        did: String,
        pds_endpoint: String,
        records_fetched: i32,
    },
    JobCounters {
        job_id: String,
        total_repos: Option<i32>,
        resolved_repos: Option<i32>,
        processed_repos: Option<i32>,
        total_records: Option<i32>,
    },
    JobStageChanged {
        job_id: String,
        stage: String,
    },
    JobCompleted {
        job_id: String,
        status: String,
        error: Option<String>,
    },
    /// Current job state, straight from the database.
    ///
    /// The SSE stream is otherwise delta-only over a lossy broadcast channel,
    /// so a client that connects mid-phase or misses an event has no way to
    /// recover. A snapshot is how it resyncs.
    JobSnapshot {
        job_id: String,
        status: String,
        stage: String,
        total_repos: Option<i32>,
        resolved_repos: Option<i32>,
        processed_repos: Option<i32>,
        total_records: Option<i32>,
        error_counts: serde_json::Value,
    },
}

// ---------------------------------------------------------------------------
// Backfill detail response types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(crate) struct BackfillRepoEntry {
    pub(crate) did: String,
    pub(crate) pds_endpoint: Option<String>,
    pub(crate) status: String,
    pub(crate) records_fetched: i32,
}

#[derive(Serialize)]
pub(crate) struct BackfillReposResponse {
    pub(crate) repos: Vec<BackfillRepoEntry>,
    pub(crate) cursor: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct PdsSummaryEntry {
    pub(crate) pds_endpoint: String,
    pub(crate) total_repos: i32,
    pub(crate) completed_repos: i32,
    pub(crate) total_records: i32,
}

#[derive(Serialize)]
pub(crate) struct PdsSummaryResponse {
    pub(crate) pds_endpoints: Vec<PdsSummaryEntry>,
}

#[derive(Serialize)]
pub(crate) struct BackfillErrorEntry {
    pub(crate) did: String,
    pub(crate) collection: Option<String>,
    pub(crate) phase: String,
    pub(crate) kind: String,
    pub(crate) message: String,
    pub(crate) attempts: i32,
    pub(crate) last_at: String,
}

#[derive(Serialize)]
pub(crate) struct BackfillErrorsResponse {
    pub(crate) errors: Vec<BackfillErrorEntry>,
    pub(crate) cursor: Option<String>,
    /// Exact per-kind totals, which stay correct past the detail cap.
    pub(crate) counts: Vec<BackfillErrorCount>,
    /// True when detail rows stopped being written.
    pub(crate) capped: bool,
    /// `ERROR_DETAIL_CAP`, served so the dashboard's truncation notice never
    /// hardcodes the number and goes stale if the constant is retuned.
    pub(crate) cap: i64,
}

/// One kind's total, carrying its own retryability.
///
/// `retryable` is served rather than duplicated in the dashboard: it is policy,
/// and `BackfillErrorKind::is_retryable` is its only definition. A TypeScript
/// copy would drift the first time a kind is added.
#[derive(Serialize)]
pub(crate) struct BackfillErrorCount {
    pub(crate) kind: String,
    pub(crate) count: i64,
    pub(crate) retryable: bool,
}

// ---------------------------------------------------------------------------
// Network lexicon types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct AddNetworkLexiconBody {
    pub(super) nsid: String,
    pub(super) target_collection: Option<String>,
}

#[derive(Serialize)]
pub(super) struct NetworkLexiconSummary {
    pub(super) nsid: String,
    pub(super) authority_did: String,
    pub(super) target_collection: Option<String>,
    pub(super) last_fetched_at: Option<String>,
    pub(super) created_at: String,
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
}

#[derive(Serialize)]
pub(super) struct OfficialPluginSummary {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) description: Option<String>,
    pub(super) icon_url: Option<String>,
    pub(super) latest_version: String,
    pub(super) manifest_url: String,
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
}

#[derive(Deserialize)]
pub(super) struct AddPluginBody {
    pub(super) url: String,
    pub(super) sha256: Option<String>,
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
    pub(super) auth_type: String,
    pub(super) required_secrets: Vec<SecretDefinition>,
    pub(super) manifest_url: String,
    pub(super) wasm_url: String,
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
