import { afterEach, beforeAll, describe, expect, mock, test } from "bun:test";
import { jwkThumbprint, sessionKeyId, type KeyStore } from "@happyview/oauth-client";
import { HappyViewBrowserClient } from "../browser-client";
import { LocalStorageAdapter } from "../local-storage-adapter";

class MemoryKeyStore implements KeyStore {
  readonly pairs = new Map<string, CryptoKeyPair>();
  async get(id: string) {
    return this.pairs.get(id) ?? null;
  }
  async set(id: string, pair: CryptoKeyPair) {
    this.pairs.set(id, pair);
  }
  async delete(id: string) {
    this.pairs.delete(id);
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
});
