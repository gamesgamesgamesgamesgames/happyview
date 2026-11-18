import { WebcryptoKey } from "@atproto/jwk-webcrypto";

/** A JWK with its private members (`d` and friends) removed. */
export function publicJwkOf(jwk: JsonWebKey): JsonWebKey {
  const { d: _d, p: _p, q: _q, dp: _dp, dq: _dq, qi: _qi, key_ops: _ops, ext: _ext, ...pub } = jwk;
  return pub;
}

/**
 * Import a raw ES256 JWK (as returned by HappyView's /oauth/dpop-keys) as a
 * Web Crypto key pair. With `extractable: false` the private key can sign but
 * can never be exported again, by this code or any other; the public key
 * stays exportable, since proofs carry it.
 */
export async function importKeyPair(
  jwk: JsonWebKey,
  options: { extractable?: boolean } = {},
): Promise<CryptoKeyPair> {
  const privateKey = await crypto.subtle.importKey(
    "jwk",
    jwk,
    { name: "ECDSA", namedCurve: "P-256" },
    options.extractable ?? true,
    ["sign"],
  );
  const publicKey = await crypto.subtle.importKey(
    "jwk",
    publicJwkOf(jwk),
    { name: "ECDSA", namedCurve: "P-256" },
    true,
    ["verify"],
  );
  return { privateKey, publicKey };
}

/** A signing key from a Web Crypto key pair, extractable or not. */
export async function keyFromPair(
  pair: CryptoKeyPair,
  kid: string = crypto.randomUUID(),
): Promise<WebcryptoKey> {
  return WebcryptoKey.fromKeypair(pair, kid);
}

/**
 * Import a raw ES256 JWK (as returned by HappyView's /oauth/dpop-keys)
 * into a WebcryptoKey that can sign JWTs.
 */
export async function importJwk(
  jwk: JsonWebKey,
  options: { extractable?: boolean } = {},
): Promise<WebcryptoKey> {
  const pair = await importKeyPair(jwk, options);
  const kid = (jwk as JsonWebKey & { kid?: string }).kid ?? crypto.randomUUID();
  return keyFromPair(pair, kid);
}
