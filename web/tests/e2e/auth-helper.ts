import crypto, { createHmac } from "crypto";
import { type Page } from "@playwright/test";
import pg from "pg";

const SESSION_SECRET = "e2e-test-secret-that-is-at-least-32-bytes";
const COOKIE_NAME = "happyview_session";
const TEST_DID = "did:plc:e2e-test-admin";
const DB_URL = "postgres://happyview:happyview@localhost:5434/happyview_test";

/**
 * HKDF-Expand (RFC 5869 section 2.3) — used by the cookie crate's
 * Key::derive_from which treats the master key as PRK directly.
 */
function hkdfExpand(prk: Buffer, info: Buffer, length: number): Buffer {
  const hashLen = 32; // SHA-256
  const n = Math.ceil(length / hashLen);
  const output = Buffer.alloc(n * hashLen);
  let prev = Buffer.alloc(0);

  for (let i = 1; i <= n; i++) {
    const hmac = createHmac("sha256", prk);
    hmac.update(prev);
    hmac.update(info);
    hmac.update(Buffer.from([i]));
    prev = hmac.digest();
    prev.copy(output, (i - 1) * hashLen);
  }

  return output.subarray(0, length);
}

function deriveSigningKey(secret: string): Buffer {
  const prk = Buffer.from(secret);
  const info = Buffer.from(
    "COOKIE;SIGNED:HMAC-SHA256;PRIVATE:AEAD-AES-256-GCM",
  );
  const expanded = hkdfExpand(prk, info, 64);
  return expanded.subarray(0, 32);
}

function signCookieValue(signingKey: Buffer, value: string): string {
  const mac = createHmac("sha256", signingKey);
  mac.update(value);
  const digest = mac.digest("base64");
  return digest + value;
}

async function ensureTestUser(did: string): Promise<void> {
  const client = new pg.Client(DB_URL);
  await client.connect();
  try {
    const id = "e2e-test-user-id";
    const now = new Date().toISOString();
    await client.query(
      `INSERT INTO happyview_users (id, did, is_super, created_at)
       VALUES ($1, $2, 1, $3)
       ON CONFLICT (did) DO NOTHING`,
      [id, did, now],
    );
  } finally {
    await client.end();
  }
}

export async function resetServiceIdentity(): Promise<void> {
  const client = new pg.Client(DB_URL);
  await client.connect();
  try {
    await client.query("DELETE FROM happyview_service_identity");
  } finally {
    await client.end();
  }
}

export async function setServiceIdentityMode(
  mode: string,
  opts?: { did?: string; attachedAccountDid?: string },
): Promise<void> {
  const client = new pg.Client(DB_URL);
  await client.connect();
  try {
    const now = new Date().toISOString();
    await client.query(
      `INSERT INTO happyview_service_identity (id, mode, did, attached_account_did, setup_complete, created_at, updated_at)
       VALUES (1, $1, $2, $3, TRUE, $4, $4)
       ON CONFLICT (id) DO UPDATE SET
         mode = $1,
         did = $2,
         attached_account_did = $3,
         setup_complete = TRUE,
         updated_at = $4`,
      [mode, opts?.did ?? null, opts?.attachedAccountDid ?? null, now],
    );
  } finally {
    await client.end();
  }
}

export async function getOauthSessionSigningKid(
  did: string,
): Promise<string | null> {
  const client = new pg.Client(DB_URL);
  await client.connect();
  try {
    const { rows } = await client.query<{ signing_kid: string | null }>(
      "SELECT signing_kid FROM happyview_oauth_sessions WHERE did = $1",
      [did],
    );
    return rows[0]?.signing_kid ?? null;
  } finally {
    await client.end();
  }
}

export async function getOauthSessionTokenState(
  did: string,
): Promise<{ accessToken: string; expiresAt: string | null }> {
  const client = new pg.Client(DB_URL);
  await client.connect();
  try {
    const { rows } = await client.query<{ session_data: string }>(
      "SELECT session_data FROM happyview_oauth_sessions WHERE did = $1",
      [did],
    );
    if (rows.length === 0) {
      throw new Error(`no happyview_oauth_sessions row for ${did}`);
    }
    const session = JSON.parse(rows[0].session_data);
    return {
      accessToken: session.token_set.access_token,
      expiresAt: session.token_set.expires_at ?? null,
    };
  } finally {
    await client.end();
  }
}

export async function waitForRealAccessTokenExpiry(did: string): Promise<void> {
  const { expiresAt } = await getOauthSessionTokenState(did);
  if (!expiresAt) {
    throw new Error(`session for ${did} has no expires_at to wait out`);
  }
  const bufferMs = 15_000;
  const delayMs = new Date(expiresAt).getTime() - Date.now() + bufferMs;
  if (delayMs > 0) {
    await new Promise((resolve) => setTimeout(resolve, delayMs));
  }
}

export async function createFakeOauthSessionPinnedToKid(
  did: string,
  kid: string,
): Promise<void> {
  const client = new pg.Client(DB_URL);
  await client.connect();
  try {
    const now = new Date().toISOString();
    const sessionData = JSON.stringify({
      token_set: {
        access_token: `e2e-fake-access-token-${did}`,
        expires_at: new Date(Date.now() + 3600_000).toISOString(),
      },
    });
    await client.query(
      `INSERT INTO happyview_oauth_sessions (did, session_data, signing_kid, created_at, updated_at)
       VALUES ($1, $2, $3, $4, $4)
       ON CONFLICT (did) DO UPDATE SET session_data = $2, signing_kid = $3, updated_at = $4`,
      [did, sessionData, kid, now],
    );
  } finally {
    await client.end();
  }
}

export async function loginAsTestAdmin(page: Page): Promise<void> {
  await ensureTestUser(TEST_DID);

  const signingKey = deriveSigningKey(SESSION_SECRET);
  const signedValue = signCookieValue(signingKey, TEST_DID);

  const baseURL =
    process.env.PLAYWRIGHT_BASE_URL || "https://happyview.127-0-0-1.sslip.io";
  const url = new URL(baseURL);

  await page.context().addCookies([
    {
      name: COOKIE_NAME,
      value: signedValue,
      domain: url.hostname,
      path: "/",
      httpOnly: true,
      sameSite: "Lax",
      secure: url.protocol === "https:",
    },
  ]);
}

/**
 * Sign in at the PDS's OAuth login form.
 */
export async function submitPdsLogin(
  page: Page,
  handle: string,
  password: string,
): Promise<void> {
  await page.locator("#username").fill(handle);
  await page.locator("#password").fill(password);

  const submit = page.locator("button[type='submit']");
  await submit.click();

  const submitted = await page
    .locator("#password")
    .waitFor({ state: "detached", timeout: 15000 })
    .then(() => true)
    .catch(() => false);

  if (!submitted) {
    await submit.click();
  }
}

/**
 * Wait for the PDS to either show its consent screen or bounce straight back
 * to HappyView, and say which happened.
 */
export async function awaitConsentOrCallback(
  page: Page,
  timeout = 60000,
): Promise<"consent" | "callback" | string> {
  const authorizeButton = page.getByRole("button", { name: /^authorize$/i });

  return Promise.any([
    page.waitForURL(/sslip\.io/, { timeout }).then(() => "callback" as const),
    authorizeButton.waitFor({ timeout }).then(() => "consent" as const),
  ]).catch(() => `neither consent nor callback within ${timeout}ms`);
}

// --- DPoP session helpers -------------------------------------------------

const TOKEN_ENCRYPTION_KEY = Buffer.from(
  "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
  "base64",
);

function encryptToken(plaintext: string): Buffer {
  const nonce = crypto.randomBytes(12);
  const cipher = crypto.createCipheriv(
    "aes-256-gcm",
    TOKEN_ENCRYPTION_KEY,
    nonce,
  );
  const ciphertext = Buffer.concat([
    cipher.update(plaintext, "utf8"),
    cipher.final(),
  ]);
  return Buffer.concat([nonce, ciphertext, cipher.getAuthTag()]);
}

export async function dpopKeyIdForProvision(
  provisionId: string,
): Promise<string> {
  const client = new pg.Client(DB_URL);
  await client.connect();
  try {
    const { rows } = await client.query<{ id: string }>(
      "SELECT id FROM happyview_dpop_keys WHERE provision_id = $1",
      [provisionId],
    );
    if (rows.length === 0) {
      throw new Error(`no happyview_dpop_keys row for ${provisionId}`);
    }
    return rows[0].id;
  } finally {
    await client.end();
  }
}

export async function insertDpopSession(params: {
  id: string;
  apiClientId: string;
  dpopKeyId: string;
  userDid: string;
  accessToken: string;
  refreshToken: string;
  issuer: string;
  pdsUrl: string;
}): Promise<void> {
  const client = new pg.Client(DB_URL);
  await client.connect();
  try {
    const now = new Date().toISOString();
    await client.query(
      `INSERT INTO happyview_dpop_sessions
         (id, api_client_id, dpop_key_id, user_did, access_token_enc,
          refresh_token_enc, token_expires_at, scopes, pds_url, issuer,
          created_at, updated_at)
       VALUES ($1, $2, $3, $4, $5, $6, NULL, 'atproto', $7, $8, $9, $9)`,
      [
        params.id,
        params.apiClientId,
        params.dpopKeyId,
        params.userDid,
        encryptToken(params.accessToken),
        encryptToken(params.refreshToken),
        params.pdsUrl,
        params.issuer,
        now,
      ],
    );
  } finally {
    await client.end();
  }
}

export async function countDpopRows(
  userDid: string,
): Promise<{ sessions: number; keys: number }> {
  const client = new pg.Client(DB_URL);
  await client.connect();
  try {
    const s = await client.query(
      "SELECT COUNT(*)::int AS n FROM happyview_dpop_sessions WHERE user_did = $1",
      [userDid],
    );
    const k = await client.query(
      `SELECT COUNT(*)::int AS n FROM happyview_dpop_keys k
         JOIN happyview_dpop_sessions s ON s.dpop_key_id = k.id
        WHERE s.user_did = $1`,
      [userDid],
    );
    return { sessions: s.rows[0].n, keys: k.rows[0].n };
  } finally {
    await client.end();
  }
}

function b64url(input: Buffer): string {
  return input
    .toString("base64")
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
}

/**
 * Build a DPoP proof for `method`/`url`, signed with a provisioned key.
 */
export async function makeDpopProof(
  privateJwk: Record<string, unknown>,
  method: string,
  url: string,
  accessToken?: string,
): Promise<string> {
  const key = await crypto.webcrypto.subtle.importKey(
    "jwk",
    { ...privateJwk, key_ops: ["sign"] } as JsonWebKey,
    { name: "ECDSA", namedCurve: "P-256" },
    false,
    ["sign"],
  );

  const publicJwk = {
    kty: privateJwk.kty,
    crv: privateJwk.crv,
    x: privateJwk.x,
    y: privateJwk.y,
  };

  const header = { alg: "ES256", typ: "dpop+jwt", jwk: publicJwk };
  const payload: Record<string, unknown> = {
    htm: method,
    htu: url.split("?")[0],
    iat: Math.floor(Date.now() / 1000),
    jti: crypto.randomBytes(16).toString("hex"),
  };
  if (accessToken) {
    payload.ath = b64url(
      crypto.createHash("sha256").update(accessToken).digest(),
    );
  }

  const signingInput = `${b64url(Buffer.from(JSON.stringify(header)))}.${b64url(
    Buffer.from(JSON.stringify(payload)),
  )}`;
  const signature = await crypto.webcrypto.subtle.sign(
    { name: "ECDSA", hash: "SHA-256" },
    key,
    Buffer.from(signingInput),
  );
  return `${signingInput}.${b64url(Buffer.from(signature))}`;
}
