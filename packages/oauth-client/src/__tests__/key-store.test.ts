import { describe, expect, mock, test } from "bun:test";
import { HappyViewOAuthClient, STORAGE_PREFIX, sessionKeyId } from "../client";
import { MemoryStorage } from "../storage";
import { jwkThumbprint } from "../thumbprint";
import type { KeyStore } from "../types";

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

const DID = "did:plc:testuser";
const INSTANCE = "https://happyview.example.com";

async function generateTestJwk(): Promise<JsonWebKey> {
  const pair = await crypto.subtle.generateKey(
    { name: "ECDSA", namedCurve: "P-256" },
    true,
    ["sign", "verify"],
  );
  const jwk = await crypto.subtle.exportKey("jwk", pair.privateKey);
  delete jwk.key_ops;
  return jwk;
}

function okFetch() {
  const calls: Array<{ url: string; init: RequestInit }> = [];
  const fetchFn = mock(async (input: RequestInfo | URL, init?: RequestInit) => {
    calls.push({ url: String(input), init: init ?? {} });
    return new Response(JSON.stringify({ session_id: "s", did: DID }), {
      status: 201,
    });
  });
  return { fetchFn: fetchFn as unknown as typeof fetch, calls };
}

function client(storage: MemoryStorage, keyStore?: KeyStore, fetchFn?: typeof fetch) {
  return new HappyViewOAuthClient({
    instanceUrl: INSTANCE,
    clientKey: "hvc_testkey",
    storage,
    keyStore,
    fetch: fetchFn,
  });
}

async function stored(storage: MemoryStorage) {
  return JSON.parse((await storage.get(`${STORAGE_PREFIX}${DID}`))!);
}

/** The DPoP proof a session sends verifies against the public key it carries. */
async function proofVerifies(proof: string, publicJwk: JsonWebKey) {
  const [h, c, s] = proof.split(".");
  const key = await crypto.subtle.importKey(
    "jwk",
    publicJwk,
    { name: "ECDSA", namedCurve: "P-256" },
    false,
    ["verify"],
  );
  const sig = Uint8Array.from(atob(s.replace(/-/g, "+").replace(/_/g, "/")), (ch) => ch.charCodeAt(0));
  return crypto.subtle.verify(
    { name: "ECDSA", hash: "SHA-256" },
    key,
    sig,
    new TextEncoder().encode(`${h}.${c}`),
  );
}

describe("with a key store", () => {
  test("a new session keeps its key non-extractable and stores no private key", async () => {
    const jwk = await generateTestJwk();
    const storage = new MemoryStorage();
    const keys = new MemoryKeyStore();
    const { fetchFn, calls } = okFetch();
    const session = await client(storage, keys, fetchFn).registerSession({
      provisionId: "hvp_1",
      did: DID,
      accessToken: "at",
      scopes: "atproto",
      dpopKey: jwk,
    });

    const saved = await stored(storage);
    expect(saved.dpopKey).toBeUndefined();
    expect(saved.dpopPublicKey.d).toBeUndefined();
    expect(saved.dpopPublicKey.x).toBe(jwk.x);
    expect(JSON.stringify(saved)).not.toContain(jwk.d!);

    const pair = keys.pairs.get(sessionKeyId(DID))!;
    expect(pair.privateKey.extractable).toBe(false);
    await expect(crypto.subtle.exportKey("jwk", pair.privateKey)).rejects.toThrow();

    // It still signs.
    await session.fetchHandler("/xrpc/x.y.z", { method: "GET" });
    const proof = new Headers(calls.at(-1)!.init.headers).get("dpop")!;
    expect(await proofVerifies(proof, saved.dpopPublicKey)).toBe(true);
    expect(await session.dpopKeyThumbprint()).toBe(await jwkThumbprint(jwk));
  });

  test("a session stored with its JWK moves the key to the store on restore", async () => {
    const jwk = await generateTestJwk();
    const storage = new MemoryStorage();
    await storage.set(
      `${STORAGE_PREFIX}${DID}`,
      JSON.stringify({ did: DID, dpopKey: jwk, accessToken: "at", clientKey: "hvc_testkey", instanceUrl: INSTANCE }),
    );
    const keys = new MemoryKeyStore();
    const c = client(storage, keys);

    const session = await c.restoreSession(DID);
    expect(session).not.toBeNull();
    const saved = await stored(storage);
    expect(saved.dpopKey).toBeUndefined();
    expect(saved.dpopPublicKey).toMatchObject({ x: jwk.x, y: jwk.y });
    expect(saved.accessToken).toBe("at");
    expect(keys.pairs.get(sessionKeyId(DID))!.privateKey.extractable).toBe(false);
    expect(await session!.dpopKeyThumbprint()).toBe(await jwkThumbprint(jwk));

    // And from then on comes from the store.
    const again = await c.restoreSession(DID);
    expect(await again!.dpopKeyThumbprint()).toBe(await jwkThumbprint(jwk));
  });

  test("forgetting a session deletes its key", async () => {
    const storage = new MemoryStorage();
    const keys = new MemoryKeyStore();
    const { fetchFn } = okFetch();
    const c = client(storage, keys, fetchFn);
    await c.registerSession({ provisionId: "p", did: DID, accessToken: "at", scopes: "atproto", dpopKey: await generateTestJwk() });
    expect(keys.pairs.size).toBe(1);
    await c.forgetSession(DID);
    expect(keys.pairs.size).toBe(0);
  });

  test("a session whose key is gone doesn't restore", async () => {
    const storage = new MemoryStorage();
    const keys = new MemoryKeyStore();
    const { fetchFn } = okFetch();
    const c = client(storage, keys, fetchFn);
    await c.registerSession({ provisionId: "p", did: DID, accessToken: "at", scopes: "atproto", dpopKey: await generateTestJwk() });
    keys.pairs.clear();
    expect(await c.restoreSession(DID)).toBeNull();
  });

  test("a key pair imported earlier is used as given", async () => {
    const jwk = await generateTestJwk();
    const { d: _, ...publicJwk } = jwk;
    const pair = {
      privateKey: await crypto.subtle.importKey("jwk", jwk, { name: "ECDSA", namedCurve: "P-256" }, false, ["sign"]),
      publicKey: await crypto.subtle.importKey("jwk", publicJwk, { name: "ECDSA", namedCurve: "P-256" }, true, ["verify"]),
    };
    const storage = new MemoryStorage();
    const keys = new MemoryKeyStore();
    const { fetchFn } = okFetch();
    await client(storage, keys, fetchFn).registerSession({
      provisionId: "p", did: DID, accessToken: "at", scopes: "atproto",
      dpopKey: publicJwk, dpopKeyPair: pair,
    });
    expect(keys.pairs.get(sessionKeyId(DID))).toBe(pair);
    expect((await stored(storage)).dpopPublicKey.x).toBe(jwk.x);
  });
});

describe("without a key store", () => {
  test("sessions keep their JWK, as before", async () => {
    const jwk = await generateTestJwk();
    const storage = new MemoryStorage();
    const { fetchFn } = okFetch();
    const session = await client(storage, undefined, fetchFn).registerSession({
      provisionId: "p", did: DID, accessToken: "at", scopes: "atproto", dpopKey: jwk,
    });
    expect((await stored(storage)).dpopKey).toEqual(jwk);
    expect(await session.dpopKeyThumbprint()).toBe(await jwkThumbprint(jwk));
  });
});
