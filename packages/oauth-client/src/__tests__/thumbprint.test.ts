import { beforeAll, describe, expect, test } from "bun:test";
import { jwkThumbprint } from "../thumbprint";

let privateJwk: JsonWebKey;
beforeAll(async () => {
  const keyPair = await crypto.subtle.generateKey(
    { name: "ECDSA", namedCurve: "P-256" },
    true,
    ["sign", "verify"],
  );
  privateJwk = await crypto.subtle.exportKey("jwk", keyPair.privateKey);
  delete privateJwk.key_ops;
});

function base64url(buffer: ArrayBuffer): string {
  const bytes = new Uint8Array(buffer);
  let binary = "";
  for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

describe("jwkThumbprint", () => {
  test("hashes exactly the RFC 7638 canonical form for an EC key", async () => {
    // The canonicalization is the part that is easy to get wrong, so it is
    // pinned here rather than delegated: for `kty: "EC"` the required members
    // are crv, kty, x, y — in lexicographic order, no whitespace, nothing else.
    const canonical = `{"crv":"${privateJwk.crv}","kty":"EC","x":"${privateJwk.x}","y":"${privateJwk.y}"}`;
    const expected = base64url(
      await crypto.subtle.digest("SHA-256", new TextEncoder().encode(canonical)),
    );

    expect(await jwkThumbprint(privateJwk)).toBe(expected);
  });

  test("is a 43-character base64url SHA-256 digest", async () => {
    const jkt = await jwkThumbprint(privateJwk);
    expect(jkt).toMatch(/^[A-Za-z0-9_-]{43}$/u);
  });

  test("ignores the private component, so public and private keys agree", async () => {
    const { d: _d, ...publicJwk } = privateJwk;

    expect(await jwkThumbprint(privateJwk)).toBe(await jwkThumbprint(publicJwk));
  });

  test("ignores non-required members such as kid, use and alg", async () => {
    const decorated = {
      ...privateJwk,
      kid: "some-key-id",
      use: "sig",
      alg: "ES256",
      ext: true,
    };

    expect(await jwkThumbprint(decorated)).toBe(
      await jwkThumbprint(privateJwk),
    );
  });

  test("does not depend on the order members appear in the object", async () => {
    const reordered: JsonWebKey = {
      y: privateJwk.y,
      kty: privateJwk.kty,
      x: privateJwk.x,
      crv: privateJwk.crv,
    };

    const { d: _d, ...publicJwk } = privateJwk;
    expect(await jwkThumbprint(reordered)).toBe(await jwkThumbprint(publicJwk));
  });

  test("canonicalizes the other RFC 7638 key types by their own member sets", async () => {
    const rsa = await jwkThumbprint({ kty: "RSA", n: "abc", e: "AQAB" });
    const expected = base64url(
      await crypto.subtle.digest(
        "SHA-256",
        new TextEncoder().encode('{"e":"AQAB","kty":"RSA","n":"abc"}'),
      ),
    );
    expect(rsa).toBe(expected);
  });

  test("rejects a key type it cannot canonicalize", async () => {
    // Silently hashing the wrong member set would produce a `dpop_jkt` that
    // never matches the proof, which fails far away from the cause.
    expect(jwkThumbprint({ kty: "banana" } as JsonWebKey)).rejects.toThrow(
      /unsupported/iu,
    );
  });

  test("rejects an EC key that is missing a required member", async () => {
    expect(jwkThumbprint({ kty: "EC", crv: "P-256", x: "abc" })).rejects.toThrow(
      /missing/iu,
    );
  });
});
