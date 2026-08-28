export interface StorageAdapter {
  get(key: string): Promise<string | null>;
  set(key: string, value: string): Promise<void>;
  delete(key: string): Promise<void>;
}

export interface SessionEventHooks {
  onSessionUpdate?: (did: string) => void;
  onSessionDelete?: (did: string, cause?: unknown) => void;
}

export interface HappyViewOAuthClientOptions {
  instanceUrl: string;
  clientKey: string;
  clientSecret?: string;
  storage?: StorageAdapter;
  sessionHooks?: SessionEventHooks;
}

export interface DpopProvision {
  provisionId: string;
  dpopKey: JsonWebKey;
}

export interface RegisterSessionParams {
  provisionId: string;
  pkceVerifier?: string;
  did: string;
  accessToken: string;
  refreshToken?: string;
  scopes: string;
  pdsUrl?: string;
  issuer?: string;
  dpopKey: JsonWebKey;
}

/**
 * Session data persisted to storage. Note: tokens and keys are stored as-is
 * via the StorageAdapter. In browser environments using LocalStorageAdapter,
 * this means they are accessible to any JS on the same origin. Consider using
 * a StorageAdapter with encryption if XSS is a concern for your application.
 */
export interface StoredSession {
  did: string;
  dpopKey: JsonWebKey;
  accessToken: string;
  clientKey: string;
  instanceUrl: string;
  scopes?: string;
  pdsUrl?: string;
  issuer?: string;
}

export interface TokenInfo {
  expiresAt?: Date;
  expired?: boolean;
  scope?: string;
  iss?: string;
  aud?: string;
  sub: string;
}

export interface ProvisionKeyResponse {
  provision_id: string;
  dpop_key: JsonWebKey;
}

export interface RegisterSessionResponse {
  session_id: string;
  did: string;
  scopes: string[];
}

export interface GetSessionResponse {
  did: string;
  scopes: string[];
}

export interface ClientAssertionResponse {
  client_assertion: string;
  client_assertion_type: string;
  expires_in: number;
}
