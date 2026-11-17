import { IDBFactory } from "fake-indexeddb";
import { describe, expect, test } from "bun:test";
import { IndexedDbKeyStore } from "../indexeddb-key-store";

async function pair(): Promise<CryptoKeyPair> {
  return crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, false, ["sign", "verify"]);
}

describe("IndexedDbKeyStore", () => {
  test("keeps a non-extractable key pair that still signs", async () => {
    const store = new IndexedDbKeyStore("keys", new IDBFactory());
    const original = await pair();
    await store.set("session:did:plc:a", original);

    const back = await store.get("session:did:plc:a");
    expect(back).not.toBeNull();
    expect(back!.privateKey.extractable).toBe(false);
    await expect(crypto.subtle.exportKey("jwk", back!.privateKey)).rejects.toThrow();
    const data = new TextEncoder().encode("hello");
    const sig = await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, back!.privateKey, data);
    expect(await crypto.subtle.verify({ name: "ECDSA", hash: "SHA-256" }, original.publicKey, sig, data)).toBe(true);
  });

  test("reads back what another instance on the same database stored, and deletes", async () => {
    const factory = new IDBFactory();
    await new IndexedDbKeyStore("keys", factory).set("k", await pair());
    const other = new IndexedDbKeyStore("keys", factory);
    expect(await other.get("k")).not.toBeNull();
    await other.delete("k");
    expect(await other.get("k")).toBeNull();
    expect(await other.get("never-set")).toBeNull();
  });
});
