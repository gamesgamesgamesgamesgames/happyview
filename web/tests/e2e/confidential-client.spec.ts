import { test, expect } from "@playwright/test"
import { loginAsTestAdmin } from "./auth-helper"

// A non-loopback host, so `register_api_client` takes the confidential branch
// rather than `AtprotoLocalhostClientMetadata`. Nothing resolves this host;
// registration never makes an outbound request.
const DOMAIN_URL = "https://confidential-client.e2e.invalid"
const DOMAIN_HOST = "confidential-client.e2e.invalid"

test.describe("Confidential OAuth client (domain)", () => {
  let domainId: string | undefined

  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page)
  })

  test.afterEach(async ({ page }) => {
    if (domainId) {
      await page.request.delete(`/admin/domains/${domainId}`)
      domainId = undefined
    }
  })

  test("both the primary client and a registered domain publish private_key_jwt metadata", async ({
    page,
    baseURL,
  }) => {
    // Register the domain through the admin API — `register_api_client` runs
    // synchronously inside the request handler, so the client is live
    // immediately, with no restart needed.
    const created = await page.request.post("/admin/domains", {
      data: { url: DOMAIN_URL },
    })
    expect(created.status()).toBe(201)
    const domain = await created.json()
    domainId = domain.id as string
    expect(domainId).toBeTruthy()

    // The domain client is confidential: HappyView serves its own metadata
    // document, holds the private key, and can sign `private_key_jwt`
    // assertions on its behalf.
    const domainMetadataResp = await page.request.get(
      "/oauth-client-metadata.json",
      { headers: { "x-forwarded-host": DOMAIN_HOST } },
    )
    expect(domainMetadataResp.ok()).toBeTruthy()
    const domainMetadata = await domainMetadataResp.json()

    expect(domainMetadata.token_endpoint_auth_method).toBe("private_key_jwt")
    expect(domainMetadata.token_endpoint_auth_signing_alg).toBe("ES256")
    expect(Array.isArray(domainMetadata.jwks?.keys)).toBe(true)
    expect(domainMetadata.jwks.keys.length).toBeGreaterThan(0)
    for (const key of domainMetadata.jwks.keys) {
      expect(key.kid).toBeTruthy()
      // The private scalar must never be served, even for our own domain.
      expect(key.d).toBeUndefined()
    }

    // The PRIMARY client is confidential too. The e2e stack serves HappyView
    // over a hostname precisely so this holds: under the old
    // `PUBLIC_URL: http://127.0.0.1:3200` it took the localhost-client branch,
    // published "none", and no test in the suite ever made a PDS validate a
    // live client assertion. This assertion is what stops that regressing — if
    // it flips back to "none", the real-OAuth specs still pass while
    // exercising no client authentication at all.
    const primaryMetadataResp = await page.request.get(
      "/oauth-client-metadata.json",
    )
    expect(primaryMetadataResp.ok()).toBeTruthy()
    const primaryMetadata = await primaryMetadataResp.json()
    expect(primaryMetadata.token_endpoint_auth_method).toBe("private_key_jwt")
    expect(primaryMetadata.token_endpoint_auth_signing_alg).toBe("ES256")
    // Derived from baseURL rather than hardcoded: the exact e2e hostname is a
    // stack detail, but "the client_id is the https origin we are served from"
    // is the property. A loopback origin here would mean the confidential
    // branch was never taken.
    const origin = new URL(baseURL!).origin
    expect(origin.startsWith("https://")).toBe(true)
    expect(primaryMetadata.client_id).toBe(
      `${origin}/oauth-client-metadata.json`,
    )
    expect(Array.isArray(primaryMetadata.jwks?.keys)).toBe(true)
    expect(primaryMetadata.jwks.keys.length).toBeGreaterThan(0)
    for (const key of primaryMetadata.jwks.keys) {
      expect(key.kid).toBeTruthy()
      expect(key.d).toBeUndefined()
    }

    // Clean up now rather than only in afterEach, so a second run of this
    // test (or another spec) never trips the "domain already exists" guard.
    const deleted = await page.request.delete(`/admin/domains/${domainId}`)
    expect(deleted.status()).toBe(204)
    domainId = undefined
  })
})
