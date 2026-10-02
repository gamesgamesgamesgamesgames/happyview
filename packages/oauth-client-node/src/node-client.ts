import { AtprotoDohHandleResolver } from "@atproto-labs/handle-resolver";
import { DidResolverCommon } from "@atproto-labs/did-resolver";
import type { DidDocument } from "@atproto/did";
import {
  HappyViewOAuthClient,
  HappyViewSession,
  importJwk,
  jwkThumbprint,
  InvalidStateError,
  OAuthCallbackError,
  ResolutionError,
  TokenExchangeError,
  type SessionEventHooks,
  type StorageAdapter,
} from "@happyview/oauth-client";

export interface HappyViewNodeClientOptions {
  instanceUrl: string;
  clientId: string;
  clientKey: string;
  clientSecret?: string;
  redirectUri: string;
  scopes?: string;
  storage: StorageAdapter;
  sessionHooks?: SessionEventHooks;
  fetch?: typeof globalThis.fetch;
}

export interface AuthorizeOptions {
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

export interface CallbackOptions {
  redirect_uri?: string;
}

interface PendingAuthState {
  did: string;
  provisionId: string;
  rawJwk: JsonWebKey;
  provisionPkceVerifier: string;
  authPkceVerifier: string;
  pdsUrl: string;
  tokenEndpoint: string;
  state: string;
  issuer: string;
  authUrl?: string;
}

interface AuthServerMetadata {
  issuer: string;
  authorization_endpoint: string;
  token_endpoint: string;
  pushed_authorization_request_endpoint?: string;
  dpop_signing_alg_values_supported?: string[];
}

export class HappyViewNodeClient extends HappyViewOAuthClient {
  readonly handleResolver: AtprotoDohHandleResolver;
  readonly didResolver: DidResolverCommon;
  private readonly clientId: string;
  private readonly redirectUri: string;
  private readonly scopes: string;

  constructor(options: HappyViewNodeClientOptions) {
    const fetchFn =
      options.fetch ??
      (((input: RequestInfo | URL, init?: RequestInit) =>
        fetch(input, init)) as typeof globalThis.fetch);

    super({
      instanceUrl: options.instanceUrl,
      clientKey: options.clientKey,
      clientSecret: options.clientSecret,
      storage: options.storage,
      sessionHooks: options.sessionHooks,
      fetch: fetchFn,
    });

    this.clientId = options.clientId;
    this.redirectUri = options.redirectUri;
    this.scopes = options.scopes ?? "atproto";
    this.handleResolver = new AtprotoDohHandleResolver({
      dohEndpoint: "https://dns.google/resolve",
      fetch: fetchFn,
    });
    this.didResolver = new DidResolverCommon({ fetch: fetchFn });
  }

  async authorize(
    handle: string,
    options?: AuthorizeOptions,
  ): Promise<URL> {
    const resolvedDid = await this.handleResolver.resolve(handle);
    if (!resolvedDid) {
      throw new ResolutionError(`Failed to resolve handle: ${handle}`);
    }
    const did = resolvedDid as string;

    const didDoc = await this.didResolver.resolve(resolvedDid);
    const pdsUrl = extractPdsUrl(didDoc);
    const authMeta = await this.fetchAuthServerMetadata(pdsUrl);

    const scopes = options?.scope ?? options?.scopes ?? this.scopes;

    const { provisionId, rawJwk, pkceVerifier: provisionPkceVerifier } =
      await this.provisionDpopKey();

    const authPkceVerifier = generatePkceVerifier();
    const authPkceChallenge = await computePkceChallenge(authPkceVerifier);

    const state = options?.state ?? randomHex(16);

    const pendingState: PendingAuthState = {
      did,
      provisionId,
      rawJwk,
      provisionPkceVerifier: provisionPkceVerifier!,
      authPkceVerifier,
      pdsUrl,
      tokenEndpoint: authMeta.token_endpoint,
      state,
      issuer: authMeta.issuer,
    };
    await this.storage.set(
      `pending-auth:${state}`,
      JSON.stringify(pendingState),
    );

    const redirectUri = options?.redirect_uri ?? this.redirectUri;

    const authParams = new URLSearchParams({
      response_type: "code",
      client_id: this.clientId,
      redirect_uri: redirectUri,
      state,
      scope: scopes,
      code_challenge: authPkceChallenge,
      code_challenge_method: "S256",
      login_hint: handle,
    });

    if (options?.display) authParams.set("display", options.display);
    if (options?.prompt) authParams.set("prompt", options.prompt);
    if (options?.nonce) authParams.set("nonce", options.nonce);
    if (options?.max_age != null) authParams.set("max_age", String(options.max_age));
    if (options?.ui_locales) authParams.set("ui_locales", options.ui_locales);
    // ⚠ THE AUTHORIZATION REQUEST MUST BE BOUND TO THE DPoP KEY, and this client
    // is the only thing that can do it — `provisionDpopKey` above returned the
    // very key that signs the token-endpoint proof in `callback`, and it never
    // leaves the SDK. Sent unbound, the PAR carries neither a `DPoP` header nor
    // `dpop_jkt`; bsky's PDS tolerates that, but the tolerance is deprecated
    // (https://atproto.com/blog/oauth-improvements#deprecation-notice) and other
    // implementations reject the request outright. A caller-supplied `dpop_jkt`
    // still wins, for anyone driving the key themselves.
    authParams.set("dpop_jkt", options?.dpop_jkt ?? (await jwkThumbprint(rawJwk)));
    if (options?.id_token_hint) authParams.set("id_token_hint", options.id_token_hint);
    if (options?.claims) authParams.set("claims", JSON.stringify(options.claims));
    if (options?.authorization_details) authParams.set("authorization_details", JSON.stringify(options.authorization_details));

    const parEndpoint = authMeta.pushed_authorization_request_endpoint;
    if (parEndpoint) {
      const parResp = await this._fetch(parEndpoint, {
        method: "POST",
        headers: {
          "content-type": "application/x-www-form-urlencoded",
        },
        body: authParams,
      });

      if (!parResp.ok) {
        const err = await parResp.text();
        throw new ResolutionError(
          `PAR request failed: ${parResp.status} ${err}`,
        );
      }

      const parData = (await parResp.json()) as { request_uri: string };
      const url = new URL(
        `${authMeta.authorization_endpoint}?` +
          new URLSearchParams({
            client_id: this.clientId,
            request_uri: parData.request_uri,
          }),
      );
      pendingState.authUrl = url.href;
      await this.storage.set(`pending-auth:${state}`, JSON.stringify(pendingState));
      await this.storage.set(`pending-auth-url:${url.href}`, state);
      return url;
    }

    const url = new URL(
      `${authMeta.authorization_endpoint}?${authParams}`,
    );
    pendingState.authUrl = url.href;
    await this.storage.set(`pending-auth:${state}`, JSON.stringify(pendingState));
    await this.storage.set(`pending-auth-url:${url.href}`, state);
    return url;
  }

  async callback(
    params: URLSearchParams,
    options?: CallbackOptions,
  ): Promise<{ session: HappyViewSession; state: string | null }> {
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
      throw new OAuthCallbackError(params, undefined, state);
    }

    if (!code) {
      throw new OAuthCallbackError(
        params,
        'Missing "code" parameter',
        state,
      );
    }
    const pending: PendingAuthState = JSON.parse(pendingJson);

    try {
      const dpopKey = await importJwk(pending.rawJwk);
      const { d: _, ...publicJwk } = pending.rawJwk;
      const redirectUri = options?.redirect_uri ?? this.redirectUri;

      let dpopNonce: string | undefined;
      let tokenResp!: Response;

      for (let attempt = 0; attempt < 2; attempt++) {
        const proof = await dpopKey.createJwt(
          {
            alg: "ES256",
            typ: "dpop+jwt",
            jwk: publicJwk as any,
          },
          {
            htm: "POST",
            htu: pending.tokenEndpoint,
            iat: Math.floor(Date.now() / 1000),
            jti: randomHex(16),
            ...(dpopNonce ? { nonce: dpopNonce } : {}),
          },
        );

        tokenResp = await this._fetch(pending.tokenEndpoint, {
          method: "POST",
          headers: {
            "content-type": "application/x-www-form-urlencoded",
            dpop: proof,
          },
          body: new URLSearchParams({
            grant_type: "authorization_code",
            code,
            redirect_uri: redirectUri,
            client_id: this.clientId,
            code_verifier: pending.authPkceVerifier,
          }),
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
        dpopKey: pending.rawJwk,
      });

      await this.storage.delete(`pending-auth:${state}`);
      if (pending.authUrl) {
        await this.storage.delete(`pending-auth-url:${pending.authUrl}`);
      }

      return { session, state };
    } catch (err) {
      throw OAuthCallbackError.from(err, params, state);
    }
  }

  override async restore(did?: string, _refresh?: boolean | "auto"): Promise<HappyViewSession> {
    if (!did) {
      throw new InvalidStateError(
        "DID is required for restore() in the Node client",
      );
    }
    const session = await this.restoreSession(did);
    if (!session) {
      throw new InvalidStateError(`No session found for ${did}`);
    }
    return session;
  }

  async revoke(did: string): Promise<void> {
    await this.deleteSession(did);
  }

  async abortRequest(url: URL): Promise<void> {
    const urlKey = `pending-auth-url:${url.href}`;
    const state = await this.storage.get(urlKey);
    if (state) {
      await this.storage.delete(`pending-auth:${state}`);
      await this.storage.delete(urlKey);
    }
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
    return metaResp.json() as Promise<AuthServerMetadata>;
  }
}

function extractPdsUrl(doc: DidDocument): string {
  const services = doc.service ?? [];
  for (const service of services) {
    if (
      service.id === "#atproto_pds" ||
      (typeof service.id === "string" &&
        service.id.endsWith("#atproto_pds"))
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

function randomHex(byteLength: number): string {
  const bytes = crypto.getRandomValues(new Uint8Array(byteLength));
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join(
    "",
  );
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
