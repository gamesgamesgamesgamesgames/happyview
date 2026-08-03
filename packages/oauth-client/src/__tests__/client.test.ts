import { describe, expect, mock, test } from "bun:test";
import {
  HappyViewOAuthClient,
  LAST_ACTIVE_KEY,
  STORAGE_PREFIX,
} from "../client";
import { ApiError } from "../errors";
import { MemoryStorage } from "../storage";

function createMockFetch(responses: Array<{ status: number; body: unknown }>) {
  let callIndex = 0;
  const calls: Array<{ url: string; init: RequestInit }> = [];

  const fetchFn = mock(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = input instanceof URL ? input.toString() : String(input);
    calls.push({ url, init: init ?? {} });
    const resp = responses[callIndex] ?? {
      status: 500,
      body: { error: "no more mocked responses" },
    };
    callIndex++;
    return new Response(JSON.stringify(resp.body), { status: resp.status });
  });

  return { fetchFn, calls };
}

async function generateTestJwk(): Promise<JsonWebKey> {
  const keyPair = await crypto.subtle.generateKey(
    { name: "ECDSA", namedCurve: "P-256" },
    true,
    ["sign", "verify"],
  );
  const jwk = await crypto.subtle.exportKey("jwk", keyPair.privateKey);
  // Remove key_ops so importJwk can re-import with its own usage constraints
  delete jwk.key_ops;
  return jwk;
}

async function storageWithSession(
  jwk: JsonWebKey,
  did = "did:plc:testuser",
): Promise<MemoryStorage> {
  const storage = new MemoryStorage();
  await storage.set(
    `happyview:session:${did}`,
    JSON.stringify({
      did,
      dpopKey: jwk,
      accessToken: "at_token",
      clientKey: "hvc_testkey",
      instanceUrl: "https://happyview.example.com",
    }),
  );
  await storage.set("happyview:last-active-did", did);
  return storage;
}

function createClient(overrides?: {
  fetchFn?: typeof globalThis.fetch;
  clientSecret?: string;
  storage?: MemoryStorage;
}) {
  return new HappyViewOAuthClient({
    instanceUrl: "https://happyview.example.com",
    clientKey: "hvc_testkey",
    clientSecret: overrides?.clientSecret,
    storage: overrides?.storage ?? new MemoryStorage(),
    fetch: overrides?.fetchFn,
  });
}

describe("HappyViewOAuthClient", () => {
  describe("provisionDpopKey", () => {
    test("calls POST /oauth/dpop-keys with client credentials in headers", async () => {
      const testJwk = await generateTestJwk();
      const { fetchFn, calls } = createMockFetch([
        {
          status: 201,
          body: {
            provision_id: "hvp_abc123",
            dpop_key: testJwk,
          },
        },
      ]);

      const client = createClient({ fetchFn, clientSecret: "hvs_secret" });
      const result = await client.provisionDpopKey();

      expect(calls[0].url).toBe(
        "https://happyview.example.com/oauth/dpop-keys",
      );
      const headers = new Headers(calls[0].init.headers);
      expect(headers.get("x-client-key")).toBe("hvc_testkey");
      expect(headers.get("x-client-secret")).toBe("hvs_secret");
      expect(result.provisionId).toBe("hvp_abc123");
      expect(result.dpopKey).toBeDefined();
      expect(result.rawJwk).toBeDefined();
    });

    test("includes PKCE challenge for public clients and returns verifier", async () => {
      const testJwk = await generateTestJwk();
      const { fetchFn, calls } = createMockFetch([
        {
          status: 201,
          body: {
            provision_id: "hvp_public",
            dpop_key: testJwk,
          },
        },
      ]);

      const client = createClient({ fetchFn });
      const result = await client.provisionDpopKey();

      const body = JSON.parse(calls[0].init.body as string);
      expect(body.pkce_challenge).toBeDefined();
      expect(typeof body.pkce_challenge).toBe("string");
      expect(result.pkceVerifier).toBeDefined();
      expect(typeof result.pkceVerifier).toBe("string");
    });

    test("does not include PKCE for confidential clients", async () => {
      const testJwk = await generateTestJwk();
      const { fetchFn, calls } = createMockFetch([
        {
          status: 201,
          body: {
            provision_id: "hvp_conf",
            dpop_key: testJwk,
          },
        },
      ]);

      const client = createClient({ fetchFn, clientSecret: "hvs_sec" });
      const result = await client.provisionDpopKey();

      const body = JSON.parse(calls[0].init.body as string);
      expect(body.pkce_challenge).toBeUndefined();
      expect(result.pkceVerifier).toBeUndefined();
    });

    test("throws ApiError on non-201 response", async () => {
      const { fetchFn } = createMockFetch([
        { status: 400, body: { message: "bad request" } },
      ]);

      const client = createClient({ fetchFn });
      try {
        await client.provisionDpopKey();
        expect(true).toBe(false);
      } catch (err) {
        expect(err).toBeInstanceOf(ApiError);
        expect((err as ApiError).status).toBe(400);
        expect((err as ApiError).body).toEqual({ message: "bad request" });
      }
    });
  });

  describe("registerSession", () => {
    test("calls POST /oauth/sessions and returns a HappyViewSession", async () => {
      const testJwk = await generateTestJwk();
      const { fetchFn, calls } = createMockFetch([
        {
          status: 201,
          body: { session_id: "sess_123", did: "did:plc:testuser" },
        },
      ]);

      const storage = new MemoryStorage();
      const client = createClient({
        fetchFn,
        clientSecret: "hvs_sec",
        storage,
      });
      const session = await client.registerSession({
        provisionId: "hvp_abc",
        did: "did:plc:testuser",
        accessToken: "at_token",
        scopes: "atproto",
        dpopKey: testJwk,
      });

      expect(calls[0].url).toBe(
        "https://happyview.example.com/oauth/sessions",
      );
      expect(session.did).toBe("did:plc:testuser");
    });

    test("persists session and last active DID to storage", async () => {
      const testJwk = await generateTestJwk();
      const { fetchFn } = createMockFetch([
        {
          status: 201,
          body: { session_id: "sess_123", did: "did:plc:testuser" },
        },
      ]);

      const storage = new MemoryStorage();
      const client = createClient({
        fetchFn,
        clientSecret: "hvs_sec",
        storage,
      });
      await client.registerSession({
        provisionId: "hvp_abc",
        did: "did:plc:testuser",
        accessToken: "at_token",
        scopes: "atproto",
        dpopKey: testJwk,
      });

      const stored = await storage.get("happyview:session:did:plc:testuser");
      expect(stored).not.toBeNull();
      const parsed = JSON.parse(stored!);
      expect(parsed.did).toBe("did:plc:testuser");
      expect(parsed.accessToken).toBe("at_token");

      const lastActive = await storage.get("happyview:last-active-did");
      expect(lastActive).toBe("did:plc:testuser");
    });
  });

  describe("deleteSession", () => {
    test("calls DELETE /oauth/sessions/:did", async () => {
      const testJwk = await generateTestJwk();
      const { fetchFn, calls } = createMockFetch([
        { status: 204, body: null },
      ]);

      const storage = new MemoryStorage();
      await storage.set(
        "happyview:session:did:plc:testuser",
        JSON.stringify({
          did: "did:plc:testuser",
          dpopKey: testJwk,
          accessToken: "at_token",
          clientKey: "hvc_testkey",
          instanceUrl: "https://happyview.example.com",
        }),
      );
      await storage.set("happyview:last-active-did", "did:plc:testuser");

      const client = createClient({
        fetchFn,
        clientSecret: "hvs_sec",
        storage,
      });
      await client.deleteSession("did:plc:testuser");

      expect(calls[0].url).toBe(
        "https://happyview.example.com/oauth/sessions/did:plc:testuser",
      );
      expect(calls[0].init.method).toBe("DELETE");
    });

    test("clears session and last active DID from storage", async () => {
      const testJwk = await generateTestJwk();
      const { fetchFn } = createMockFetch([{ status: 204, body: null }]);

      const storage = new MemoryStorage();
      await storage.set(
        "happyview:session:did:plc:testuser",
        JSON.stringify({
          did: "did:plc:testuser",
          dpopKey: testJwk,
          accessToken: "at_token",
          clientKey: "hvc_testkey",
          instanceUrl: "https://happyview.example.com",
        }),
      );
      await storage.set("happyview:last-active-did", "did:plc:testuser");

      const client = createClient({
        fetchFn,
        clientSecret: "hvs_sec",
        storage,
      });
      await client.deleteSession("did:plc:testuser");

      expect(
        await storage.get("happyview:session:did:plc:testuser"),
      ).toBeNull();
      expect(await storage.get("happyview:last-active-did")).toBeNull();
    });

    test("preserves last active DID when deleting a different session", async () => {
      const testJwk = await generateTestJwk();
      const { fetchFn } = createMockFetch([{ status: 204, body: null }]);

      const storage = new MemoryStorage();
      await storage.set(
        "happyview:session:did:plc:other",
        JSON.stringify({
          did: "did:plc:other",
          dpopKey: testJwk,
          accessToken: "at_token",
          clientKey: "hvc_testkey",
          instanceUrl: "https://happyview.example.com",
        }),
      );
      await storage.set("happyview:last-active-did", "did:plc:testuser");

      const client = createClient({
        fetchFn,
        clientSecret: "hvs_sec",
        storage,
      });
      await client.deleteSession("did:plc:other");

      expect(await storage.get("happyview:session:did:plc:other")).toBeNull();
      expect(await storage.get("happyview:last-active-did")).toBe(
        "did:plc:testuser",
      );
    });

    // A logout that leaves the session in storage is self-perpetuating: the
    // next restore() signs the user straight back in, and pressing "log out"
    // again repeats the same failure. Local cleanup must be unconditional.
    test("clears storage when the server rejects the delete with 401", async () => {
      const testJwk = await generateTestJwk();
      const { fetchFn } = createMockFetch([{ status: 401, body: {} }]);

      const storage = await storageWithSession(testJwk);

      const client = createClient({ fetchFn, storage });
      await client.deleteSession("did:plc:testuser");

      expect(
        await storage.get("happyview:session:did:plc:testuser"),
      ).toBeNull();
      expect(await storage.get("happyview:last-active-did")).toBeNull();
    });

    // 401 means the credential is already invalid — there is nothing left to
    // revoke, so this is a completed logout, not a failed one.
    test("does not throw on 401", async () => {
      const testJwk = await generateTestJwk();
      const { fetchFn } = createMockFetch([{ status: 401, body: {} }]);
      const storage = await storageWithSession(testJwk);
      const client = createClient({ fetchFn, storage });

      await expect(
        client.deleteSession("did:plc:testuser"),
      ).resolves.toBeUndefined();
    });

    test("does not throw on 403", async () => {
      const testJwk = await generateTestJwk();
      const { fetchFn } = createMockFetch([{ status: 403, body: {} }]);
      const storage = await storageWithSession(testJwk);
      const client = createClient({ fetchFn, storage });

      await expect(
        client.deleteSession("did:plc:testuser"),
      ).resolves.toBeUndefined();
    });

    // A 5xx may mean the server still holds a live session, so the caller has
    // to hear about it — but they are still logged out locally either way.
    test("still throws on 500, after clearing storage", async () => {
      const testJwk = await generateTestJwk();
      const { fetchFn } = createMockFetch([{ status: 500, body: {} }]);
      const storage = await storageWithSession(testJwk);
      const client = createClient({ fetchFn, storage });

      await expect(
        client.deleteSession("did:plc:testuser"),
      ).rejects.toThrow(ApiError);

      expect(
        await storage.get("happyview:session:did:plc:testuser"),
      ).toBeNull();
      expect(await storage.get("happyview:last-active-did")).toBeNull();
    });

    // restoreSession JSON.parses the stored blob, so a corrupt entry throws
    // before any request is made. That must not strand the user either.
    test("clears storage when the stored session is corrupt", async () => {
      const { fetchFn } = createMockFetch([]);
      const storage = new MemoryStorage();
      await storage.set("happyview:session:did:plc:testuser", "{not json");
      await storage.set("happyview:last-active-did", "did:plc:testuser");

      const client = createClient({ fetchFn, storage });
      await client.deleteSession("did:plc:testuser").catch(() => {});

      expect(
        await storage.get("happyview:session:did:plc:testuser"),
      ).toBeNull();
      expect(await storage.get("happyview:last-active-did")).toBeNull();
    });

    test("fires onSessionDelete even when the server returns 401", async () => {
      const testJwk = await generateTestJwk();
      const { fetchFn } = createMockFetch([{ status: 401, body: {} }]);
      const storage = await storageWithSession(testJwk);

      const deleted: string[] = [];
      const client = new HappyViewOAuthClient({
        instanceUrl: "https://happyview.example.com",
        clientKey: "hvc_testkey",
        storage,
        fetch: fetchFn,
        sessionHooks: { onSessionDelete: (did) => deleted.push(did) },
      });
      await client.deleteSession("did:plc:testuser");

      expect(deleted).toEqual(["did:plc:testuser"]);
    });
  });

  // Anyone recovering a stuck session by hand has to know these strings, so
  // they are API whether or not they are exported. Pin them.
  describe("storage keys", () => {
    test("exports the key format it writes", async () => {
      const testJwk = await generateTestJwk();
      const storage = await storageWithSession(testJwk);

      expect(STORAGE_PREFIX).toBe("happyview:session:");
      expect(LAST_ACTIVE_KEY).toBe("happyview:last-active-did");
      expect(
        await storage.get(`${STORAGE_PREFIX}did:plc:testuser`),
      ).not.toBeNull();
    });
  });

  describe("forgetSession", () => {
    test("clears local session state without contacting the server", async () => {
      const testJwk = await generateTestJwk();
      const { fetchFn, calls } = createMockFetch([]);
      const storage = await storageWithSession(testJwk);

      const client = createClient({ fetchFn, storage });
      await client.forgetSession("did:plc:testuser");

      expect(calls).toHaveLength(0);
      expect(
        await storage.get("happyview:session:did:plc:testuser"),
      ).toBeNull();
      expect(await storage.get("happyview:last-active-did")).toBeNull();
    });

    test("preserves last active DID when forgetting a different session", async () => {
      const testJwk = await generateTestJwk();
      const storage = new MemoryStorage();
      await storage.set(
        "happyview:session:did:plc:other",
        JSON.stringify({
          did: "did:plc:other",
          dpopKey: testJwk,
          accessToken: "at_token",
          clientKey: "hvc_testkey",
          instanceUrl: "https://happyview.example.com",
        }),
      );
      await storage.set("happyview:last-active-did", "did:plc:testuser");

      const client = createClient({ storage });
      await client.forgetSession("did:plc:other");

      expect(await storage.get("happyview:session:did:plc:other")).toBeNull();
      expect(await storage.get("happyview:last-active-did")).toBe(
        "did:plc:testuser",
      );
    });
  });

  describe("restoreSession", () => {
    test("returns null when no session in storage", async () => {
      const client = createClient();
      const session = await client.restoreSession("did:plc:nobody");
      expect(session).toBeNull();
    });

    test("restores session from storage", async () => {
      const testJwk = await generateTestJwk();
      const storage = new MemoryStorage();
      await storage.set(
        "happyview:session:did:plc:testuser",
        JSON.stringify({
          did: "did:plc:testuser",
          dpopKey: testJwk,
          accessToken: "at_stored",
          clientKey: "hvc_testkey",
          instanceUrl: "https://happyview.example.com",
        }),
      );

      const client = createClient({ storage });
      const session = await client.restoreSession("did:plc:testuser");
      expect(session).not.toBeNull();
      expect(session!.did).toBe("did:plc:testuser");
    });
  });

  describe("restore", () => {
    test("returns null when no last active DID", async () => {
      const client = createClient();
      const session = await client.restore();
      expect(session).toBeNull();
    });

    test("restores last active session", async () => {
      const testJwk = await generateTestJwk();
      const storage = new MemoryStorage();
      await storage.set("happyview:last-active-did", "did:plc:testuser");
      await storage.set(
        "happyview:session:did:plc:testuser",
        JSON.stringify({
          did: "did:plc:testuser",
          dpopKey: testJwk,
          accessToken: "at_stored",
          clientKey: "hvc_testkey",
          instanceUrl: "https://happyview.example.com",
        }),
      );

      const client = createClient({ storage });
      const session = await client.restore();
      expect(session).not.toBeNull();
      expect(session!.did).toBe("did:plc:testuser");
    });
  });
});
