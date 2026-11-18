export interface StorageAdapter {
  get(key: string): Promise<string | null>;
  set(key: string, value: string): Promise<void>;
  delete(key: string): Promise<void>;
  /** Every key held, when the adapter can list them; used to sweep stale entries. */
  keys?(): Promise<string[]>;
}

export interface SessionEventHooks {
  onSessionUpdate?: (did: string) => void;
  onSessionDelete?: (did: string, cause?: unknown) => void;
}

/**
 * Where a client keeps its DPoP keys as Web Crypto key pairs rather than as
 * JWKs. Keys kept here are imported non-extractable: a session can sign with
 * its key, but nothing, not even this library, can read it back out. The
 * browser client uses IndexedDB, which stores a `CryptoKey` as is.
 */
export interface KeyStore {
  get(id: string): Promise<CryptoKeyPair | null>;
  set(id: string, pair: CryptoKeyPair): Promise<void>;
  delete(id: string): Promise<void>;
  /**
   * Every id held and when it was set (milliseconds), when the store can
   * list them; used to sweep keys left by sign-ins that never finished.
   */
  entries?(): Promise<Array<{ id: string; createdAt: number }>>;
}

export interface HappyViewOAuthClientOptions {
  instanceUrl: string;
  clientKey: string;
  clientSecret?: string;
  storage?: StorageAdapter;
  /**
   * When set, DPoP keys are kept here, non-extractable, and sessions in
   * `storage` hold only the public key. Sessions stored with a JWK before a
   * key store was set move to it the next time they're restored.
   */
  keyStore?: KeyStore;
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
  /** The provisioned key: private, or public when `dpopKeyPair` is given. */
  dpopKey: JsonWebKey;
  /** The provisioned key already imported, for a client with a key store. */
  dpopKeyPair?: CryptoKeyPair;
}

/**
 * Session data persisted to storage. Note: tokens and keys are stored as-is
 * via the StorageAdapter. In browser environments using LocalStorageAdapter,
 * this means they are accessible to any JS on the same origin. Consider using
 * a StorageAdapter with encryption if XSS is a concern for your application.
 */
export interface StoredSession {
  did: string;
  /** The private DPoP key, for a client without a key store. */
  dpopKey?: JsonWebKey;
  /** The public DPoP key, when the private one is in the key store. */
  dpopPublicKey?: JsonWebKey;
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
  confidential?: boolean;
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
