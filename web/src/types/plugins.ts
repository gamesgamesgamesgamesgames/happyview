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

/** Where a publisher has put a version. `active` and `deprecated` may be
 * installed; `unavailable` and `retired` may not. */
export type LifecycleState =
  "active" | "deprecated" | "unavailable" | "retired";

/** Whether the registry holds a version's bytes. */
export type Availability = "pending" | "available" | "deletedAtSource";

export interface ReleaseEntry {
  version: string;
  name: string;
  published_at: string | null;
  /** The version's changelog, as markdown. */
  body: string | null;
  state: LifecycleState;
  /** Why the publisher set `state`, when they said. */
  reason: string | null;
  /** The registry saw this version's records change after publication. */
  suspect: boolean;
  /** Its state allows it, the registry holds its bytes and it isn't
   * suspect. A version that may not be installed is still listed. */
  installable: boolean;
  availability: Availability;
  version_uri: string;
  manifest_cid: string;
  package_cid: string;
  manifest_blob_cid: string;
}

/** Where one of a project's channels points. */
export interface ChannelEntry {
  version: string;
  state: LifecycleState;
  installable: boolean;
  availability: Availability;
}

/** Where an installed plugin stands in the registry. */
export interface PluginRegistryStatus {
  /** `null` for a URL install no registry project matched. */
  did: string | null;
  name: string | null;
  /** The channel the install follows; `null` when pinned to a version. */
  channel: string | null;
  version_uri: string | null;
  /** `null` when the catalogue doesn't hold the project. */
  installed_state: LifecycleState | null;
  installed_reason: string | null;
  /** The state of the version the followed channel points at. */
  channel_state: LifecycleState | null;
  /** A URL install that no registry project matched. */
  unresolved: boolean;
}

export interface PluginSummary {
  id: string;
  name: string;
  version: string;
  /** What a surface shows before it shows what the plugin does. Each is
   * independently optional: a plugin may carry an icon, a banner, both, or
   * neither. */
  description: string | null;
  icon_url: string | null;
  banner_url: string | null;
  source: "file" | "url";
  url: string | null;
  sha256: string | null;
  enabled: boolean;
  auth_type: string;
  required_secrets: SecretDefinition[];
  secrets_configured: boolean;
  loaded_at: string | null;
  update_available: boolean;
  /** Where the followed channel points; absent when the catalogue has
   * nothing to say about this plugin. */
  latest_version?: string | null;
  pending_releases: ReleaseEntry[];
  /** Whether `latest_version` may be installed. A channel can move to a
   * version that may not be, and then nothing is offered. */
  latest_version_installable?: boolean;
  latest_state?: LifecycleState;
  /** `null` for a plugin installed from a file, or from a URL that was never
   * checked against the registry. */
  registry: PluginRegistryStatus | null;
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
  /** The project name. With `publisher_did`, what an install names. */
  id: string;
  publisher_did: string | null;
  name: string;
  description: string | null;
  icon_url: string | null;
  plugin_type: PluginType;
  /** The version `latest` points at; `null` when it points nowhere. */
  latest_version: string | null;
  /** Every channel the project publishes, by name. */
  channels: Record<string, ChannelEntry>;
  /** The project's own lifecycle state. */
  lifecycle_state: LifecycleState;
  official: boolean;
  banner_url: string | null;
  /** Every release the catalogue knows, newest first. */
  releases: ReleaseEntry[];
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
