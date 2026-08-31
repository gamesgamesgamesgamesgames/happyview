import type { Key } from "@atproto/jwk";
import { ApiError } from "./errors";
import { importJwk } from "./import-jwk";
import { HappyViewSession } from "./session";
import { MemoryStorage } from "./storage";
import type {
  ClientAssertionResponse,
  GetSessionResponse,
  HappyViewOAuthClientOptions,
  ProvisionKeyResponse,
  RegisterSessionParams,
  RegisterSessionResponse,
  SessionEventHooks,
  StorageAdapter,
  StoredSession,
} from "./types";

/// The storage keys this client reads and writes. Exported because a consumer
/// recovering a wedged session by hand would otherwise have to hardcode them,
/// which silently breaks the day they are renamed.
export const STORAGE_PREFIX = "happyview:session:";
export const LAST_ACTIVE_KEY = "happyview:last-active-did";

export interface FetchMetadataOptions {
  clientId: string;
  fetch?: typeof globalThis.fetch;
  signal?: AbortSignal;
}

export class HappyViewOAuthClient {
  static async fetchMetadata({
    clientId,
    fetch: fetchFn = globalThis.fetch,
    signal,
  }: FetchMetadataOptions): Promise<Record<string, unknown>> {
    signal?.throwIfAborted();
    const resp = await fetchFn(clientId, { redirect: "error", signal });
    if (resp.status !== 200) {
      resp.body?.cancel?.();
      throw new Error(`Failed to fetch client metadata: ${resp.status}`);
    }
    const mime = resp.headers.get("content-type")?.split(";")[0].trim();
    if (mime !== "application/json") {
      resp.body?.cancel?.();
      throw new Error(`Invalid client metadata content type: ${mime}`);
    }
    return resp.json() as Promise<Record<string, unknown>>;
  }

  protected readonly instanceUrl: string;
  protected readonly clientKey: string;
  protected readonly storage: StorageAdapter;
  private readonly clientSecret: string | undefined;
  protected readonly _fetch: typeof globalThis.fetch;
  private readonly sessionHooks: SessionEventHooks;

  constructor(
    options: HappyViewOAuthClientOptions & {
      fetch?: typeof globalThis.fetch;
    },
  ) {
    this.instanceUrl = options.instanceUrl.replace(/\/+$/, "");
    this.clientKey = options.clientKey;
    this.clientSecret = options.clientSecret;
    this.storage = options.storage ?? new MemoryStorage();
    this._fetch =
      options.fetch ??
      (((input: RequestInfo | URL, init?: RequestInit) =>
        fetch(input, init)) as typeof globalThis.fetch);
    this.sessionHooks = options.sessionHooks ?? {};
  }

  get isConfidential(): boolean {
    return this.clientSecret !== undefined;
  }

  async provisionDpopKey(): Promise<{
    provisionId: string;
    dpopKey: Key;
    rawJwk: JsonWebKey;
    pkceVerifier?: string;
  }> {
    const headers: Record<string, string> = {
      "content-type": "application/json",
      "x-client-key": this.clientKey,
    };
    if (this.clientSecret) {
      headers["x-client-secret"] = this.clientSecret;
    }

    const body: Record<string, unknown> = {};
    let pkceVerifier: string | undefined;

    if (!this.isConfidential) {
      pkceVerifier = generatePkceVerifier();
      const challenge = await computePkceChallenge(pkceVerifier);
      body.pkce_challenge = challenge;
    }

    const resp = await this._fetch(`${this.instanceUrl}/oauth/dpop-keys`, {
      method: "POST",
      headers,
      body: JSON.stringify(body),
    });

    if (!resp.ok) {
      const body = await resp.json().catch(() => ({}));
      throw new ApiError(
        `Failed to provision DPoP key: ${resp.status} ${(body as any).error ?? (body as any).message ?? resp.statusText}`,
        resp.status,
        body,
      );
    }

    const data: ProvisionKeyResponse = await resp.json();
    const dpopKey = await importJwk(data.dpop_key);
    return {
      provisionId: data.provision_id,
      dpopKey,
      rawJwk: data.dpop_key,
      pkceVerifier,
    };
  }

  /**
   * Mint a `private_key_jwt` client assertion for the OAuth flow against a
   * user's PDS.
   */
  async getClientAssertion(
    issuer: string,
  ): Promise<{ clientAssertion: string; clientAssertionType: string }> {
    const headers: Record<string, string> = {
      "content-type": "application/json",
      "x-client-key": this.clientKey,
    };
    if (this.clientSecret) {
      headers["x-client-secret"] = this.clientSecret;
    }

    const resp = await this._fetch(
      `${this.instanceUrl}/oauth/client-assertion`,
      {
        method: "POST",
        headers,
        body: JSON.stringify({ issuer }),
      },
    );

    if (!resp.ok) {
      const body = await resp.json().catch(() => ({}));
      throw new ApiError(
        `Failed to mint client assertion: ${resp.status} ${(body as any).error ?? (body as any).message ?? resp.statusText}`,
        resp.status,
        body,
      );
    }

    const data: ClientAssertionResponse = await resp.json();
    return {
      clientAssertion: data.client_assertion,
      clientAssertionType: data.client_assertion_type,
    };
  }

  async registerSession(
    params: RegisterSessionParams,
  ): Promise<HappyViewSession> {
    const headers: Record<string, string> = {
      "content-type": "application/json",
      "x-client-key": this.clientKey,
    };
    if (this.clientSecret) {
      headers["x-client-secret"] = this.clientSecret;
    }

    const body: Record<string, unknown> = {
      provision_id: params.provisionId,
      did: params.did,
      access_token: params.accessToken,
      refresh_token: params.refreshToken,
      scopes: params.scopes,
      pds_url: params.pdsUrl,
      issuer: params.issuer,
    };

    if (!this.isConfidential && params.pkceVerifier) {
      body.pkce_verifier = params.pkceVerifier;
    }

    const resp = await this._fetch(`${this.instanceUrl}/oauth/sessions`, {
      method: "POST",
      headers,
      body: JSON.stringify(body),
    });

    if (!resp.ok) {
      const body = await resp.json().catch(() => ({}));
      throw new ApiError(
        `Failed to register session: ${resp.status} ${(body as any).error ?? (body as any).message ?? resp.statusText}`,
        resp.status,
        body,
      );
    }

    const data: RegisterSessionResponse = await resp.json();

    await this.retirePreviousSession(data.did, params.dpopKey);

    const dpopKey = await importJwk(params.dpopKey);

    const storedSession: StoredSession = {
      did: data.did,
      dpopKey: params.dpopKey,
      accessToken: params.accessToken,
      clientKey: this.clientKey,
      instanceUrl: this.instanceUrl,
      scopes: params.scopes,
      pdsUrl: params.pdsUrl,
      issuer: params.issuer,
    };
    await this.storage.set(
      `${STORAGE_PREFIX}${data.did}`,
      JSON.stringify(storedSession),
    );
    await this.storage.set(LAST_ACTIVE_KEY, data.did);

    const session = new HappyViewSession({
      did: data.did,
      dpopKey,
      accessToken: params.accessToken,
      clientKey: this.clientKey,
      instanceUrl: this.instanceUrl,
      scopes: params.scopes,
      pdsUrl: params.pdsUrl,
      issuer: params.issuer,
      fetch: this._fetch,
      onSignOut: () => this.deleteSession(data.did),
    });

    this.sessionHooks.onSessionUpdate?.(data.did);

    return session;
  }

  /**
   * Revoke this client's previous session for `did`, when the login that just
   * completed replaced it.
   */
  private async retirePreviousSession(
    did: string,
    newDpopKey: JsonWebKey,
  ): Promise<void> {
    try {
      const raw = await this.storage.get(`${STORAGE_PREFIX}${did}`);
      if (!raw) return;

      const previous = JSON.parse(raw) as StoredSession;

      if (
        typeof newDpopKey.x === "string" &&
        previous.dpopKey?.x === newDpopKey.x &&
        previous.dpopKey?.y === newDpopKey.y
      ) {
        return;
      }

      const session = await this.restoreSession(did);
      if (!session) return;

      await session.fetchHandler(`${this.instanceUrl}/oauth/sessions/${did}`, {
        method: "DELETE",
      });
    } catch {
      // Best effort — see above.
    }
  }

  /**
   * Revoke the session server-side and clear it locally.
   *
   * The local cleanup runs unconditionally. If it did not, a server that
   * refuses the revocation would leave the session in storage, `restore()`
   * would sign the user back in on the next load, and the next logout would
   * fail identically — a loop with no exit through this API.
   */
  async deleteSession(did: string): Promise<void> {
    try {
      const session = await this.restoreSession(did);
      if (session) {
        const resp = await session.fetchHandler(
          `${this.instanceUrl}/oauth/sessions/${did}`,
          { method: "DELETE" },
        );

        // 404: already gone. 401/403: the credential is itself invalid, so
        // there is no live session left to revoke. None of these is a logout
        // that failed, so none of them should be reported as one.
        const alreadyUnusable =
          resp.status === 404 || resp.status === 401 || resp.status === 403;

        if (!resp.ok && !alreadyUnusable) {
          const body = await resp.json().catch(() => ({}));
          throw new ApiError(
            `Failed to delete session: ${resp.status} ${(body as any).error ?? (body as any).message ?? resp.statusText}`,
            resp.status,
            body,
          );
        }
      }
    } finally {
      // 5xx and network errors still throw — the server may genuinely still
      // hold a live session and the caller deserves to know. They throw from
      // here, after the local state is gone, so the user is logged out either
      // way.
      await this.forgetSession(did);
    }
  }

  /**
   * Clear a session locally without contacting the server.
   *
   * The escape hatch for a session that cannot be revoked remotely — a revoked
   * or expired credential, an unreachable instance. Nothing is revoked, so the
   * server may still consider the session live until it expires.
   */
  async forgetSession(did: string): Promise<void> {
    await this.storage.delete(`${STORAGE_PREFIX}${did}`);

    const lastActive = await this.storage.get(LAST_ACTIVE_KEY);
    if (lastActive === did) {
      await this.storage.delete(LAST_ACTIVE_KEY);
    }

    this.sessionHooks.onSessionDelete?.(did);
  }

  async restoreSession(did: string): Promise<HappyViewSession | null> {
    const stored = await this.storage.get(`${STORAGE_PREFIX}${did}`);
    if (!stored) return null;

    const data: StoredSession = JSON.parse(stored);
    const dpopKey = await importJwk(data.dpopKey);
    return new HappyViewSession({
      did: data.did,
      dpopKey,
      accessToken: data.accessToken,
      clientKey: data.clientKey,
      instanceUrl: data.instanceUrl,
      scopes: data.scopes,
      pdsUrl: data.pdsUrl,
      issuer: data.issuer,
      fetch: this._fetch,
      onSignOut: () => this.deleteSession(data.did),
    });
  }

  async getSession(did: string): Promise<GetSessionResponse> {
    const session = await this.restoreSession(did);
    if (!session) {
      throw new ApiError("No stored session for this DID", 404, {});
    }

    const resp = await session.fetchHandler(
      `${this.instanceUrl}/oauth/sessions/${did}`,
      { method: "GET" },
    );

    if (!resp.ok) {
      const body = await resp.json().catch(() => ({}));
      throw new ApiError(
        `Failed to get session: ${resp.status} ${(body as any).error ?? (body as any).message ?? resp.statusText}`,
        resp.status,
        body,
      );
    }

    return resp.json();
  }

  async restore(): Promise<HappyViewSession | null> {
    const did = await this.storage.get(LAST_ACTIVE_KEY);
    if (!did) return null;
    return this.restoreSession(did);
  }
}

// PKCE helpers — inline since they're just a few lines of Web Crypto
function generatePkceVerifier(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  let binary = "";
  for (let i = 0; i < bytes.length; i++) {
    binary += String.fromCharCode(bytes[i]);
  }
  return btoa(binary)
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
}

async function computePkceChallenge(verifier: string): Promise<string> {
  const hash = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(verifier),
  );
  const bytes = new Uint8Array(hash);
  let binary = "";
  for (let i = 0; i < bytes.length; i++) {
    binary += String.fromCharCode(bytes[i]);
  }
  return btoa(binary)
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
}
