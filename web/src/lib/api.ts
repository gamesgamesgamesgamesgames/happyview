import type { ApiKeySummary, CreateApiKeyResponse } from "@/types/api-keys";
import type { StatsResponse } from "@/types/stats";
import type { LexiconSummary, LexiconDetail } from "@/types/lexicons";
import type { NetworkLexiconSummary } from "@/types/network-lexicons";
import type { BackfillJob, BackfillReposResponse, PdsSummaryResponse } from "@/types/backfill";
import type { UserSummary } from "@/types/users";
import type { AdminListRecordsResponse } from "@/types/records";
import type { EventsListResponse } from "@/types/events";
import type { ScriptVariableSummary } from "@/types/script-variables";
import type { Script, UpsertScriptBody, PatchScriptBody } from "@/types/scripts";
import type { LabelerSummary } from "@/types/labelers";
import type {
  ApiClientSummary,
  CreateApiClientResponse,
} from "@/types/api-clients";
import type { SettingEntry } from "@/types/settings";
import type {
  ExternalProvider,
  LinkedAccount,
  AuthorizeResponse,
  SyncResponse,
  UnlinkResponse,
  ConnectResponse,
} from "@/types/external-accounts";
import type {
  DeadLettersListResponse,
  DeadLetterDetail,
  DeadLetterCountResponse,
  BulkActionResponse,
} from "@/types/dead-letters";

export type { ApiKeySummary, CreateApiKeyResponse } from "@/types/api-keys";
export type { CollectionStat, StatsResponse } from "@/types/stats";
export type { LexiconSummary, LexiconDetail } from "@/types/lexicons";
export type { NetworkLexiconSummary } from "@/types/network-lexicons";
export type { BackfillJob, BackfillRepoEntry, BackfillReposResponse, PdsSummaryEntry, PdsSummaryResponse, BackfillEvent, BlueskyProfile } from "@/types/backfill";
export type { UserSummary } from "@/types/users";
export type { AdminRecord, AdminListRecordsResponse } from "@/types/records";
export type { EventLogEntry, EventsListResponse } from "@/types/events";
export type { ScriptVariableSummary } from "@/types/script-variables";
export type {
  Script,
  ScriptLanguage,
  TriggerKind,
  TriggerFamily,
  UpsertScriptBody,
  PatchScriptBody,
} from "@/types/scripts";
export {
  TRIGGER_KIND_LABELS,
  TRIGGER_FAMILY_LABELS,
  familyOf,
  parseTriggerId,
  DEFAULT_SCRIPT_BODY,
} from "@/types/scripts";
export type { LabelerSummary } from "@/types/labelers";
export type { RecordLabel } from "@/types/records";
export type {
  ApiClientSummary,
  CreateApiClientResponse,
} from "@/types/api-clients";
export type { SettingEntry, InstanceSettings } from "@/types/settings";
export { INSTANCE_SETTING_KEYS } from "@/types/settings";
export type {
  ExternalProvider,
  LinkedAccount,
  AuthorizeResponse,
  SyncResponse,
  UnlinkResponse,
  ConnectResponse,
  ConfigSchema,
  ConfigProperty,
} from "@/types/external-accounts";
export type {
  DeadLetterSummary,
  DeadLetterDetail,
  DeadLettersListResponse,
  DeadLetterCountResponse,
  BulkActionResponse,
} from "@/types/dead-letters";

const BASE_PATH = process.env.NEXT_PUBLIC_BASE_PATH || "";

export class ApiError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

async function apiFetch<T = unknown>(
  path: string,
  options?: RequestInit,
): Promise<T> {
  const headers: Record<string, string> = {};
  if (
    options?.method === "POST" ||
    options?.method === "PUT" ||
    options?.method === "PATCH"
  ) {
    headers["Content-Type"] = "application/json";
  }

  const res = await fetch(`${BASE_PATH}${path}`, {
    ...options,
    headers: { ...headers, ...options?.headers },
    credentials: "same-origin",
  });

  if (!res.ok) {
    const text = await res.text().catch(() => res.statusText);
    throw new ApiError(res.status, text);
  }
  if (res.status === 204) return null as T;
  const text = await res.text();
  if (!text) return null as T;
  return JSON.parse(text);
}

// Stats
export function getStats() {
  return apiFetch<StatsResponse>("/admin/stats");
}

export function getCollections() {
  return apiFetch<{ collections: string[] }>("/admin/records/collections");
}

// Lexicons
export function getLexicons() {
  return apiFetch<LexiconSummary[]>("/admin/lexicons");
}

export function getLexicon(id: string) {
  return apiFetch<LexiconDetail>(`/admin/lexicons/${encodeURIComponent(id)}`);
}

export function uploadLexicon(body: {
  lexicon_json: unknown;
  backfill?: boolean;
  target_collection?: string;
  action?: string;
  token_cost?: number | null;
}) {
  return apiFetch<{ id: string; revision: number }>("/admin/lexicons", {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export function deleteLexicon(id: string) {
  return apiFetch(`/admin/lexicons/${encodeURIComponent(id)}`, {
    method: "DELETE",
  });
}

// Network Lexicons
export function getNetworkLexicons() {
  return apiFetch<NetworkLexiconSummary[]>("/admin/network-lexicons");
}

export function resolveNetworkLexicon(
  nsid: string,
  signal?: AbortSignal,
) {
  return apiFetch<{
    nsid: string;
    authority_did: string;
    type: string | null;
    lexicon_json: Record<string, unknown>;
  }>(`/admin/network-lexicons/resolve/${encodeURIComponent(nsid)}`, { signal });
}

export function addNetworkLexicon(body: {
  nsid: string;
  target_collection?: string;
}) {
  return apiFetch<{ nsid: string; authority_did: string; revision: number }>(
    "/admin/network-lexicons",
    { method: "POST", body: JSON.stringify(body) },
  );
}

export function deleteNetworkLexicon(nsid: string) {
  return apiFetch(`/admin/network-lexicons/${encodeURIComponent(nsid)}`, {
    method: "DELETE",
  });
}

// Backfill
export function getBackfillJobs() {
  return apiFetch<BackfillJob[]>("/admin/backfill/status");
}

export function createBackfillJob(body: { collection?: string; did?: string }) {
  return apiFetch<{ id: string; status: string }>("/admin/backfill", {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export function cancelBackfillJob(id: string) {
  return apiFetch<{ id: string; status: string }>(
    `/admin/backfill/${id}/cancel`,
    { method: "POST" },
  );
}

export function pauseBackfillJob(id: string) {
  return apiFetch<{ id: string; status: string }>(
    `/admin/backfill/${id}/pause`,
    { method: "POST" },
  );
}

export function resumeBackfillJob(id: string) {
  return apiFetch<{ id: string; status: string }>(
    `/admin/backfill/${id}/resume`,
    { method: "POST" },
  );
}

export function getBackfillRepos(
  jobId: string,
  params: { phase?: string; cursor?: string; limit?: number } = {},
) {
  const search = new URLSearchParams();
  if (params.phase) search.set("phase", params.phase);
  if (params.cursor) search.set("cursor", params.cursor);
  if (params.limit) search.set("limit", String(params.limit));
  const qs = search.toString();
  return apiFetch<BackfillReposResponse>(
    `/admin/backfill/${jobId}/repos${qs ? `?${qs}` : ""}`,
  );
}

export function getBackfillPdsSummary(jobId: string) {
  return apiFetch<PdsSummaryResponse>(
    `/admin/backfill/${jobId}/pds-summary`,
  );
}

export function flushBackfillDetails(jobId: string) {
  return apiFetch(`/admin/backfill/${jobId}/details`, { method: "DELETE" });
}

export function flushAllBackfillDetails() {
  return apiFetch(`/admin/backfill/details`, { method: "DELETE" });
}

// Users
export function getUsers() {
  return apiFetch<UserSummary[]>("/admin/users");
}

export function getUser(id: string) {
  return apiFetch<UserSummary>(`/admin/users/${encodeURIComponent(id)}`);
}

export function addUser(body: {
  did: string;
  template?: string;
  permissions?: string[];
}) {
  return apiFetch<{ id: string; did: string }>("/admin/users", {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export function deleteUser(id: string) {
  return apiFetch(`/admin/users/${encodeURIComponent(id)}`, {
    method: "DELETE",
  });
}

export function updateUserPermissions(
  id: string,
  body: { grant?: string[]; revoke?: string[] },
) {
  return apiFetch(`/admin/users/${encodeURIComponent(id)}/permissions`, {
    method: "PATCH",
    body: JSON.stringify(body),
  });
}

export function transferSuper(body: { target_user_id: string }) {
  return apiFetch("/admin/users/transfer-super", {
    method: "POST",
    body: JSON.stringify(body),
  });
}

// Permissions catalog
export type PermissionEntry = {
  key: string;
  name: string;
  description: string;
  category: string;
};

export type PermissionTemplate = {
  key: string;
  label: string;
  permissions: string[];
};

export type PermissionsCatalog = {
  permissions: PermissionEntry[];
  templates: PermissionTemplate[];
};

export function getPermissions() {
  return apiFetch<PermissionsCatalog>("/admin/permissions");
}

// API Keys
export function getApiKeys() {
  return apiFetch<ApiKeySummary[]>("/admin/api-keys");
}

export function createApiKey(body: { name: string; permissions: string[] }) {
  return apiFetch<CreateApiKeyResponse>("/admin/api-keys", {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export function revokeApiKey(id: string) {
  return apiFetch(`/admin/api-keys/${encodeURIComponent(id)}`, {
    method: "DELETE",
  });
}

// XRPC (public, no auth needed)
export async function xrpcQuery<T = unknown>(
  method: string,
  params?: Record<string, string>,
): Promise<T> {
  const search = params ? `?${new URLSearchParams(params)}` : "";
  const res = await fetch(
    `${BASE_PATH}/xrpc/${encodeURIComponent(method)}${search}`,
  );
  if (!res.ok) {
    const text = await res.text().catch(() => res.statusText);
    throw new ApiError(res.status, text);
  }
  return res.json();
}

// Admin records browsing
export function getAdminRecords(
  collection: string,
  limit?: number,
  cursor?: string,
) {
  const params = new URLSearchParams({ collection });
  if (limit) params.set("limit", String(limit));
  if (cursor) params.set("cursor", cursor);
  return apiFetch<AdminListRecordsResponse>(`/admin/records?${params}`);
}

export function deleteRecord(uri: string) {
  return apiFetch(`/admin/records?${new URLSearchParams({ uri })}`, {
    method: "DELETE",
  });
}

export function deleteCollectionRecords(collection: string) {
  return apiFetch<{ deleted: number }>(
    `/admin/records/collection?${new URLSearchParams({ collection })}`,
    { method: "DELETE" },
  );
}

// Script Variables
export function getScriptVariables() {
  return apiFetch<ScriptVariableSummary[]>("/admin/script-variables");
}

export function upsertScriptVariable(body: { key: string; value: string }) {
  return apiFetch("/admin/script-variables", {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export function deleteScriptVariable(key: string) {
  return apiFetch(`/admin/script-variables/${encodeURIComponent(key)}`, {
    method: "DELETE",
  });
}

// Settings
export function getSettings() {
  return apiFetch<SettingEntry[]>("/admin/settings");
}

export type DbInfo = {
  backend: "sqlite" | "postgres";
  server_max_connections: number | null;
  main_pool_size: number;
  backfill_pool_size: number;
  restart_recommended: boolean;
};

export function getDbInfo() {
  return apiFetch<DbInfo>("/admin/settings/db-info");
}

export function upsertSetting(key: string, value: string) {
  return apiFetch(`/admin/settings/${encodeURIComponent(key)}`, {
    method: "PUT",
    body: JSON.stringify({ value }),
  });
}

export function deleteSetting(key: string) {
  return apiFetch(`/admin/settings/${encodeURIComponent(key)}`, {
    method: "DELETE",
  });
}

export async function uploadLogo(file: File) {
  const formData = new FormData();
  formData.append("file", file);
  const res = await fetch(`${BASE_PATH}/admin/settings/logo`, {
    method: "PUT",
    body: formData,
    credentials: "same-origin",
  });
  if (!res.ok) {
    const text = await res.text().catch(() => res.statusText);
    throw new ApiError(res.status, text);
  }
}

export function deleteLogo() {
  return apiFetch("/admin/settings/logo", { method: "DELETE" });
}

// Feature Flags
export type FeatureFlag = {
  key: string;
  name: string;
  description: string;
  enabled: boolean;
};

export function getFeatureFlags() {
  return apiFetch<FeatureFlag[]>("/admin/feature-flags");
}

export function setFeatureFlag(key: string, enabled: boolean) {
  if (enabled) {
    return upsertSetting(key, "true");
  }
  return deleteSetting(key);
}

// Proxy config
export type ProxyConfig = {
  mode: "disabled" | "open" | "allowlist" | "blocklist";
  nsids: string[];
};

export function getProxyConfig() {
  return apiFetch<ProxyConfig>("/admin/settings/xrpc-proxy");
}

export function updateProxyConfig(config: ProxyConfig) {
  return apiFetch("/admin/settings/xrpc-proxy", {
    method: "PUT",
    body: JSON.stringify(config),
  });
}

// Labelers
export function getLabelers() {
  return apiFetch<LabelerSummary[]>("/admin/labelers");
}

export function addLabeler(body: { did: string }) {
  return apiFetch("/admin/labelers", {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export function updateLabeler(did: string, body: { status: string }) {
  return apiFetch(`/admin/labelers/${encodeURIComponent(did)}`, {
    method: "PATCH",
    body: JSON.stringify(body),
  });
}

export function deleteLabeler(did: string) {
  return apiFetch(`/admin/labelers/${encodeURIComponent(did)}`, {
    method: "DELETE",
  });
}

// API Clients
export function getApiClients() {
  return apiFetch<ApiClientSummary[]>("/admin/api-clients");
}

export function getApiClient(id: string) {
  return apiFetch<ApiClientSummary>(
    `/admin/api-clients/${encodeURIComponent(id)}`,
  );
}

export function createApiClient(body: {
  name: string;
  client_id_url: string;
  client_uri: string;
  redirect_uris: string[];
  scopes?: string;
  client_type?: string;
  allowed_origins?: string[];
  rate_limit_capacity: number | null;
  rate_limit_refill_rate: number | null;
}) {
  return apiFetch<CreateApiClientResponse>("/admin/api-clients", {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export function updateApiClient(
  id: string,
  body: {
    name?: string;
    client_uri?: string;
    redirect_uris?: string[];
    scopes?: string;
    allowed_origins?: string[] | null;
    rate_limit_capacity?: number | null;
    rate_limit_refill_rate?: number | null;
    is_active?: boolean;
  },
) {
  return apiFetch(`/admin/api-clients/${encodeURIComponent(id)}`, {
    method: "PUT",
    body: JSON.stringify(body),
  });
}

export function deleteApiClient(id: string) {
  return apiFetch(`/admin/api-clients/${encodeURIComponent(id)}`, {
    method: "DELETE",
  });
}

// Event Logs
export function getEvents(params?: {
  category?: string;
  severity?: string;
  subject?: string;
  cursor?: string;
  limit?: number;
}) {
  const searchParams = new URLSearchParams();
  if (params?.category) searchParams.set("category", params.category);
  if (params?.severity) searchParams.set("severity", params.severity);
  if (params?.subject) searchParams.set("subject", params.subject);
  if (params?.cursor) searchParams.set("cursor", params.cursor);
  if (params?.limit) searchParams.set("limit", String(params.limit));
  const qs = searchParams.toString();
  return apiFetch<EventsListResponse>(`/admin/events${qs ? `?${qs}` : ""}`);
}

// External Accounts
export function getExternalProviders() {
  return apiFetch<ExternalProvider[]>("/external-auth/providers");
}

export function getLinkedAccounts() {
  return apiFetch<LinkedAccount[]>("/external-auth/accounts");
}

export function authorizeExternal(pluginId: string, redirectUri: string) {
  const params = new URLSearchParams({ redirect_uri: redirectUri });
  return apiFetch<AuthorizeResponse>(
    `/external-auth/${encodeURIComponent(pluginId)}/authorize?${params}`,
  );
}

export function syncExternal(pluginId: string) {
  return apiFetch<SyncResponse>(
    `/external-auth/${encodeURIComponent(pluginId)}/sync`,
    { method: "POST" },
  );
}

export function unlinkExternal(pluginId: string) {
  return apiFetch<UnlinkResponse>(
    `/external-auth/${encodeURIComponent(pluginId)}/unlink`,
    { method: "POST" },
  );
}

export function connectWithConfig(
  pluginId: string,
  config: Record<string, unknown>,
) {
  return apiFetch<ConnectResponse>(
    `/external-auth/${encodeURIComponent(pluginId)}/connect`,
    { method: "POST", body: JSON.stringify({ config }) },
  );
}

// Plugins
import type {
  PluginSummary,
  PluginsListResponse,
  OfficialPluginsListResponse,
} from "@/types/plugins";
export type {
  PluginSummary,
  PluginsListResponse,
  OfficialPluginSummary,
  OfficialPluginsListResponse,
  ReleaseEntry,
} from "@/types/plugins";

export function getPlugins() {
  return apiFetch<PluginsListResponse>("/admin/plugins");
}

export function addPlugin(body: { url: string; sha256?: string }) {
  return apiFetch<PluginSummary>("/admin/plugins", {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export function removePlugin(id: string) {
  return apiFetch(`/admin/plugins/${encodeURIComponent(id)}`, {
    method: "DELETE",
  });
}

export function reloadPlugin(id: string, body?: { url?: string }) {
  return apiFetch<PluginSummary>(
    `/admin/plugins/${encodeURIComponent(id)}/reload`,
    {
      method: "POST",
      body: body ? JSON.stringify(body) : undefined,
    },
  );
}

export function getOfficialPlugins() {
  return apiFetch<OfficialPluginsListResponse>("/admin/plugins/official");
}

export function checkPluginUpdate(id: string) {
  return apiFetch<PluginSummary>(
    `/admin/plugins/${encodeURIComponent(id)}/check-update`,
    { method: "POST" },
  );
}

export interface PluginSecretsResponse {
  plugin_id: string;
  secrets: Record<string, string>;
}

export function getPluginSecrets(id: string) {
  return apiFetch<PluginSecretsResponse>(
    `/admin/plugins/${encodeURIComponent(id)}/secrets`,
  );
}

export function updatePluginSecrets(
  id: string,
  secrets: Record<string, string>,
) {
  return apiFetch<void>(`/admin/plugins/${encodeURIComponent(id)}/secrets`, {
    method: "PUT",
    body: JSON.stringify({ secrets }),
  });
}

export interface SecretDefinition {
  key: string;
  name: string;
  description: string | null;
}

export interface PluginPreview {
  id: string;
  name: string;
  version: string;
  description: string | null;
  icon_url: string | null;
  auth_type: string;
  required_secrets: SecretDefinition[];
  manifest_url: string;
  wasm_url: string;
}

export function previewPlugin(url: string, signal?: AbortSignal) {
  return apiFetch<PluginPreview>("/admin/plugins/preview", {
    method: "POST",
    body: JSON.stringify({ url }),
    signal,
  });
}

// ---------------------------------------------------------------------------
// Dead Letters
// ---------------------------------------------------------------------------

export function getDeadLetters(params?: {
  collection?: string;
  resolved?: string;
  cursor?: string;
  limit?: number;
}) {
  const searchParams = new URLSearchParams();
  if (params?.collection) searchParams.set("collection", params.collection);
  if (params?.resolved) searchParams.set("resolved", params.resolved);
  if (params?.cursor) searchParams.set("cursor", params.cursor);
  if (params?.limit) searchParams.set("limit", String(params.limit));
  const qs = searchParams.toString();
  return apiFetch<DeadLettersListResponse>(
    `/admin/dead-letters${qs ? `?${qs}` : ""}`,
  );
}

export function getDeadLetterCount(resolved?: string) {
  const searchParams = new URLSearchParams();
  if (resolved) searchParams.set("resolved", resolved);
  const qs = searchParams.toString();
  return apiFetch<DeadLetterCountResponse>(
    `/admin/dead-letters/count${qs ? `?${qs}` : ""}`,
  );
}

export function getDeadLetter(id: string) {
  return apiFetch<DeadLetterDetail>(
    `/admin/dead-letters/${encodeURIComponent(id)}`,
  );
}

export function retryDeadLetter(id: string) {
  return apiFetch(`/admin/dead-letters/${encodeURIComponent(id)}/retry`, {
    method: "POST",
  });
}

export function reindexDeadLetter(id: string) {
  return apiFetch(`/admin/dead-letters/${encodeURIComponent(id)}/reindex`, {
    method: "POST",
  });
}

export function dismissDeadLetter(id: string) {
  return apiFetch(`/admin/dead-letters/${encodeURIComponent(id)}/dismiss`, {
    method: "POST",
  });
}

export function bulkDismissDeadLetters(body: {
  ids?: string[];
  all?: boolean;
  collection?: string;
}) {
  return apiFetch<BulkActionResponse>("/admin/dead-letters/bulk/dismiss", {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export function bulkRetryDeadLetters(body: {
  ids?: string[];
  all?: boolean;
  collection?: string;
}) {
  return apiFetch<BulkActionResponse>("/admin/dead-letters/bulk/retry", {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export function bulkReindexDeadLetters(body: {
  ids?: string[];
  all?: boolean;
  collection?: string;
}) {
  return apiFetch<BulkActionResponse>("/admin/dead-letters/bulk/reindex", {
    method: "POST",
    body: JSON.stringify(body),
  });
}

// Scripts (trigger-keyed)
export function getScripts(opts?: { suffix?: string }) {
  const params = new URLSearchParams();
  if (opts?.suffix) params.set("suffix", opts.suffix);
  const qs = params.toString();
  return apiFetch<Script[]>(`/admin/scripts${qs ? `?${qs}` : ""}`)
}

export function getScript(id: string) {
  return apiFetch<Script>(`/admin/scripts/${encodeURIComponent(id)}`)
}

export function upsertScript(body: UpsertScriptBody) {
  return apiFetch<Script>("/admin/scripts", {
    method: "POST",
    body: JSON.stringify(body),
  })
}

export function patchScript(id: string, body: PatchScriptBody) {
  return apiFetch<Script>(`/admin/scripts/${encodeURIComponent(id)}`, {
    method: "PATCH",
    body: JSON.stringify(body),
  })
}

export function deleteScript(id: string) {
  return apiFetch(`/admin/scripts/${encodeURIComponent(id)}`, {
    method: "DELETE",
  })
}

// Setup
export interface SetupStatus {
  identity_mode: "did_web" | "did_plc" | "attach_account" | "not_exposed" | null;
  identity_configured: boolean;
  plc_verified: boolean;
  setup_complete: boolean;
}

export interface ServiceIdentityResponse {
  mode: string;
  did: string | null;
  setup_complete: boolean;
  created_at: string;
  updated_at: string;
}

export interface ServiceEntry {
  id: number;
  fragment_id: string;
  service_type: string;
  access_mode: string;
  created_at: string;
  updated_at: string;
}

export function getSetupStatus() {
  return apiFetch<SetupStatus>("/api/setup/status");
}

export function setSetupIdentity(body: {
  mode: string;
  did?: string;
  attached_account_did?: string;
}) {
  return apiFetch("/api/setup/identity", {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export function completeSetup() {
  return apiFetch("/api/setup/complete", { method: "POST" });
}

export interface ResolveResult {
  did: string
  handle: string | null
  display_name: string | null
  avatar: string | null
}

export function resolveIdentity(q: string) {
  return apiFetch<ResolveResult[]>(`/api/setup/resolve?q=${encodeURIComponent(q)}`)
}

export function confirmAttachAuth(body: { original_did: string }) {
  return apiFetch("/api/setup/attach-auth/confirm", {
    method: "POST",
    body: JSON.stringify(body),
  })
}

export function plcRequest() {
  return apiFetch("/api/setup/plc/request", { method: "POST" })
}

export function plcSubmit(token: string) {
  return apiFetch("/api/setup/plc/submit", {
    method: "POST",
    body: JSON.stringify({ token }),
  })
}

export function plcRegister() {
  return apiFetch<{ did: string }>("/api/setup/plc/register", { method: "POST" })
}

// Service Identity
export function getServiceIdentity() {
  return apiFetch<ServiceIdentityResponse | null>("/admin/service-identity");
}

export function updateServiceIdentity(body: {
  mode: string;
  did?: string;
  signing_key_enc?: string;
  rotation_key_enc?: string;
  attached_account_did?: string;
}) {
  return apiFetch("/admin/service-identity", {
    method: "PUT",
    body: JSON.stringify(body),
  });
}

// Service Entries
export function getServiceEntries() {
  return apiFetch<ServiceEntry[]>("/admin/service-entries");
}

export function createServiceEntry(body: {
  fragment_id: string;
  service_type: string;
}) {
  return apiFetch<ServiceEntry>("/admin/service-entries", {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export function updateServiceEntry(
  id: number,
  body: {
    fragment_id?: string;
    service_type?: string;
    access_mode?: string;
  },
) {
  return apiFetch(`/admin/service-entries/${id}`, {
    method: "PUT",
    body: JSON.stringify(body),
  });
}

export function deleteServiceEntry(id: number) {
  return apiFetch(`/admin/service-entries/${id}`, { method: "DELETE" });
}

export function getServiceEntryXrpcs(id: number) {
  return apiFetch<string[]>(`/admin/service-entries/${id}/xrpcs`);
}

export function addServiceEntryXrpcs(id: number, lexicon_ids: string[]) {
  return apiFetch(`/admin/service-entries/${id}/xrpcs`, {
    method: "POST",
    body: JSON.stringify({ lexicon_ids }),
  });
}

export function removeServiceEntryXrpcs(id: number, lexicon_ids: string[]) {
  return apiFetch(`/admin/service-entries/${id}/xrpcs`, {
    method: "DELETE",
    body: JSON.stringify({ lexicon_ids }),
  });
}

export function getLexiconServices(lexiconId: string) {
  return apiFetch<ServiceEntry[]>(
    `/admin/lexicons/${encodeURIComponent(lexiconId)}/services`,
  );
}

// Service Entry PLC Sync
export function syncPlc() {
  return apiFetch("/admin/service-entries/sync-plc", { method: "POST" });
}

export function syncPlcRequest() {
  return apiFetch("/admin/service-entries/sync-plc/request", { method: "POST" });
}

export function syncPlcSubmit(token: string) {
  return apiFetch("/admin/service-entries/sync-plc/submit", {
    method: "POST",
    body: JSON.stringify({ token }),
  });
}
