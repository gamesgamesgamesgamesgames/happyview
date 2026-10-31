export interface ApiClientSummary {
  id: string;
  client_key: string;
  name: string;
  client_id_url: string;
  client_uri: string;
  redirect_uris: string[];
  scopes: string;
  client_type: string;
  allowed_origins: string[] | null;
  rate_limit_capacity: number | null;
  rate_limit_refill_rate: number | null;
  is_active: boolean;
  created_by: string;
  created_at: string;
  updated_at: string;
  parent_client_id: string | null;
  owner_did: string | null;
}

export interface CreateApiClientResponse {
  id: string;
  client_key: string;
  client_secret?: string;
  name: string;
  client_id_url: string;
  client_type: string;
}

export interface ApiClientAuthKey {
  kid: string;
  jwks_uri: string;
}

export interface ApiClientAuthProbe {
  confidential: boolean;
  reason: string;
  checked_at: string;
}

export interface KeyRotationResult {
  kid: string;
  /** `pending` until it takes over at `activates_at`; `current` when switched immediately. */
  status: "pending" | "current";
  activates_at: string | null;
  orphaned_sessions: number;
}

export interface ApiClientAuthKeyListEntry {
  kid: string;
  status: "pending" | "current" | "retiring" | "revoked";
  created_at: string;
  /** When a `pending` key takes over. */
  activates_at: string | null;
  session_count: number;
}

export interface ApiClientAuthKeysResponse {
  keys: ApiClientAuthKeyListEntry[];
  jwks_uri: string;
}

export interface RevokeApiClientAuthKeyResult {
  kid: string;
  sessions_destroyed: number;
}

export interface RevokeAllApiClientAuthKeysResult {
  revoked: number;
  sessions_destroyed: number;
}

/** One of a confidential client's secrets. The secret itself is never listed. */
export interface ApiClientSecret {
  id: string;
  label: string;
  created_at: string;
  last_used_at: string | null;
  revoked_at: string | null;
}

/** A secret just created: the only response that carries the plaintext. */
export interface CreatedApiClientSecret extends ApiClientSecret {
  client_secret: string;
}

export interface RevokeApiClientSecretResult {
  /** False when it had been revoked already. */
  revoked: boolean;
  /** How many of the client's secrets still work. */
  active_secrets: number;
}

export interface InstanceOauthKey {
  kid: string;
  status: "pending" | "current" | "retiring" | "revoked";
  created_at: string;
  /** When a `pending` key takes over. */
  activates_at: string | null;
  session_count: number;
}

export interface InstanceOauthKeysResponse {
  keys: InstanceOauthKey[];
}

export interface RevokeInstanceKeyResult {
  kid: string;
  sessions_destroyed: number;
}
