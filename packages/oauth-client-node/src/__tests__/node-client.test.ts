import { afterEach, beforeAll, describe, expect, mock, test } from "bun:test";
import {
  HappyViewOAuthClient,
  HappyViewError,
  InvalidStateError,
  MemoryStorage,
  OAuthCallbackError,
  TokenExchangeError,
  jwkThumbprint,
} from "@happyview/oauth-client";
import { HappyViewNodeClient } from "../node-client";

let testJwk: JsonWebKey;
beforeAll(async () => {
  const keyPair = await crypto.subtle.generateKey(
    { name: "ECDSA", namedCurve: "P-256" },
    true,
    ["sign", "verify"],
  );
  testJwk = await crypto.subtle.exportKey("jwk", keyPair.privateKey);
  delete testJwk.key_ops;
});

let storage: MemoryStorage;

afterEach(() => {
  storage = new MemoryStorage();
});

function createClient(fetchFn?: typeof globalThis.fetch) {
  storage = new MemoryStorage();
  return new HappyViewNodeClient({
    instanceUrl: "https://happyview.example.com",
    clientId: "https://example.com/oauth-client-metadata.json",
    clientKey: "hvc_test",
    redirectUri: "https://example.com/oauth/callback",
    storage,
    fetch: fetchFn,
  });
}

function mockFetchForFullFlow() {
  return mock(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = input instanceof Request ? input.url : String(input);

    if (url.includes("dns.google")) {
      return new Response(
        JSON.stringify({
          Status: 0,
          Answer: [
            {
              name: "_atproto.user.bsky.social.",
              type: 16,
              TTL: 300,
              data: '"did=did:plc:abcdefghijklmnopqrstuvwx"',
            },
          ],
        }),
        { status: 200, headers: { "content-type": "application/dns-json" } },
      );
    }

    if (url.includes("plc.directory")) {
      return new Response(
        JSON.stringify({
          id: "did:plc:abcdefghijklmnopqrstuvwx",
          service: [
            {
              id: "#atproto_pds",
              type: "AtprotoPersonalDataServer",
              serviceEndpoint: "https://pds.example.com",
            },
          ],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (url.includes(".well-known/oauth-protected-resource")) {
      return new Response(
        JSON.stringify({
          authorization_servers: ["https://pds.example.com"],
        }),
        { status: 200 },
      );
    }

    if (url.includes(".well-known/oauth-authorization-server")) {
      return new Response(
        JSON.stringify({
          issuer: "https://pds.example.com",
          authorization_endpoint: "https://pds.example.com/oauth/authorize",
          token_endpoint: "https://pds.example.com/oauth/token",
          pushed_authorization_request_endpoint:
            "https://pds.example.com/oauth/par",
        }),
        { status: 200 },
      );
    }

    if (url.includes("/oauth/dpop-keys")) {
      return new Response(
        JSON.stringify({
          provision_id: "hvp_test123",
          dpop_key: testJwk,
        }),
        { status: 201 },
      );
    }

    if (url.includes("/oauth/par")) {
      return new Response(
        JSON.stringify({
          request_uri: "urn:ietf:params:oauth:request_uri:test",
          expires_in: 60,
        }),
        { status: 201 },
      );
    }

    if (url.includes("/oauth/sessions") && init?.method === "POST") {
      return new Response(
        JSON.stringify({
          session_id: "sess_test",
          did: "did:plc:abcdefghijklmnopqrstuvwx",
        }),
        { status: 201 },
      );
    }

    if (url.includes("/oauth/token")) {
      return new Response(
        JSON.stringify({
          access_token: "at_test_token",
          refresh_token: "rt_test_token",
          token_type: "DPoP",
          scope: "atproto",
          sub: "did:plc:abcdefghijklmnopqrstuvwx",
          iss: "https://pds.example.com",
        }),
        { status: 200 },
      );
    }

    return new Response("not found", { status: 404 });
  });
}

describe("HappyViewNodeClient", () => {
  test("constructor requires storage", () => {
    const client = new HappyViewNodeClient({
      instanceUrl: "https://happyview.example.com",
      clientId: "https://example.com/oauth-client-metadata.json",
      clientKey: "hvc_test",
      redirectUri: "https://example.com/oauth/callback",
      storage: new MemoryStorage(),
    });
    expect(client).toBeDefined();
  });

  test("authorize returns a URL object", async () => {
    const fetchFn = mockFetchForFullFlow();
    const client = createClient(fetchFn);

    const url = await client.authorize("user.bsky.social");

    expect(url).toBeInstanceOf(URL);
    expect(url.hostname).toBe("pds.example.com");
  });

  test("authorize stores pending auth state", async () => {
    const fetchFn = mockFetchForFullFlow();
    const client = createClient(fetchFn);

    const url = await client.authorize("user.bsky.social");

    const params = new URLSearchParams(url.search);
    const requestUri = params.get("request_uri");
    expect(requestUri).toBe("urn:ietf:params:oauth:request_uri:test");
  });

  test("authorize uses custom scopes", async () => {
    const fetchFn = mockFetchForFullFlow();
    const client = createClient(fetchFn);

    await client.authorize("user.bsky.social", {
      scopes: "atproto transition:generic",
    });

    const parCall = fetchFn.mock.calls.find((call: any[]) =>
      String(call[0]).includes("/oauth/par"),
    );
    expect(parCall).toBeDefined();
    const body = new URLSearchParams(
      (parCall![1] as RequestInit).body as string,
    );
    expect(body.get("scope")).toBe("atproto transition:generic");
  });

  test("authorize binds the PAR request to the provisioned DPoP key", async () => {
    // ⚠ AN UNBOUND PAR IS OFF-SPEC. See the matching test in
    // oauth-client-browser for the full story; the client provisions the key
    // itself, so nothing outside the SDK can supply the binding.
    const fetchFn = mockFetchForFullFlow();
    const client = createClient(fetchFn);

    await client.authorize("user.bsky.social");

    const parCall = fetchFn.mock.calls.find((call: any[]) =>
      String(call[0]).includes("/oauth/par"),
    );
    expect(parCall).toBeDefined();
    const body = new URLSearchParams(
      (parCall![1] as RequestInit).body as string,
    );
    expect(body.get("dpop_jkt")).toBe(await jwkThumbprint(testJwk));
  });

  test("authorize lets an explicit dpop_jkt override the derived one", async () => {
    const fetchFn = mockFetchForFullFlow();
    const client = createClient(fetchFn);

    await client.authorize("user.bsky.social", { dpop_jkt: "caller-supplied" });

    const parCall = fetchFn.mock.calls.find((call: any[]) =>
      String(call[0]).includes("/oauth/par"),
    );
    const body = new URLSearchParams(
      (parCall![1] as RequestInit).body as string,
    );
    expect(body.get("dpop_jkt")).toBe("caller-supplied");
  });

  test("authorize uses custom state", async () => {
    const fetchFn = mockFetchForFullFlow();
    const client = createClient(fetchFn);

    await client.authorize("user.bsky.social", {
      state: "my-custom-state",
    });

    const stored = await storage.get("pending-auth:my-custom-state");
    expect(stored).not.toBeNull();
  });

  test("callback exchanges code for tokens and returns session with state", async () => {
    const fetchFn = mockFetchForFullFlow();
    const client = createClient(fetchFn);

    await storage.set(
      "pending-auth:state123",
      JSON.stringify({
        did: "did:plc:abcdefghijklmnopqrstuvwx",
        provisionId: "hvp_test123",
        rawJwk: testJwk,
        provisionPkceVerifier: "provision-verifier",
        authPkceVerifier: "auth-verifier",
        pdsUrl: "https://pds.example.com",
        tokenEndpoint: "https://pds.example.com/oauth/token",
        state: "state123",
        issuer: "https://pds.example.com",
      }),
    );

    const params = new URLSearchParams({
      code: "auth-code-123",
      state: "state123",
    });

    const result = await client.callback(params);
    expect(result.session.did).toBe("did:plc:abcdefghijklmnopqrstuvwx");
    expect(result.state).toBe("state123");
  });

  test("callback includes DPoP proof", async () => {
    const fetchFn = mockFetchForFullFlow();
    const client = createClient(fetchFn);

    await storage.set(
      "pending-auth:statedpop",
      JSON.stringify({
        did: "did:plc:abcdefghijklmnopqrstuvwx",
        provisionId: "hvp_test123",
        rawJwk: testJwk,
        provisionPkceVerifier: "provision-verifier",
        authPkceVerifier: "auth-verifier",
        pdsUrl: "https://pds.example.com",
        tokenEndpoint: "https://pds.example.com/oauth/token",
        state: "statedpop",
        issuer: "https://pds.example.com",
      }),
    );

    await client.callback(
      new URLSearchParams({ code: "auth-code", state: "statedpop" }),
    );

    const tokenCall = fetchFn.mock.calls.find((call: any[]) =>
      String(call[0]).includes("/oauth/token"),
    );
    expect(tokenCall).toBeDefined();
    const tokenHeaders = new Headers((tokenCall![1] as RequestInit).headers);
    expect(tokenHeaders.get("dpop")).not.toBeNull();
    expect(tokenHeaders.get("dpop")!.split(".")).toHaveLength(3);
  });

  test("callback accepts redirect_uri override", async () => {
    const fetchFn = mockFetchForFullFlow();
    const client = createClient(fetchFn);

    await storage.set(
      "pending-auth:stateuri",
      JSON.stringify({
        did: "did:plc:abcdefghijklmnopqrstuvwx",
        provisionId: "hvp_test123",
        rawJwk: testJwk,
        provisionPkceVerifier: "provision-verifier",
        authPkceVerifier: "auth-verifier",
        pdsUrl: "https://pds.example.com",
        tokenEndpoint: "https://pds.example.com/oauth/token",
        state: "stateuri",
        issuer: "https://pds.example.com",
      }),
    );

    await client.callback(
      new URLSearchParams({ code: "auth-code", state: "stateuri" }),
      { redirect_uri: "https://other.example.com/callback" },
    );

    const tokenCall = fetchFn.mock.calls.find((call: any[]) =>
      String(call[0]).includes("/oauth/token"),
    );
    const body = new URLSearchParams(
      (tokenCall![1] as RequestInit).body as string,
    );
    expect(body.get("redirect_uri")).toBe(
      "https://other.example.com/callback",
    );
  });

  test("callback throws OAuthCallbackError when state is missing", async () => {
    const client = createClient();
    try {
      await client.callback(new URLSearchParams({ code: "auth-code" }));
      expect(true).toBe(false);
    } catch (err) {
      expect(err).toBeInstanceOf(OAuthCallbackError);
      expect((err as OAuthCallbackError).state).toBeUndefined();
    }
  });

  test("callback throws OAuthCallbackError when no pending state found", async () => {
    const client = createClient();
    try {
      await client.callback(
        new URLSearchParams({ code: "auth-code", state: "nonexistent" }),
      );
      expect(true).toBe(false);
    } catch (err) {
      expect(err).toBeInstanceOf(OAuthCallbackError);
      expect((err as OAuthCallbackError).state).toBe("nonexistent");
    }
  });

  test("callback throws OAuthCallbackError wrapping TokenExchangeError on token failure", async () => {
    const fetchFn = mock(
      async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        if (url.includes("/oauth/token")) {
          return new Response("invalid_grant", { status: 400 });
        }
        return new Response("not found", { status: 404 });
      },
    );

    const client = createClient(fetchFn);

    await storage.set(
      "pending-auth:statefail",
      JSON.stringify({
        did: "did:plc:abcdefghijklmnopqrstuvwx",
        provisionId: "hvp_test123",
        rawJwk: testJwk,
        provisionPkceVerifier: "provision-verifier",
        authPkceVerifier: "auth-verifier",
        pdsUrl: "https://pds.example.com",
        tokenEndpoint: "https://pds.example.com/oauth/token",
        state: "statefail",
        issuer: "https://pds.example.com",
      }),
    );

    try {
      await client.callback(
        new URLSearchParams({ code: "auth-code", state: "statefail" }),
      );
      expect(true).toBe(false);
    } catch (err) {
      expect(err).toBeInstanceOf(OAuthCallbackError);
      expect((err as OAuthCallbackError).state).toBe("statefail");
      expect((err as OAuthCallbackError).cause).toBeInstanceOf(TokenExchangeError);
      expect(((err as OAuthCallbackError).cause as TokenExchangeError).status).toBe(400);
    }
  });

  test("restore returns session for existing DID", async () => {
    const fetchFn = mockFetchForFullFlow();
    const client = createClient(fetchFn);

    await storage.set(
      "happyview:session:did:plc:abcdefghijklmnopqrstuvwx",
      JSON.stringify({
        did: "did:plc:abcdefghijklmnopqrstuvwx",
        dpopKey: testJwk,
        accessToken: "at_stored",
        clientKey: "hvc_test",
        instanceUrl: "https://happyview.example.com",
      }),
    );

    const session = await client.restore(
      "did:plc:abcdefghijklmnopqrstuvwx",
    );
    expect(session.did).toBe("did:plc:abcdefghijklmnopqrstuvwx");
  });

  test("restore throws when session does not exist", async () => {
    const client = createClient();
    try {
      await client.restore("did:plc:nonexistent");
      expect(true).toBe(false);
    } catch (err) {
      expect(err).toBeInstanceOf(InvalidStateError);
    }
  });

  test("revoke deletes session", async () => {
    const deleteFn = mock(
      async (input: RequestInfo | URL, init?: RequestInit) => {
        return new Response(null, { status: 204 });
      },
    );
    const client = createClient(deleteFn);

    await storage.set(
      "happyview:session:did:plc:abcdefghijklmnopqrstuvwx",
      JSON.stringify({
        did: "did:plc:abcdefghijklmnopqrstuvwx",
        dpopKey: testJwk,
        accessToken: "at_stored",
        clientKey: "hvc_test",
        instanceUrl: "https://happyview.example.com",
      }),
    );
    await storage.set(
      "happyview:last-active-did",
      "did:plc:abcdefghijklmnopqrstuvwx",
    );

    await client.revoke("did:plc:abcdefghijklmnopqrstuvwx");

    const session = await storage.get(
      "happyview:session:did:plc:abcdefghijklmnopqrstuvwx",
    );
    expect(session).toBeNull();
  });

  test("authorize uses scope (singular) option", async () => {
    const fetchFn = mockFetchForFullFlow();
    const client = createClient(fetchFn);

    await client.authorize("user.bsky.social", {
      scope: "atproto transition:generic",
    });

    const parCall = fetchFn.mock.calls.find((call: any[]) =>
      String(call[0]).includes("/oauth/par"),
    );
    expect(parCall).toBeDefined();
    const body = new URLSearchParams(
      (parCall![1] as RequestInit).body as string,
    );
    expect(body.get("scope")).toBe("atproto transition:generic");
  });

  test("authorize prefers scope over scopes when both provided", async () => {
    const fetchFn = mockFetchForFullFlow();
    const client = createClient(fetchFn);

    await client.authorize("user.bsky.social", {
      scope: "atproto transition:generic",
      scopes: "atproto",
    });

    const parCall = fetchFn.mock.calls.find((call: any[]) =>
      String(call[0]).includes("/oauth/par"),
    );
    const body = new URLSearchParams(
      (parCall![1] as RequestInit).body as string,
    );
    expect(body.get("scope")).toBe("atproto transition:generic");
  });

  test("restore throws when called with no DID", async () => {
    const client = createClient();
    try {
      await client.restore();
      expect(true).toBe(false);
    } catch (err) {
      expect(err).toBeInstanceOf(InvalidStateError);
    }
  });

  test("restore accepts and ignores refresh parameter", async () => {
    const fetchFn = mockFetchForFullFlow();
    const client = createClient(fetchFn);

    await storage.set(
      "happyview:session:did:plc:abcdefghijklmnopqrstuvwx",
      JSON.stringify({
        did: "did:plc:abcdefghijklmnopqrstuvwx",
        dpopKey: testJwk,
        accessToken: "at_stored",
        clientKey: "hvc_test",
        instanceUrl: "https://happyview.example.com",
      }),
    );

    const session = await client.restore(
      "did:plc:abcdefghijklmnopqrstuvwx",
      true,
    );
    expect(session.did).toBe("did:plc:abcdefghijklmnopqrstuvwx");
  });

  test("authorize without PAR falls back to direct URL", async () => {
    const fetchFn = mock(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = input instanceof Request ? input.url : String(input);

      if (url.includes("dns.google")) {
        return new Response(
          JSON.stringify({
            Status: 0,
            Answer: [
              {
                name: "_atproto.user.bsky.social.",
                type: 16,
                TTL: 300,
                data: '"did=did:plc:abcdefghijklmnopqrstuvwx"',
              },
            ],
          }),
          { status: 200, headers: { "content-type": "application/dns-json" } },
        );
      }

      if (url.includes("plc.directory")) {
        return new Response(
          JSON.stringify({
            id: "did:plc:abcdefghijklmnopqrstuvwx",
            service: [
              {
                id: "#atproto_pds",
                type: "AtprotoPersonalDataServer",
                serviceEndpoint: "https://pds.example.com",
              },
            ],
          }),
          { status: 200, headers: { "content-type": "application/json" } },
        );
      }

      if (url.includes(".well-known/oauth-protected-resource")) {
        return new Response(
          JSON.stringify({
            authorization_servers: ["https://pds.example.com"],
          }),
          { status: 200 },
        );
      }

      if (url.includes(".well-known/oauth-authorization-server")) {
        return new Response(
          JSON.stringify({
            issuer: "https://pds.example.com",
            authorization_endpoint: "https://pds.example.com/oauth/authorize",
            token_endpoint: "https://pds.example.com/oauth/token",
          }),
          { status: 200 },
        );
      }

      if (url.includes("/oauth/dpop-keys")) {
        return new Response(
          JSON.stringify({
            provision_id: "hvp_test123",
            dpop_key: testJwk,
          }),
          { status: 201 },
        );
      }

      return new Response("not found", { status: 404 });
    });

    const client = createClient(fetchFn);
    const url = await client.authorize("user.bsky.social");

    expect(url).toBeInstanceOf(URL);
    expect(url.hostname).toBe("pds.example.com");
    expect(url.pathname).toBe("/oauth/authorize");
    expect(url.searchParams.get("response_type")).toBe("code");
    expect(url.searchParams.get("client_id")).toBe(
      "https://example.com/oauth-client-metadata.json",
    );
    expect(url.searchParams.get("scope")).toBe("atproto");
  });

  test("callback retries with DPoP nonce on use_dpop_nonce error", async () => {
    let tokenAttempt = 0;
    const fetchFn = mock(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = input instanceof Request ? input.url : String(input);

      if (url.includes("/oauth/token")) {
        tokenAttempt++;
        if (tokenAttempt === 1) {
          return new Response(
            JSON.stringify({ error: "use_dpop_nonce" }),
            {
              status: 400,
              headers: { "dpop-nonce": "server-nonce-123" },
            },
          );
        }
        return new Response(
          JSON.stringify({
            access_token: "at_test_token",
            refresh_token: "rt_test_token",
            scope: "atproto",
            sub: "did:plc:abcdefghijklmnopqrstuvwx",
            iss: "https://pds.example.com",
          }),
          { status: 200 },
        );
      }

      if (url.includes("/oauth/sessions") && init?.method === "POST") {
        return new Response(
          JSON.stringify({
            session_id: "sess_test",
            did: "did:plc:abcdefghijklmnopqrstuvwx",
          }),
          { status: 201 },
        );
      }

      return new Response("not found", { status: 404 });
    });

    const client = createClient(fetchFn);

    await storage.set(
      "pending-auth:statenonce",
      JSON.stringify({
        did: "did:plc:abcdefghijklmnopqrstuvwx",
        provisionId: "hvp_test123",
        rawJwk: testJwk,
        provisionPkceVerifier: "provision-verifier",
        authPkceVerifier: "auth-verifier",
        pdsUrl: "https://pds.example.com",
        tokenEndpoint: "https://pds.example.com/oauth/token",
        state: "statenonce",
        issuer: "https://pds.example.com",
      }),
    );

    const result = await client.callback(
      new URLSearchParams({ code: "auth-code", state: "statenonce" }),
    );

    expect(result.session.did).toBe("did:plc:abcdefghijklmnopqrstuvwx");
    expect(tokenAttempt).toBe(2);

    const secondTokenCall = fetchFn.mock.calls.filter((call: any[]) =>
      String(call[0]).includes("/oauth/token"),
    )[1];
    const dpopJwt = new Headers(
      (secondTokenCall![1] as RequestInit).headers,
    ).get("dpop")!;
    const payloadB64 = dpopJwt.split(".")[1];
    const padded =
      payloadB64 + "=".repeat((4 - (payloadB64.length % 4)) % 4);
    const payload = JSON.parse(
      atob(padded.replace(/-/g, "+").replace(/_/g, "/")),
    );
    expect(payload.nonce).toBe("server-nonce-123");
  });

  test("abortRequest cleans up pending auth state", async () => {
    const fetchFn = mockFetchForFullFlow();
    const client = createClient(fetchFn);

    const url = await client.authorize("user.bsky.social", {
      state: "abort-test",
    });

    const pendingBefore = await storage.get("pending-auth:abort-test");
    expect(pendingBefore).not.toBeNull();

    await client.abortRequest(url);

    const pendingAfter = await storage.get("pending-auth:abort-test");
    expect(pendingAfter).toBeNull();
  });

  test("abortRequest is a no-op for unknown URLs", async () => {
    const client = createClient();
    await client.abortRequest(new URL("https://unknown.example.com/auth"));
  });

  test("confidential client passes clientSecret to base class", () => {
    storage = new MemoryStorage();
    const client = new HappyViewNodeClient({
      instanceUrl: "https://happyview.example.com",
      clientId: "https://example.com/oauth-client-metadata.json",
      clientKey: "hvc_test",
      clientSecret: "hvs_test_secret",
      redirectUri: "https://example.com/oauth/callback",
      storage,
    });
    expect(client.isConfidential).toBe(true);
  });

  test("public client is not confidential", () => {
    const client = createClient();
    expect(client.isConfidential).toBe(false);
  });

  test("full authorize → callback flow", async () => {
    const fetchFn = mockFetchForFullFlow();
    const client = createClient(fetchFn);

    const url = await client.authorize("user.bsky.social");
    expect(url).toBeInstanceOf(URL);

    // Simulate the PDS redirecting back with code and state
    // Extract the state from stored pending auth
    const keys: string[] = [];
    // MemoryStorage doesn't expose keys, so find it via the authorize call
    // The state is random, but we can find it by checking the PAR body
    const parCall = fetchFn.mock.calls.find((call: any[]) =>
      String(call[0]).includes("/oauth/par"),
    );
    const parBody = new URLSearchParams(
      (parCall![1] as RequestInit).body as string,
    );
    const state = parBody.get("state")!;

    const result = await client.callback(
      new URLSearchParams({ code: "auth-code", state }),
    );

    expect(result.session.did).toBe("did:plc:abcdefghijklmnopqrstuvwx");
    expect(result.state).toBe(state);
  });

  test("session.sub is an alias for session.did", async () => {
    const fetchFn = mockFetchForFullFlow();
    const client = createClient(fetchFn);

    await storage.set(
      "happyview:session:did:plc:abcdefghijklmnopqrstuvwx",
      JSON.stringify({
        did: "did:plc:abcdefghijklmnopqrstuvwx",
        dpopKey: testJwk,
        accessToken: "at_stored",
        clientKey: "hvc_test",
        instanceUrl: "https://happyview.example.com",
      }),
    );

    const session = await client.restore("did:plc:abcdefghijklmnopqrstuvwx");
    expect(session.sub).toBe(session.did);
    expect(session.sub).toBe("did:plc:abcdefghijklmnopqrstuvwx");
  });

  test("session.getTokenInfo returns available metadata", async () => {
    const fetchFn = mockFetchForFullFlow();
    const client = createClient(fetchFn);

    await storage.set(
      "happyview:session:did:plc:abcdefghijklmnopqrstuvwx",
      JSON.stringify({
        did: "did:plc:abcdefghijklmnopqrstuvwx",
        dpopKey: testJwk,
        accessToken: "at_stored",
        clientKey: "hvc_test",
        instanceUrl: "https://happyview.example.com",
        scopes: "atproto",
        pdsUrl: "https://pds.example.com",
        issuer: "https://pds.example.com",
      }),
    );

    const session = await client.restore("did:plc:abcdefghijklmnopqrstuvwx");
    const info = session.getTokenInfo();
    expect(info.sub).toBe("did:plc:abcdefghijklmnopqrstuvwx");
    expect(info.scope).toBe("atproto");
    expect(info.aud).toBe("https://pds.example.com");
    expect(info.iss).toBe("https://pds.example.com");
  });

  test("session.getTokenInfo works with legacy stored sessions (no extra fields)", async () => {
    const fetchFn = mockFetchForFullFlow();
    const client = createClient(fetchFn);

    await storage.set(
      "happyview:session:did:plc:abcdefghijklmnopqrstuvwx",
      JSON.stringify({
        did: "did:plc:abcdefghijklmnopqrstuvwx",
        dpopKey: testJwk,
        accessToken: "at_stored",
        clientKey: "hvc_test",
        instanceUrl: "https://happyview.example.com",
      }),
    );

    const session = await client.restore("did:plc:abcdefghijklmnopqrstuvwx");
    const info = session.getTokenInfo();
    expect(info.sub).toBe("did:plc:abcdefghijklmnopqrstuvwx");
    expect(info.scope).toBeUndefined();
    expect(info.aud).toBeUndefined();
    expect(info.iss).toBeUndefined();
  });

  test("session.signOut deletes the session", async () => {
    const deleteFn = mock(
      async (input: RequestInfo | URL, init?: RequestInit) => {
        return new Response(null, { status: 204 });
      },
    );
    const client = createClient(deleteFn);

    await storage.set(
      "happyview:session:did:plc:abcdefghijklmnopqrstuvwx",
      JSON.stringify({
        did: "did:plc:abcdefghijklmnopqrstuvwx",
        dpopKey: testJwk,
        accessToken: "at_stored",
        clientKey: "hvc_test",
        instanceUrl: "https://happyview.example.com",
      }),
    );
    await storage.set(
      "happyview:last-active-did",
      "did:plc:abcdefghijklmnopqrstuvwx",
    );

    const session = await client.restore("did:plc:abcdefghijklmnopqrstuvwx");
    await session.signOut();

    const stored = await storage.get(
      "happyview:session:did:plc:abcdefghijklmnopqrstuvwx",
    );
    expect(stored).toBeNull();
  });

  test("callback session includes token metadata", async () => {
    const fetchFn = mockFetchForFullFlow();
    const client = createClient(fetchFn);

    await storage.set(
      "pending-auth:statemeta",
      JSON.stringify({
        did: "did:plc:abcdefghijklmnopqrstuvwx",
        provisionId: "hvp_test123",
        rawJwk: testJwk,
        provisionPkceVerifier: "provision-verifier",
        authPkceVerifier: "auth-verifier",
        pdsUrl: "https://pds.example.com",
        tokenEndpoint: "https://pds.example.com/oauth/token",
        state: "statemeta",
        issuer: "https://pds.example.com",
      }),
    );

    const result = await client.callback(
      new URLSearchParams({ code: "auth-code", state: "statemeta" }),
    );

    const info = result.session.getTokenInfo();
    expect(info.scope).toBe("atproto");
    expect(info.iss).toBe("https://pds.example.com");
    expect(info.aud).toBe("https://pds.example.com");
  });

  test("authorize passes prompt option to PAR", async () => {
    const fetchFn = mockFetchForFullFlow();
    const client = createClient(fetchFn);

    await client.authorize("user.bsky.social", {
      prompt: "login",
    });

    const parCall = fetchFn.mock.calls.find((call: any[]) =>
      String(call[0]).includes("/oauth/par"),
    );
    const body = new URLSearchParams(
      (parCall![1] as RequestInit).body as string,
    );
    expect(body.get("prompt")).toBe("login");
  });

  test("authorize passes redirect_uri option", async () => {
    const fetchFn = mockFetchForFullFlow();
    const client = createClient(fetchFn);

    await client.authorize("user.bsky.social", {
      redirect_uri: "https://other.example.com/cb",
    });

    const parCall = fetchFn.mock.calls.find((call: any[]) =>
      String(call[0]).includes("/oauth/par"),
    );
    const body = new URLSearchParams(
      (parCall![1] as RequestInit).body as string,
    );
    expect(body.get("redirect_uri")).toBe("https://other.example.com/cb");
  });

  test("authorize passes display and ui_locales options", async () => {
    const fetchFn = mockFetchForFullFlow();
    const client = createClient(fetchFn);

    await client.authorize("user.bsky.social", {
      display: "popup",
      ui_locales: "en fr",
    });

    const parCall = fetchFn.mock.calls.find((call: any[]) =>
      String(call[0]).includes("/oauth/par"),
    );
    const body = new URLSearchParams(
      (parCall![1] as RequestInit).body as string,
    );
    expect(body.get("display")).toBe("popup");
    expect(body.get("ui_locales")).toBe("en fr");
  });

  test("handleResolver and didResolver are publicly accessible", () => {
    const client = createClient();
    expect(client.handleResolver).toBeDefined();
    expect(client.didResolver).toBeDefined();
  });

  test("fetchMetadata fetches and returns client metadata JSON", async () => {
    const metadata = {
      client_id: "https://example.com/metadata.json",
      client_name: "Test",
    };
    const fetchFn = mock(async () => {
      return new Response(JSON.stringify(metadata), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    });

    const result = await HappyViewOAuthClient.fetchMetadata({
      clientId: "https://example.com/metadata.json",
      fetch: fetchFn as typeof globalThis.fetch,
    });

    expect(result.client_id).toBe("https://example.com/metadata.json");
    expect(result.client_name).toBe("Test");
  });

  test("fetchMetadata throws on non-200 response", async () => {
    const fetchFn = mock(async () => {
      return new Response("not found", { status: 404 });
    });

    try {
      await HappyViewOAuthClient.fetchMetadata({
        clientId: "https://example.com/metadata.json",
        fetch: fetchFn as typeof globalThis.fetch,
      });
      expect(true).toBe(false);
    } catch (err) {
      expect((err as Error).message).toContain("404");
    }
  });

  test("fetchMetadata throws on non-JSON content type", async () => {
    const fetchFn = mock(async () => {
      return new Response("<html>hi</html>", {
        status: 200,
        headers: { "content-type": "text/html" },
      });
    });

    try {
      await HappyViewOAuthClient.fetchMetadata({
        clientId: "https://example.com/metadata.json",
        fetch: fetchFn as typeof globalThis.fetch,
      });
      expect(true).toBe(false);
    } catch (err) {
      expect((err as Error).message).toContain("content type");
    }
  });

  test("sessionHooks.onSessionUpdate fires after callback", async () => {
    const onSessionUpdate = mock((did: string) => {});
    const fetchFn = mockFetchForFullFlow();
    storage = new MemoryStorage();
    const client = new HappyViewNodeClient({
      instanceUrl: "https://happyview.example.com",
      clientId: "https://example.com/oauth-client-metadata.json",
      clientKey: "hvc_test",
      redirectUri: "https://example.com/oauth/callback",
      storage,
      sessionHooks: { onSessionUpdate },
      fetch: fetchFn,
    });

    await storage.set(
      "pending-auth:statehook",
      JSON.stringify({
        did: "did:plc:abcdefghijklmnopqrstuvwx",
        provisionId: "hvp_test123",
        rawJwk: testJwk,
        provisionPkceVerifier: "provision-verifier",
        authPkceVerifier: "auth-verifier",
        pdsUrl: "https://pds.example.com",
        tokenEndpoint: "https://pds.example.com/oauth/token",
        state: "statehook",
        issuer: "https://pds.example.com",
      }),
    );

    await client.callback(
      new URLSearchParams({ code: "auth-code", state: "statehook" }),
    );

    expect(onSessionUpdate).toHaveBeenCalledTimes(1);
    expect(onSessionUpdate.mock.calls[0][0]).toBe(
      "did:plc:abcdefghijklmnopqrstuvwx",
    );
  });

  test("sessionHooks.onSessionDelete fires after revoke", async () => {
    const onSessionDelete = mock((did: string) => {});
    const deleteFn = mock(async () => new Response(null, { status: 204 }));
    storage = new MemoryStorage();
    const client = new HappyViewNodeClient({
      instanceUrl: "https://happyview.example.com",
      clientId: "https://example.com/oauth-client-metadata.json",
      clientKey: "hvc_test",
      redirectUri: "https://example.com/oauth/callback",
      storage,
      sessionHooks: { onSessionDelete },
      fetch: deleteFn,
    });

    await storage.set(
      "happyview:session:did:plc:abcdefghijklmnopqrstuvwx",
      JSON.stringify({
        did: "did:plc:abcdefghijklmnopqrstuvwx",
        dpopKey: testJwk,
        accessToken: "at_stored",
        clientKey: "hvc_test",
        instanceUrl: "https://happyview.example.com",
      }),
    );

    await client.revoke("did:plc:abcdefghijklmnopqrstuvwx");

    expect(onSessionDelete).toHaveBeenCalledTimes(1);
    expect(onSessionDelete.mock.calls[0][0]).toBe(
      "did:plc:abcdefghijklmnopqrstuvwx",
    );
  });

  test("callback throws OAuthCallbackError when params contain error", async () => {
    const client = createClient();

    await storage.set(
      "pending-auth:stateerr",
      JSON.stringify({
        did: "did:plc:abcdefghijklmnopqrstuvwx",
        provisionId: "hvp_test123",
        rawJwk: testJwk,
        provisionPkceVerifier: "pv",
        authPkceVerifier: "av",
        pdsUrl: "https://pds.example.com",
        tokenEndpoint: "https://pds.example.com/oauth/token",
        state: "stateerr",
        issuer: "https://pds.example.com",
      }),
    );

    try {
      await client.callback(
        new URLSearchParams({
          error: "access_denied",
          error_description: "User denied access",
          state: "stateerr",
        }),
      );
      expect(true).toBe(false);
    } catch (err) {
      expect(err).toBeInstanceOf(OAuthCallbackError);
      const oauthErr = err as OAuthCallbackError;
      expect(oauthErr.state).toBe("stateerr");
      expect(oauthErr.params.get("error")).toBe("access_denied");
      expect(oauthErr.message).toBe("User denied access");
    }
  });

  test("callback cleans up pending state on error param", async () => {
    const client = createClient();

    await storage.set(
      "pending-auth:stateclean",
      JSON.stringify({
        did: "did:plc:abcdefghijklmnopqrstuvwx",
        provisionId: "hvp_test123",
        rawJwk: testJwk,
        provisionPkceVerifier: "pv",
        authPkceVerifier: "av",
        pdsUrl: "https://pds.example.com",
        tokenEndpoint: "https://pds.example.com/oauth/token",
        state: "stateclean",
        issuer: "https://pds.example.com",
      }),
    );

    try {
      await client.callback(
        new URLSearchParams({
          error: "access_denied",
          state: "stateclean",
        }),
      );
    } catch {
      // expected
    }

    const pending = await storage.get("pending-auth:stateclean");
    expect(pending).toBeNull();
  });
});

describe("OAuthCallbackError", () => {
  test("extends HappyViewError", () => {
    const err = new OAuthCallbackError(new URLSearchParams(), "test");
    expect(err).toBeInstanceOf(HappyViewError);
    expect(err).toBeInstanceOf(Error);
  });

  test("uses error_description from params when no message given", () => {
    const params = new URLSearchParams({
      error: "access_denied",
      error_description: "User denied",
    });
    const err = new OAuthCallbackError(params);
    expect(err.message).toBe("User denied");
  });

  test("falls back to default message when no description or message", () => {
    const err = new OAuthCallbackError(new URLSearchParams());
    expect(err.message).toBe("OAuth callback error");
  });

  test("explicit message overrides error_description", () => {
    const params = new URLSearchParams({
      error_description: "from params",
    });
    const err = new OAuthCallbackError(params, "explicit message");
    expect(err.message).toBe("explicit message");
  });

  test("preserves params and state", () => {
    const params = new URLSearchParams({ code: "abc", state: "xyz" });
    const err = new OAuthCallbackError(params, "msg", "xyz");
    expect(err.params).toBe(params);
    expect(err.state).toBe("xyz");
  });

  test("preserves cause when provided", () => {
    const cause = new Error("original");
    const err = new OAuthCallbackError(
      new URLSearchParams(),
      "wrapped",
      "s1",
      cause,
    );
    expect(err.cause).toBe(cause);
  });

  test("from() returns same instance for OAuthCallbackError input", () => {
    const original = new OAuthCallbackError(
      new URLSearchParams(),
      "original",
      "s1",
    );
    const result = OAuthCallbackError.from(
      original,
      new URLSearchParams(),
      "s2",
    );
    expect(result).toBe(original);
  });

  test("from() wraps Error with message and cause", () => {
    const cause = new TokenExchangeError("exchange failed", 400, "body");
    const params = new URLSearchParams({ state: "s1" });
    const result = OAuthCallbackError.from(cause, params, "s1");
    expect(result).toBeInstanceOf(OAuthCallbackError);
    expect(result.message).toBe("exchange failed");
    expect(result.state).toBe("s1");
    expect(result.cause).toBe(cause);
  });

  test("from() wraps non-Error with undefined message", () => {
    const params = new URLSearchParams({
      error_description: "desc from params",
    });
    const result = OAuthCallbackError.from("string error", params, "s1");
    expect(result).toBeInstanceOf(OAuthCallbackError);
    expect(result.message).toBe("desc from params");
    expect(result.cause).toBe("string error");
  });
});

describe("re-exports from sub-packages", () => {
  test("re-exports Key from @atproto/jwk via index", async () => {
    const mod = await import("../index");
    expect(mod.Key).toBeDefined();
  });

  test("re-exports AtprotoDohHandleResolver from @atproto-labs/handle-resolver via index", async () => {
    const mod = await import("../index");
    expect(mod.AtprotoDohHandleResolver).toBeDefined();
  });

  test("re-exports DidResolverCommon from @atproto-labs/did-resolver via index", async () => {
    const mod = await import("../index");
    expect(mod.DidResolverCommon).toBeDefined();
  });

  test("re-exports HappyViewSession from @happyview/oauth-client via index", async () => {
    const mod = await import("../index");
    expect(mod.HappyViewSession).toBeDefined();
  });

  test("re-exports OAuthCallbackError from @happyview/oauth-client via index", async () => {
    const mod = await import("../index");
    expect(mod.OAuthCallbackError).toBeDefined();
  });
});
