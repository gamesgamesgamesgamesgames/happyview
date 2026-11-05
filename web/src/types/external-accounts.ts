export interface ExternalProvider {
  id: string;
  name: string;
  icon_url: string | null;
  auth_type: "oauth2" | "openid" | "api_key";
  config_schema?: ConfigSchema;
}

/** JSON Schema for plugin configuration */
export interface ConfigSchema {
  type: "object";
  required?: string[];
  properties: Record<string, ConfigProperty>;
}

export interface ConfigProperty {
  type: "string" | "number" | "boolean";
  title?: string;
  description?: string;
  format?: "password" | "uri" | "email";
  default?: unknown;
}

export interface LinkedAccount {
  plugin_id: string;
  account_id: string;
  created_at: string;
  updated_at: string;
  expires_at: string | null;
}

export interface RefreshResponse {
  refreshed: boolean;
  expires_at: string | null;
}

export interface AuthorizeResponse {
  authorize_url: string;
  state: string;
}

export interface UnlinkResponse {
  status: string;
  was_linked: boolean;
}

export interface ConnectResponse {
  status: string;
  account_id: string;
  display_name: string | null;
}
