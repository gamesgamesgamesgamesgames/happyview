import { afterEach, beforeAll, describe, expect, mock, test } from "bun:test";
import { jwkThumbprint, sessionKeyId, type KeyStore } from "@happyview/oauth-client";
import { HappyViewBrowserClient, PENDING_SIGN_IN_TTL_MS } from "../browser-client";
import { LocalStorageAdapter } from "../local-storage-adapter";

class MemoryKeyStore implements KeyStore {
  readonly pairs = new Map<string, CryptoKeyPair>();
  readonly times = new Map<string, number>();
  async get(id: string) {
    return this.pairs.get(id) ?? null;
  }
  async set(id: string, pair: CryptoKeyPair) {
    this.pairs.set(id, pair);
    this.times.set(id, Date.now());
  }
  async delete(id: string) {
    this.pairs.delete(id);
    this.times.delete(id);
  }
  async entries() {
    return [...this.times].map(([id, createdAt]) => ({ id, createdAt }));
  }
}

const DID = "did:plc:abcdefghijklmnopqrstuvwx";
let testJwk: JsonWebKey;
beforeAll(async () => {
  const pair = await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, true, ["sign", "verify"]);
  testJwk = await crypto.subtle.exportKey("jwk", pair.privateKey);
  delete testJwk.key_ops;
});

afterEach(() => {
  localStorage.clear();
});

function fetchFor() {
  return mock(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = input instanceof Request ? input.url : String(input);
    const json = (body: unknown, status = 200) =>
      new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
    if (url.includes("plc.directory")) {
      return json({
        id: DID,
        service: [{ id: "#atproto_pds", type: "AtprotoPersonalDataServer", serviceEndpoint: "https://pds.example.com" }],
      });
    }
    if (url.includes("oauth-protected-resource")) return json({ authorization_servers: ["https://pds.example.com"] });
    if (url.includes("oauth-authorization-server")) {
      return json({
        issuer: "https://pds.example.com",
        authorization_endpoint: "https://pds.example.com/oauth/authorize",
        token_endpoint: "https://pds.example.com/oauth/token",
        pushed_authorization_request_endpoint: "https://pds.example.com/oauth/par",
      });
    }
    if (url.includes("/oauth/dpop-keys")) return json({ provision_id: "hvp_1", dpop_key: testJwk }, 201);
    if (url.includes("/oauth/par")) return json({ request_uri: "urn:x", expires_in: 60 }, 201);
    if (url.includes("/oauth/token")) return json({ access_token: "at", scope: "atproto", iss: "https://pds.example.com" });
    if (url.includes("/oauth/sessions") && init?.method === "POST") return json({ session_id: "s", did: DID }, 201);
    throw new Error(`unexpected fetch: ${url}`);
  });
}

function client(keyStore: KeyStore | null, fetchFn: typeof fetch) {
  return new HappyViewBrowserClient({
    instanceUrl: "https://happyview.example.com",
    clientId: "https://example.com/client-metadata.json",
    clientKey: "hvc_test",
    storage: new LocalStorageAdapter(),
    keyStore,
    fetch: fetchFn,
  });
}

function everythingStored(): string {
  let all = "";
  for (let i = 0; i < localStorage.length; i++) {
    const k = localStorage.key(i)!;
    all += `${k}=${localStorage.getItem(k)}\n`;
  }
  return all;
}

describe("sign-in with a key store", () => {
  test("the private key never reaches storage, from provisioning to the session", async () => {
    const keys = new MemoryKeyStore();
    const fetchFn = fetchFor() as unknown as typeof fetch;
    const c = client(keys, fetchFn);

    const { state } = await c.prepareLogin(DID);
    expect(everythingStored()).not.toContain(testJwk.d!);
    const pending = keys.pairs.get(`pending:${state}`)!;
    expect(pending.privateKey.extractable).toBe(false);

    const session = await c.callback(`?code=c&state=${state}`);
    expect(everythingStored()).not.toContain(testJwk.d!);
    expect(keys.pairs.has(`pending:${state}`)).toBe(false);
    expect(keys.pairs.get(sessionKeyId(DID))!.privateKey.extractable).toBe(false);
    expect(await session.dpopKeyThumbprint()).toBe(await jwkThumbprint(testJwk));

    // The token exchange was proven with that key.
    const tokenCall = (fetchFn as any).mock.calls.find((call: any[]) => String(call[0]).includes("/oauth/token"));
    const proof = new Headers(tokenCall[1].headers).get("dpop")!;
    const header = JSON.parse(atob(proof.split(".")[0].replace(/-/g, "+").replace(/_/g, "/")));
    expect(await jwkThumbprint(header.jwk)).toBe(await jwkThumbprint(testJwk));
    expect(header.jwk.d).toBeUndefined();

    // A reload restores it from the store.
    const restored = await client(keys, fetchFn).restore();
    expect(await restored!.dpopKeyThumbprint()).toBe(await jwkThumbprint(testJwk));
  });

  test("an authorization error clears the pending key", async () => {
    const keys = new MemoryKeyStore();
    const c = client(keys, fetchFor() as unknown as typeof fetch);
    const { state } = await c.prepareLogin(DID);
    await expect(c.callback(`?error=access_denied&state=${state}`)).rejects.toThrow();
    expect(keys.pairs.size).toBe(0);
  });

  test("a sign-in whose key is gone asks to start again", async () => {
    const keys = new MemoryKeyStore();
    const c = client(keys, fetchFor() as unknown as typeof fetch);
    const { state } = await c.prepareLogin(DID);
    keys.pairs.clear();
    await expect(c.callback(`?code=c&state=${state}`)).rejects.toThrow(/start the sign-in again/);
  });

  test("keyStore: null keeps the JWK in storage, as before", async () => {
    const fetchFn = fetchFor() as unknown as typeof fetch;
    const c = client(null, fetchFn);
    const { state } = await c.prepareLogin(DID);
    expect(everythingStored()).toContain(testJwk.d!);
    await c.callback(`?code=c&state=${state}`);
    expect(everythingStored()).toContain(testJwk.d!);
  });

  test("provisioning never makes an extractable key", async () => {
    const keys = new MemoryKeyStore();
    const c = client(keys, fetchFor() as unknown as typeof fetch);
    const provisioned = await c.provisionDpopKey();
    expect(provisioned.dpopKeyPair!.privateKey.extractable).toBe(false);
    const { state } = await c.prepareLogin(DID);
    expect(keys.pairs.get(`pending:${state}`)!.privateKey.extractable).toBe(false);
  });

  test("sign-ins abandoned for over an hour are swept, and newer ones kept", async () => {
    const keys = new MemoryKeyStore();
    const fetchFn = fetchFor() as unknown as typeof fetch;
    const c = client(keys, fetchFn);
    const old = await c.prepareLogin(DID);
    const fresh = await c.prepareLogin(DID);
    const later = Date.now() + 30 * 60 * 1000;
    keys.times.set(`pending:${old.state}`, later - PENDING_SIGN_IN_TTL_MS - 1);
    const pendingKey = (state: string) => `@happyview/oauth(pending-auth:${state})`;
    const aged = JSON.parse(localStorage.getItem(pendingKey(old.state))!);
    localStorage.setItem(pendingKey(old.state), JSON.stringify({ ...aged, createdAt: later - PENDING_SIGN_IN_TTL_MS - 1 }));

    await c.sweepPendingSignIns(later);
    expect(keys.pairs.has(`pending:${old.state}`)).toBe(false);
    expect(localStorage.getItem(pendingKey(old.state))).toBeNull();
    expect(keys.pairs.has(`pending:${fresh.state}`)).toBe(true);
    expect(localStorage.getItem(pendingKey(fresh.state))).not.toBeNull();
    // The one left still completes.
    expect((await c.callback(`?code=c&state=${fresh.state}`)).did).toBe(DID);
  });

  test("a pending sign-in from before timing starts its clock when swept", async () => {
    const c = client(new MemoryKeyStore(), fetchFor() as unknown as typeof fetch);
    const key = "@happyview/oauth(pending-auth:legacy)";
    localStorage.setItem(key, JSON.stringify({ did: DID, state: "legacy" }));
    await c.sweepPendingSignIns(1_000);
    expect(JSON.parse(localStorage.getItem(key)!).createdAt).toBe(1_000);
    await c.sweepPendingSignIns(1_000 + PENDING_SIGN_IN_TTL_MS + 1);
    expect(localStorage.getItem(key)).toBeNull();
  });

  test("a ticket key proof goes with the session's registration", async () => {
    const fetchFn = fetchFor();
    const asked: unknown[] = [];
    const c = new HappyViewBrowserClient({
      instanceUrl: "https://happyview.example.com",
      clientId: "https://example.com/client-metadata.json",
      clientKey: "hvc_test",
      storage: new LocalStorageAdapter(),
      keyStore: new MemoryKeyStore(),
      fetch: fetchFn as unknown as typeof fetch,
      ticketKeyProof: async (request) => {
        asked.push(request);
        return "the.ticket.proof";
      },
    });
    const { state } = await c.prepareLogin(DID);
    await c.callback(`?code=c&state=${state}`);
    expect(asked).toEqual([
      {
        did: DID,
        dpopJkt: await jwkThumbprint(testJwk),
        audience: "https://happyview.example.com",
        lxm: "happyview.oauth.registerSession",
      },
    ]);
    const register = (fetchFn as any).mock.calls.find(
      (call: any[]) => String(call[0]).includes("/oauth/sessions") && call[1]?.method === "POST",
    );
    expect(JSON.parse(register[1].body).ticket_key_proof).toBe("the.ticket.proof");
  });

  test("without a prover, registration carries no ticket key proof", async () => {
    const fetchFn = fetchFor();
    const c = client(new MemoryKeyStore(), fetchFn as unknown as typeof fetch);
    const { state } = await c.prepareLogin(DID);
    await c.callback(`?code=c&state=${state}`);
    const register = (fetchFn as any).mock.calls.find(
      (call: any[]) => String(call[0]).includes("/oauth/sessions") && call[1]?.method === "POST",
    );
    expect(JSON.parse(register[1].body)).not.toHaveProperty("ticket_key_proof");
  });
});
