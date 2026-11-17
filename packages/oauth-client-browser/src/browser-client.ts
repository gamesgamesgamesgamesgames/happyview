import { AtprotoDohHandleResolver } from "@atproto-labs/handle-resolver";
import { DidResolverCommon } from "@atproto-labs/did-resolver";
import { isDid, type Did, type DidDocument } from "@atproto/did";
import {
  HappyViewOAuthClient,
  HappyViewSession,
  LAST_ACTIVE_KEY,
  importJwk,
  jwkThumbprint,
  keyFromPair,
  publicJwkOf,
  InvalidStateError,
  OAuthCallbackError,
  ResolutionError,
  TokenExchangeError,
  type Key,
  type KeyStore,
  SESSION_PIN_LXM,
  type SessionEventHooks,
  type TicketKeyProver,
  type StorageAdapter,
} from "@happyview/oauth-client";
import { IndexedDbKeyStore } from "./indexeddb-key-store";
import { LocalStorageAdapter } from "./local-storage-adapter";

const NAMESPACE = "@happyview/oauth-client-browser";
const POPUP_CHANNEL_NAME = `${NAMESPACE}(popup-channel)`;
const POPUP_STATE_PREFIX = `${NAMESPACE}(popup-state):`;

export class LoginContinuedInParentWindowError extends Error {
  constructor() {
    super("Login continued in parent window");
    this.name = "LoginContinuedInParentWindowError";
  }
}

export interface HappyViewBrowserClientOptions {
  instanceUrl: string;
  clientId: string;
  clientKey: string;
  redirectUri?: string;
  scopes?: string;
  storage?: StorageAdapter;
  /**
   * Where DPoP keys are kept, non-extractable. Defaults to IndexedDB where
   * the browser has it. Pass `null` to keep keys as JWKs in `storage`, as
   * before.
   */
  keyStore?: KeyStore | null;
  /**
   * Called as a sign-in completes, to pin a key the app holds (a ticket key
   * it made before sign-in) to the new session. Its proof goes with the
   * session's registration.
   */
  ticketKeyProof?: TicketKeyProver;
  sessionHooks?: SessionEventHooks;
  fetch?: typeof globalThis.fetch;
}

/** The key store id of the key for a sign-in still in progress. */
const pendingKeyId = (state: string) => `pending:${state}`;
const PENDING_AUTH_PREFIX = "pending-auth:";

/** A sign-in not finished in this long is abandoned, and swept. */
export const PENDING_SIGN_IN_TTL_MS = 60 * 60 * 1000;

interface PendingAuthState {
  did: string;
  provisionId: string;
  /** The provisioned key, when there's no key store. */
  rawJwk?: JsonWebKey;
  /** Its public half, when the private key waits in the key store. */
  publicJwk?: JsonWebKey;
  /** When the sign-in started (milliseconds); older ones are swept. */
  createdAt?: number;
  provisionPkceVerifier: string;
  authPkceVerifier: string;
  pdsUrl: string;
  tokenEndpoint: string;
  state: string;
  issuer: string;
  confidential: boolean;
}

export interface LoginOptions {
  scope?: string;
  /** @deprecated Use `scope` instead. */
  scopes?: string;
  state?: string;
  redirect_uri?: string;
  signal?: AbortSignal;
  display?: "page" | "popup" | "touch" | "wap";
  prompt?: string;
  nonce?: string;
  max_age?: number;
  ui_locales?: string;
  dpop_jkt?: string;
  claims?: Record<string, Record<string, null | Record<string, unknown>>>;
  authorization_details?: unknown[];
  id_token_hint?: string;
}

export interface PopupLoginOptions extends LoginOptions {
  popupName?: string;
  popupFeatures?: string;
}

export interface SignInOptions extends LoginOptions {
  display?: "popup" | "page";
  popupName?: string;
  popupFeatures?: string;
}

export interface PrepareLoginResult {
  authorizationUrl: string;
  did: string;
  state: string;
}

interface AuthServerMetadata {
  issuer: string;
  authorization_endpoint: string;
  token_endpoint: string;
  pushed_authorization_request_endpoint?: string;
  dpop_signing_alg_values_supported?: string[];
}

export class HappyViewBrowserClient extends HappyViewOAuthClient {
  readonly handleResolver: AtprotoDohHandleResolver;
  readonly didResolver: DidResolverCommon;
  private readonly clientId: string;
  private readonly redirectUri: string | undefined;
  private readonly scopes: string;
  private readonly ticketKeyProver: TicketKeyProver | undefined;
  constructor(options: HappyViewBrowserClientOptions) {
    const fetchFn =
      options.fetch ??
      (((input: RequestInfo | URL, init?: RequestInit) =>
        fetch(input, init)) as typeof globalThis.fetch);
    const storageAdapter = options.storage ?? new LocalStorageAdapter();
    const keyStore =
      options.keyStore === undefined
        ? IndexedDbKeyStore.isAvailable()
          ? new IndexedDbKeyStore()
          : undefined
        : (options.keyStore ?? undefined);
    super({
      instanceUrl: options.instanceUrl,
      clientKey: options.clientKey,
      storage: storageAdapter,
      keyStore,
      sessionHooks: options.sessionHooks,
      fetch: fetchFn,
    });

    this.clientId = options.clientId;
    this.redirectUri = options.redirectUri;
    this.scopes = options.scopes ?? "atproto";
    this.ticketKeyProver = options.ticketKeyProof;
    this.handleResolver = new AtprotoDohHandleResolver({
      dohEndpoint: "https://dns.google/resolve",
      fetch: fetchFn,
    });
    this.didResolver = new DidResolverCommon({ fetch: fetchFn });
    // Best effort, in the background: nothing waits on it.
    this.sweepPendingSignIns().catch(() => undefined);
  }

  /**
   * Remove what sign-ins that never finished left behind: their keys in the
   * key store and their pending state in storage, once older than
   * `PENDING_SIGN_IN_TTL_MS`. Runs when the client is made; exposed so a
   * caller can run it again.
   */
  async sweepPendingSignIns(now: number = Date.now()): Promise<void> {
    const cutoff = now - PENDING_SIGN_IN_TTL_MS;
    const stale = new Set<string>();
    for (const { id, createdAt } of (await this.keyStore?.entries?.()) ?? []) {
      if (id.startsWith("pending:") && createdAt < cutoff) {
        stale.add(id.slice("pending:".length));
      }
    }
    for (const key of (await this.storage.keys?.()) ?? []) {
      if (!key.startsWith(PENDING_AUTH_PREFIX)) continue;
      const state = key.slice(PENDING_AUTH_PREFIX.length);
      const raw = await this.storage.get(key);
      let pending: PendingAuthState | undefined;
      try {
        pending = raw ? (JSON.parse(raw) as PendingAuthState) : undefined;
      } catch {
        pending = undefined;
      }
      if (!pending) {
        stale.add(state);
      } else if (pending.createdAt === undefined) {
        // From before sign-ins were timed: start its clock now.
        await this.storage.set(key, JSON.stringify({ ...pending, createdAt: now }));
      } else if (pending.createdAt < cutoff) {
        stale.add(state);
      }
    }
    for (const state of stale) {
      await this.storage.delete(`${PENDING_AUTH_PREFIX}${state}`);
      await this.keyStore?.delete(pendingKeyId(state));
    }
  }

  async prepareLogin(
    identifier: string,
    options?: LoginOptions,
  ): Promise<PrepareLoginResult> {
    // Resolve identifier → DID → DID document → PDS URL → auth server metadata.
    // A DID is already the resolution target: sending it to the handle resolver
    // would look up a `_atproto.did:plc:…` TXT record, which cannot exist.
    let resolvedDid: Did;
    if (isDid(identifier)) {
      resolvedDid = identifier;
    } else {
      const handleDid = await this.handleResolver.resolve(identifier);
      if (!handleDid) {
        throw new ResolutionError(`Failed to resolve handle: ${identifier}`);
      }
      resolvedDid = handleDid;
    }
    const did = resolvedDid as string;

    const didDoc = await this.didResolver.resolve(resolvedDid);
    const pdsUrl = extractPdsUrl(didDoc);
    const authMeta = await this.fetchAuthServerMetadata(pdsUrl);

    const scopes = options?.scope ?? options?.scopes ?? this.scopes;

    // Provision DPoP key from HappyView. The response also says, authoritatively,
    // whether this client must authenticate to the PDS as a confidential atproto
    // client — the deterministic signal we attach assertions on, below.
    const {
      provisionId,
      rawJwk,
      dpopKey: provisionedKey,
      dpopKeyPair,
      pkceVerifier: provisionPkceVerifier,
      confidential,
    } = await this.provisionDpopKey();

    // Separate PKCE for the PDS authorization server
    const authPkceVerifier = generatePkceVerifier();
    const authPkceChallenge = await computePkceChallenge(authPkceVerifier);

    const state = options?.state ?? randomHex(16);

    // With a key store, the private key waits there, non-extractable, for the
    // redirect back; storage only ever sees the public half.
    // provisionDpopKey imported it non-extractable already.
    let keyFields: Pick<PendingAuthState, "rawJwk" | "publicJwk">;
    const signingKey: Key = provisionedKey;
    if (this.keyStore && dpopKeyPair) {
      await this.keyStore.set(pendingKeyId(state), dpopKeyPair);
      keyFields = { publicJwk: publicJwkOf(rawJwk) };
    } else {
      keyFields = { rawJwk };
    }

    const pendingState: PendingAuthState = {
      did,
      provisionId,
      createdAt: Date.now(),
      ...keyFields,
      provisionPkceVerifier: provisionPkceVerifier!,
      authPkceVerifier,
      pdsUrl,
      tokenEndpoint: authMeta.token_endpoint,
      state,
      issuer: authMeta.issuer,
      confidential,
    };
    await this.storage.set(
      `pending-auth:${state}`,
      JSON.stringify(pendingState),
    );

    const { clientId, redirectUri: defaultRedirectUri } =
      this.resolveOAuthEndpoints();
    const redirectUri = options?.redirect_uri ?? defaultRedirectUri;

    const authParams = new URLSearchParams({
      response_type: "code",
      client_id: clientId,
      redirect_uri: redirectUri,
      state,
      scope: scopes,
      code_challenge: authPkceChallenge,
      code_challenge_method: "S256",
      login_hint: identifier,
    });

    if (options?.display) authParams.set("display", options.display);
    if (options?.prompt) authParams.set("prompt", options.prompt);
    if (options?.nonce) authParams.set("nonce", options.nonce);
    if (options?.max_age != null)
      authParams.set("max_age", String(options.max_age));
    if (options?.ui_locales) authParams.set("ui_locales", options.ui_locales);
    // ⚠ THE AUTHORIZATION REQUEST MUST BE BOUND TO THE DPoP KEY, and this client
    // is the only thing that can do it — `provisionDpopKey` above returned the
    // very key that signs the token-endpoint proof in `callback`, and it never
    // leaves the SDK. Sent unbound, the PAR carries neither a `DPoP` header nor
    // `dpop_jkt`; bsky's PDS tolerates that, but the tolerance is deprecated
    // (https://atproto.com/blog/oauth-improvements#deprecation-notice) and other
    // implementations reject the request outright. A caller-supplied `dpop_jkt`
    // still wins, for anyone driving the key themselves.
    const provisionedJkt = await jwkThumbprint(rawJwk);
    authParams.set("dpop_jkt", options?.dpop_jkt ?? provisionedJkt);
    if (options?.id_token_hint)
      authParams.set("id_token_hint", options.id_token_hint);
    if (options?.claims)
      authParams.set("claims", JSON.stringify(options.claims));
    if (options?.authorization_details)
      authParams.set(
        "authorization_details",
        JSON.stringify(options.authorization_details),
      );

    if (confidential) {
      const { clientAssertion, clientAssertionType } =
        await this.getClientAssertion(authMeta.issuer, {
          provisionId,
          pkceVerifier: provisionPkceVerifier!,
        });
      authParams.set("client_assertion", clientAssertion);
      authParams.set("client_assertion_type", clientAssertionType);
    }

    // ATProto requires Pushed Authorization Requests (PAR)
    const parEndpoint = authMeta.pushed_authorization_request_endpoint;
    if (parEndpoint) {
      const signProof =
        authParams.get("dpop_jkt") === provisionedJkt
          ? buildProofSigner(signingKey, publicJwkOf(rawJwk), parEndpoint)
          : undefined;

      let dpopNonce: string | undefined;
      let parResp!: Response;

      for (let attempt = 0; attempt < 2; attempt++) {
        const headers: Record<string, string> = {
          "content-type": "application/x-www-form-urlencoded",
        };
        if (signProof) headers.dpop = await signProof(dpopNonce);

        parResp = await this._fetch(parEndpoint, {
          method: "POST",
          headers,
          body: authParams,
        });

        if (!parResp.ok && attempt === 0 && signProof) {
          const nonceHeader = parResp.headers.get("dpop-nonce");
          if (nonceHeader) {
            const errorBody = await parResp.text();
            if (errorBody.includes("use_dpop_nonce")) {
              dpopNonce = nonceHeader;
              continue;
            }
            throw new ResolutionError(
              `PAR request failed: ${parResp.status} ${errorBody}`,
            );
          }
        }

        break;
      }

      if (!parResp.ok) {
        const err = await parResp.text();
        throw new ResolutionError(
          `PAR request failed: ${parResp.status} ${err}`,
        );
      }

      const parData = (await parResp.json()) as { request_uri: string };
      const authorizationUrl =
        `${authMeta.authorization_endpoint}?` +
        new URLSearchParams({
          client_id: clientId,
          request_uri: parData.request_uri,
        });

      return { authorizationUrl, did, state };
    }

    // Fallback: direct authorization URL (for servers that don't require PAR)
    const authorizationUrl = `${authMeta.authorization_endpoint}?${authParams}`;

    return { authorizationUrl, did, state };
  }

  async login(identifier: string, options?: LoginOptions): Promise<void> {
    const { authorizationUrl } = await this.prepareLogin(identifier, options);
    window.location.href = authorizationUrl;
  }

  async callback(search?: string): Promise<HappyViewSession> {
    const params = new URLSearchParams(search ?? window.location.search);
    const code = params.get("code");
    const state = params.get("state");

    if (!state) {
      throw new OAuthCallbackError(params, 'Missing "state" parameter');
    }

    const pendingJson = await this.storage.get(`pending-auth:${state}`);
    if (!pendingJson) {
      throw new OAuthCallbackError(
        params,
        `Unknown authorization session "${state}"`,
        state,
      );
    }

    if (params.has("error")) {
      await this.storage.delete(`pending-auth:${state}`);
      await this.keyStore?.delete(pendingKeyId(state));
      throw new OAuthCallbackError(params, undefined, state);
    }

    if (!code) {
      throw new OAuthCallbackError(params, 'Missing "code" parameter', state);
    }

    const pending: PendingAuthState = JSON.parse(pendingJson);

    try {
      const { key, publicJwk, pair } = await this.pendingKey(pending, state);
      const signProof = buildProofSigner(key, publicJwk, pending.tokenEndpoint);

      const { clientId, redirectUri } = this.resolveOAuthEndpoints();

      let dpopNonce: string | undefined;
      let tokenResp!: Response;

      for (let attempt = 0; attempt < 2; attempt++) {
        const proof = await signProof(dpopNonce);

        const requestBody = new URLSearchParams({
          grant_type: "authorization_code",
          code,
          redirect_uri: redirectUri,
          client_id: clientId,
          code_verifier: pending.authPkceVerifier,
        });

        if (pending.confidential) {
          const { clientAssertion, clientAssertionType } =
            await this.getClientAssertion(pending.issuer, {
              provisionId: pending.provisionId,
              pkceVerifier: pending.provisionPkceVerifier,
            });
          requestBody.set("client_assertion", clientAssertion);
          requestBody.set("client_assertion_type", clientAssertionType);
        }

        tokenResp = await this._fetch(pending.tokenEndpoint, {
          method: "POST",
          headers: {
            "content-type": "application/x-www-form-urlencoded",
            dpop: proof,
          },
          body: requestBody,
        });

        if (!tokenResp.ok && attempt === 0) {
          const nonceHeader = tokenResp.headers.get("dpop-nonce");
          if (nonceHeader) {
            const errorBody = await tokenResp.text();
            if (errorBody.includes("use_dpop_nonce")) {
              dpopNonce = nonceHeader;
              continue;
            }
            throw new TokenExchangeError(
              `Token exchange failed: ${tokenResp.status} ${errorBody}`,
              tokenResp.status,
              errorBody,
            );
          }
        }

        break;
      }

      if (!tokenResp!.ok) {
        const err = await tokenResp!.text();
        throw new TokenExchangeError(
          `Token exchange failed: ${tokenResp!.status} ${err}`,
          tokenResp!.status,
          err,
        );
      }

      const tokens = (await tokenResp.json()) as {
        access_token: string;
        refresh_token?: string;
        scope?: string;
        sub?: string;
        iss?: string;
      };

      const session = await this.registerSession({
        provisionId: pending.provisionId,
        pkceVerifier: pending.provisionPkceVerifier,
        did: pending.did,
        accessToken: tokens.access_token,
        refreshToken: tokens.refresh_token,
        scopes: tokens.scope ?? this.scopes,
        pdsUrl: pending.pdsUrl,
        issuer: tokens.iss ?? pending.issuer,
        dpopKey: pending.rawJwk ?? publicJwk,
        dpopKeyPair: pair,
        ticketKeyProof: await this.proveTicketKey(pending.did, publicJwk),
      });

      await this.storage.delete(`pending-auth:${state}`);
      await this.keyStore?.delete(pendingKeyId(state));

      return session;
    } catch (err) {
      throw OAuthCallbackError.from(err, params, state);
    }
  }

  /**
   * The app's ticket key proof for a sign-in completing, or `undefined`: when
   * there's no prover, it returns none, or it fails. A pin is a bonus the
   * sign-in never waits on.
   */
  private async proveTicketKey(did: string, publicJwk: JsonWebKey): Promise<string | undefined> {
    if (!this.ticketKeyProver) return undefined;
    try {
      return (
        (await this.ticketKeyProver({
          did,
          dpopJkt: await jwkThumbprint(publicJwk),
          audience: this.instanceUrl,
          lxm: SESSION_PIN_LXM,
        })) || undefined
      );
    } catch {
      return undefined;
    }
  }

  /** The key a pending sign-in provisioned, from the key store or storage. */
  private async pendingKey(
    pending: PendingAuthState,
    state: string,
  ): Promise<{ key: Key; publicJwk: JsonWebKey; pair?: CryptoKeyPair }> {
    if (pending.rawJwk) {
      return {
        key: await importJwk(pending.rawJwk),
        publicJwk: publicJwkOf(pending.rawJwk),
      };
    }
    const pair = await this.keyStore?.get(pendingKeyId(state));
    if (!pair || !pending.publicJwk) {
      throw new InvalidStateError(
        "The key for this sign-in is gone; start the sign-in again.",
      );
    }
    return { key: await keyFromPair(pair), publicJwk: pending.publicJwk, pair };
  }

  async logout(did: string): Promise<void> {
    await this.deleteSession(did);
  }

  async revoke(did: string): Promise<void> {
    return this.logout(did);
  }

  override async restore(
    did?: string,
    _refresh?: boolean | "auto",
  ): Promise<HappyViewSession | null> {
    if (did) {
      const session = await this.restoreSession(did);
      if (session) {
        await this.storage.set(LAST_ACTIVE_KEY, did);
      }
      return session;
    }
    return super.restore();
  }

  async init(): Promise<
    { session: HappyViewSession; state?: string | null } | undefined
  > {
    const params = this.readCallbackParams();
    if (params) {
      return this.initCallback(`?${params.toString()}`);
    }
    return this.initRestore();
  }

  async initRestore(): Promise<{ session: HappyViewSession } | undefined> {
    const session = await this.restore();
    if (session) return { session };
    return undefined;
  }

  async initCallback(
    search?: string,
  ): Promise<{ session: HappyViewSession; state: string | null }> {
    const searchStr = search ?? window.location.search;
    const params = new URLSearchParams(searchStr);
    const state = params.get("state");

    history.replaceState(null, "", window.location.pathname);

    const session = await this.callback(searchStr);

    if (state?.startsWith(POPUP_STATE_PREFIX)) {
      const stateKey = state.slice(POPUP_STATE_PREFIX.length);
      const received = await sendPopupResult(stateKey, {
        status: "fulfilled",
        value: session.did,
      });
      if (!received) {
        await this.logout(session.did);
      }
      window.close();
      throw new LoginContinuedInParentWindowError();
    }

    return { session, state };
  }

  async signIn(
    identifier: string,
    options?: SignInOptions,
  ): Promise<HappyViewSession | void> {
    if (options?.display === "popup") {
      return this.signInPopup(identifier, options);
    }
    return this.signInRedirect(identifier, options);
  }

  async signInRedirect(
    identifier: string,
    options?: LoginOptions,
  ): Promise<void> {
    return this.login(identifier, options);
  }

  async signInPopup(
    identifier: string,
    options?: PopupLoginOptions,
  ): Promise<HappyViewSession> {
    const popupTarget = options?.popupName ?? "_blank";
    const popupFeatures =
      options?.popupFeatures ?? "width=600,height=600,menubar=no,toolbar=no";

    let popup = window.open("about:blank", popupTarget, popupFeatures);

    const stateKey = Math.random().toString(36).slice(2);
    const result = await this.prepareLogin(identifier, {
      ...options,
      state: `${POPUP_STATE_PREFIX}${stateKey}`,
    });

    if (popup) {
      popup.location.href = result.authorizationUrl;
    } else {
      popup = window.open(result.authorizationUrl, popupTarget, popupFeatures);
    }
    popup?.focus();

    return new Promise<HappyViewSession>((resolve, reject) => {
      const channel = new BroadcastChannel(POPUP_CHANNEL_NAME);
      const cleanup = () => {
        clearTimeout(timeout);
        channel.removeEventListener("message", onMessage);
        channel.close();
        popup?.close();
      };

      const timeout = setTimeout(() => {
        reject(new Error("Popup login timed out"));
        cleanup();
      }, 5 * 60e3);

      const onMessage = async ({ data }: MessageEvent) => {
        if (data.key !== stateKey) return;
        if (!("result" in data)) return;

        channel.postMessage({ key: stateKey, ack: true });
        cleanup();

        if (data.result.status === "fulfilled") {
          const did = data.result.value as string;
          try {
            const session = await this.restoreSession(did);
            if (session) {
              resolve(session);
            } else {
              reject(new Error("Failed to restore session after popup login"));
            }
          } catch (err) {
            reject(err);
            await this.logout(did);
          }
        } else {
          reject(
            new Error(data.result.reason?.message ?? "Popup login failed"),
          );
        }
      };

      channel.addEventListener("message", onMessage);
    });
  }

  readCallbackParams(): URLSearchParams | null {
    const params = new URLSearchParams(window.location.search);
    if (!params.has("state") || !(params.has("code") || params.has("error"))) {
      return null;
    }
    return params;
  }

  findRedirectUrl(): string {
    return this.redirectUri ?? `${window.location.origin}/oauth/callback`;
  }

  dispose(): void {
    // No persistent resources to clean up
  }

  async [Symbol.asyncDispose](): Promise<void> {
    this.dispose();
  }

  private resolveOAuthEndpoints(): { clientId: string; redirectUri: string } {
    return {
      clientId: this.clientId,
      redirectUri:
        this.redirectUri ?? `${window.location.origin}/oauth/callback`,
    };
  }

  private async fetchAuthServerMetadata(
    pdsUrl: string,
  ): Promise<AuthServerMetadata> {
    const base = pdsUrl.replace(/\/+$/, "");

    const resourceResp = await this._fetch(
      `${base}/.well-known/oauth-protected-resource`,
    );
    if (!resourceResp.ok) {
      throw new ResolutionError(
        `Failed to fetch protected resource metadata from ${pdsUrl}: ${resourceResp.status}`,
      );
    }
    const resource = (await resourceResp.json()) as {
      authorization_servers?: string[];
    };
    const authServer = resource.authorization_servers?.[0];
    if (!authServer) {
      throw new ResolutionError(
        `No authorization server found in protected resource metadata from ${pdsUrl}`,
      );
    }

    const metaResp = await this._fetch(
      `${authServer.replace(/\/+$/, "")}/.well-known/oauth-authorization-server`,
    );
    if (!metaResp.ok) {
      throw new ResolutionError(
        `Failed to fetch auth server metadata from ${authServer}: ${metaResp.status}`,
      );
    }
    return metaResp.json();
  }
}

function extractPdsUrl(doc: DidDocument): string {
  const services = doc.service ?? [];
  for (const service of services) {
    if (
      service.id === "#atproto_pds" ||
      (typeof service.id === "string" && service.id.endsWith("#atproto_pds"))
    ) {
      if (typeof service.serviceEndpoint === "string") {
        return service.serviceEndpoint;
      }
      throw new ResolutionError(
        `#atproto_pds service endpoint is not a string URL in DID document for ${doc.id}`,
      );
    }
  }
  throw new ResolutionError(
    `No #atproto_pds service found in DID document for ${doc.id}`,
  );
}

function buildProofSigner(
  dpopKey: Key,
  publicJwk: JsonWebKey,
  endpoint: string,
): (nonce?: string) => Promise<string> {
  const htu = endpoint.split("?")[0];

  return (nonce?: string) =>
    dpopKey.createJwt(
      {
        alg: "ES256",
        typ: "dpop+jwt",
        jwk: publicJwk as any,
      },
      {
        htm: "POST",
        htu,
        iat: Math.floor(Date.now() / 1000),
        jti: randomHex(16),
        ...(nonce ? { nonce } : {}),
      },
    );
}

function randomHex(byteLength: number): string {
  const bytes = crypto.getRandomValues(new Uint8Array(byteLength));
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

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

function sendPopupResult(
  key: string,
  result: {
    status: "fulfilled" | "rejected";
    value?: string;
    reason?: { message: string };
  },
): Promise<boolean> {
  const channel = new BroadcastChannel(POPUP_CHANNEL_NAME);
  return new Promise((resolve) => {
    const cleanup = (received: boolean) => {
      clearTimeout(timer);
      channel.removeEventListener("message", onMessage);
      channel.close();
      resolve(received);
    };
    const onMessage = ({ data }: MessageEvent) => {
      if ("ack" in data && data.key === key) cleanup(true);
    };
    channel.addEventListener("message", onMessage);
    channel.postMessage({ key, result });
    const timer = setTimeout(() => cleanup(false), 500);
  });
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
