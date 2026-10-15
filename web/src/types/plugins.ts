export interface SecretDefinition {
  key: string;
  name: string;
  description: string | null;
}

export type CapabilityRisk = "low" | "medium" | "high" | "critical";

export interface CapabilityEntry {
  name: string;
  risk: CapabilityRisk;
  description: string;
}

export interface CapabilityReport {
  declared: CapabilityEntry[];
  required_by_imports: CapabilityEntry[];
  undeclared: string[];
}

export interface PluginDependency {
  id: string;
  version: string;
}

export type PluginType = "auth" | "interpreter" | "library";

export interface ReleaseEntry {
  version: string;
  name: string;
  published_at: string;
  body: string;
}

export interface PluginSummary {
  id: string;
  name: string;
  version: string;
  source: "file" | "url";
  url: string | null;
  sha256: string | null;
  enabled: boolean;
  auth_type: string;
  required_secrets: SecretDefinition[];
  secrets_configured: boolean;
  loaded_at: string | null;
  update_available: boolean;
  latest_version: string | null;
  pending_releases: ReleaseEntry[];
  /** The manifest an update installs; absent when no update is offered. */
  latest_manifest_url?: string;
  plugin_type: PluginType;
  namespace: string | null;
  dependencies: PluginDependency[];
  allowed_hosts: string[];
  capabilities: CapabilityReport;
}

export interface PluginsListResponse {
  plugins: PluginSummary[];
  encryption_configured: boolean;
}

export interface OfficialPluginSummary {
  id: string;
  name: string;
  description: string | null;
  icon_url: string | null;
  latest_version: string;
  manifest_url: string;
  official: boolean;
  /** The publisher's long-form prose. Every field is optional, and the whole
   * object is empty for a GitHub-sourced entry, which has no profile record
   * behind it. */
  sections: PluginSections;
}

/** The four long-form fields of `at.happyproto.plugin.profile`. CommonMark
 * from a publisher, so a renderer strips embedded HTML and refuses a link
 * scheme it does not recognise. */
export interface PluginSections {
  description?: string;
  installation?: string;
  changelog?: string;
  security?: string;
}

export interface OfficialPluginsListResponse {
  plugins: OfficialPluginSummary[];
  last_refreshed_at: string | null;
}
