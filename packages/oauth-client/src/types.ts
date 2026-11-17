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
  /**
   * A key proof pinning a key the client holds to this sign-in
   * (`hv-key-proof+jwt` with `lxm` {@link SESSION_PIN_LXM} and `dpop_jkt` the
   * provisioned key's thumbprint). The instance's caller assertions for the
   * sign-in can then name that key, and only that one.
   */
  ticketKeyProof?: string;
}

/** The `lxm` of the key proof that pins a key to a sign-in. */
export const SESSION_PIN_LXM = "happyview.oauth.registerSession";

/** What a {@link TicketKeyProver} is asked to prove a key for. */
export interface TicketKeyProofRequest {
  did: string;
  /** The RFC 7638 thumbprint of the sign-in's DPoP key: the proof's `dpop_jkt`. */
  dpopJkt: string;
  /** The instance: the proof's `aud`. */
  audience: string;
  /** The proof's `lxm`. */
  lxm: string;
}

/** Signs a key proof for a sign-in; `undefined` pins nothing. */
export type TicketKeyProver = (request: TicketKeyProofRequest) => Promise<string | undefined>;

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
  /** With a `ticketKeyProof`: whether the instance pinned the key. */
  ticket_pin?: "pinned" | "refused";
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
