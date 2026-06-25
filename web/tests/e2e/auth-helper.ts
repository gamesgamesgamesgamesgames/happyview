import { createHmac } from "crypto"
import { type Page } from "@playwright/test"
import pg from "pg"

const SESSION_SECRET = "e2e-test-secret-that-is-at-least-32-bytes"
const COOKIE_NAME = "happyview_session"
const TEST_DID = "did:plc:e2e-test-admin"
const DB_URL = "postgres://happyview:happyview@localhost:5434/happyview_test"

/**
 * HKDF-Expand (RFC 5869 section 2.3) — used by the cookie crate's
 * Key::derive_from which treats the master key as PRK directly.
 */
function hkdfExpand(prk: Buffer, info: Buffer, length: number): Buffer {
  const hashLen = 32 // SHA-256
  const n = Math.ceil(length / hashLen)
  const output = Buffer.alloc(n * hashLen)
  let prev = Buffer.alloc(0)

  for (let i = 1; i <= n; i++) {
    const hmac = createHmac("sha256", prk)
    hmac.update(prev)
    hmac.update(info)
    hmac.update(Buffer.from([i]))
    prev = hmac.digest()
    prev.copy(output, (i - 1) * hashLen)
  }

  return output.subarray(0, length)
}

function deriveSigningKey(secret: string): Buffer {
  const prk = Buffer.from(secret)
  const info = Buffer.from(
    "COOKIE;SIGNED:HMAC-SHA256;PRIVATE:AEAD-AES-256-GCM",
  )
  const expanded = hkdfExpand(prk, info, 64)
  return expanded.subarray(0, 32)
}

function signCookieValue(signingKey: Buffer, value: string): string {
  const mac = createHmac("sha256", signingKey)
  mac.update(value)
  const digest = mac.digest("base64")
  return digest + value
}

async function ensureTestUser(did: string): Promise<void> {
  const client = new pg.Client(DB_URL)
  await client.connect()
  try {
    const id = "e2e-test-user-id"
    const now = new Date().toISOString()
    await client.query(
      `INSERT INTO users (id, did, is_super, created_at)
       VALUES ($1, $2, 1, $3)
       ON CONFLICT (did) DO NOTHING`,
      [id, did, now],
    )
  } finally {
    await client.end()
  }
}

export async function resetServiceIdentity(): Promise<void> {
  const client = new pg.Client(DB_URL)
  await client.connect()
  try {
    await client.query("DELETE FROM service_identity")
  } finally {
    await client.end()
  }
}

export async function setServiceIdentityMode(
  mode: string,
  opts?: { did?: string; attachedAccountDid?: string },
): Promise<void> {
  const client = new pg.Client(DB_URL)
  await client.connect()
  try {
    const now = new Date().toISOString()
    await client.query(
      `INSERT INTO service_identity (id, mode, did, attached_account_did, setup_complete, created_at, updated_at)
       VALUES (1, $1, $2, $3, TRUE, $4, $4)
       ON CONFLICT (id) DO UPDATE SET
         mode = $1,
         did = $2,
         attached_account_did = $3,
         setup_complete = TRUE,
         updated_at = $4`,
      [mode, opts?.did ?? null, opts?.attachedAccountDid ?? null, now],
    )
  } finally {
    await client.end()
  }
}

export async function loginAsTestAdmin(page: Page): Promise<void> {
  await ensureTestUser(TEST_DID)

  const signingKey = deriveSigningKey(SESSION_SECRET)
  const signedValue = signCookieValue(signingKey, TEST_DID)

  const baseURL = process.env.PLAYWRIGHT_BASE_URL || "http://127.0.0.1:3200"
  const url = new URL(baseURL)

  await page.context().addCookies([
    {
      name: COOKIE_NAME,
      value: signedValue,
      domain: url.hostname,
      path: "/",
      httpOnly: true,
      sameSite: "Lax",
      secure: false,
    },
  ])
}
