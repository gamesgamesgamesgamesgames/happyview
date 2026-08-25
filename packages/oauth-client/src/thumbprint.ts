/**
 * RFC 7638 JWK thumbprints, used to bind an authorization request to the DPoP
 * key that will later prove possession at the token endpoint (`dpop_jkt`).
 *
 * ⚠ THE MEMBER SET AND ORDER ARE THE WHOLE SPECIFICATION. A thumbprint is the
 * SHA-256 of a JSON object containing *only* the members required for the key
 * type, with lexicographically ordered keys and no whitespace. Hashing the JWK
 * as it arrives would fold in `d`, `kid`, `ext` and `key_ops` — all of which
 * vary between the private key the client holds and the public key the
 * authorization server derives from the DPoP proof — producing a `dpop_jkt`
 * that never matches and a failure that surfaces at the token exchange, far
 * from its cause.
 */

function base64url(buffer: ArrayBuffer): string {
  const bytes = new Uint8Array(buffer);
  let binary = "";
  for (let i = 0; i < bytes.length; i++) {
    binary += String.fromCharCode(bytes[i]);
  }
  return btoa(binary)
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
}

/** The required members per key type, already in lexicographic order. */
const REQUIRED_MEMBERS: Record<string, readonly string[]> = {
  EC: ["crv", "kty", "x", "y"],
  OKP: ["crv", "kty", "x"],
  RSA: ["e", "kty", "n"],
  oct: ["k", "kty"],
};

/**
 * Compute the RFC 7638 thumbprint of `jwk` as a base64url-encoded SHA-256.
 *
 * Accepts either a private or a public JWK — the private component is not a
 * required member for any key type, so both yield the same thumbprint, which is
 * what makes this usable on the key as provisioned.
 */
export async function jwkThumbprint(jwk: JsonWebKey): Promise<string> {
  const kty = jwk.kty;
  if (!kty || !(kty in REQUIRED_MEMBERS)) {
    throw new TypeError(
      `Cannot compute a JWK thumbprint for unsupported key type "${kty ?? "undefined"}"`,
    );
  }

  const members = REQUIRED_MEMBERS[kty];
  const canonical: Record<string, string> = {};
  for (const member of members) {
    const value = (jwk as Record<string, unknown>)[member];
    if (typeof value !== "string" || value === "") {
      throw new TypeError(
        `Cannot compute a JWK thumbprint: "${kty}" key is missing required member "${member}"`,
      );
    }
    canonical[member] = value;
  }

  // `members` is already sorted, and object literals preserve insertion order
  // for string keys, so JSON.stringify emits the canonical form directly.
  const json = JSON.stringify(canonical);

  return base64url(
    await crypto.subtle.digest("SHA-256", new TextEncoder().encode(json)),
  );
}
