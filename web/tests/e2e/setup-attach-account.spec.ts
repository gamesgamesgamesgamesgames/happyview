import { test, expect } from "@playwright/test"
import { loginAsTestAdmin, resetServiceIdentity } from "./auth-helper"

const PDS_URL = "http://localhost:3100"

async function createPdsAccount(): Promise<{ did: string; handle: string }> {
  const suffix = Date.now().toString(36)
  const handle = `testuser-${suffix}.test`

  const resp = await fetch(`${PDS_URL}/xrpc/com.atproto.server.createAccount`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      email: `testuser-${suffix}@example.com`,
      handle,
      password: "Test-password-e2e-123",
    }),
  })

  if (!resp.ok) {
    const body = await resp.text()
    throw new Error(`PDS createAccount failed (${resp.status}): ${body}`)
  }

  const data = (await resp.json()) as { did: string; handle: string }
  return { did: data.did, handle: data.handle ?? handle }
}

test.describe("Setup - Attach Account", () => {
  let account: { did: string; handle: string }

  test.beforeAll(async () => {
    account = await createPdsAccount()
    await resetServiceIdentity()
  })

  test("attach_account flow reaches authenticate step", async ({ page }) => {
    await page.goto("/setup")
    await expect(
      page.getByText(/how should this appview be identified/i),
    ).toBeVisible({ timeout: 10000 })

    // Select "Attach existing account"
    await page.getByText(/attach existing account/i).click()
    await page.getByRole("button", { name: /continue/i }).click()

    // Configure step: enter the PDS account DID
    const identifierInput = page.getByLabel(/account identifier/i)
    await expect(identifierInput).toBeVisible({ timeout: 5000 })
    await identifierInput.fill(account.did)

    // Wait for the typeahead dropdown and select the suggestion
    const suggestion = page.locator(".bg-popover button").first()
    await expect(suggestion).toBeVisible({ timeout: 5000 })
    await suggestion.click()

    // Click continue to submit the identity
    const continueButton = page.getByRole("button", { name: /continue/i })
    await expect(continueButton).toBeEnabled({ timeout: 5000 })
    await continueButton.click()

    // Should reach the authenticate step
    await expect(
      page.getByText(/authenticate attached account/i),
    ).toBeVisible({ timeout: 10000 })

    // Verify the authenticate button contains the account handle
    await expect(
      page.getByRole("button", { name: new RegExp(`authenticate as.*${account.handle}`, "i") }),
    ).toBeVisible()
  })

  test("full OAuth flow completes through PDS authorization", async ({ page }) => {
    await resetServiceIdentity()
    await loginAsTestAdmin(page)
    await page.goto("/setup")

    // Select "Attach existing account"
    await expect(
      page.getByText(/how should this appview be identified/i),
    ).toBeVisible({ timeout: 10000 })
    await page.getByText(/attach existing account/i).click()
    await page.getByRole("button", { name: /continue/i }).click()

    // Enter the DID directly without selecting from the typeahead so
    // attachedHandle stays null and the OAuth flow uses the DID.
    // Handle resolution for .test domains won't work from inside Docker.
    const identifierInput = page.getByLabel(/account identifier/i)
    await expect(identifierInput).toBeVisible({ timeout: 5000 })
    await identifierInput.fill(account.did)

    // Dismiss any typeahead dropdown that appears
    await page.keyboard.press("Escape")

    const continueButton = page.getByRole("button", { name: /continue/i })
    await expect(continueButton).toBeEnabled({ timeout: 5000 })
    await continueButton.click()

    // Reach the authenticate step
    await expect(
      page.getByText(/authenticate attached account/i),
    ).toBeVisible({ timeout: 10000 })

    // Click "Authenticate as @handle" — this triggers the OAuth flow:
    // 1. Frontend fetches /auth/login?handle=<did> to get the authorization URL
    // 2. HappyView's backend makes a PAR request to the PDS via Caddy (HTTPS)
    // 3. Frontend redirects to the PDS OAuth login page
    const authButton = page.getByRole("button", {
      name: /authenticate as/i,
    })
    await authButton.click()

    // Wait for redirect to PDS OAuth login page (served via Caddy at pds.localhost)
    await page.waitForURL(/pds\.localhost/, { timeout: 30000 })

    // Fill in credentials on the PDS OAuth login form
    // The PDS login page has: username input (#username), password input (#password), submit button
    const usernameInput = page.locator("#username")
    await expect(usernameInput).toBeVisible({ timeout: 10000 })
    await usernameInput.fill(account.handle)

    const passwordInput = page.locator("#password")
    await expect(passwordInput).toBeVisible({ timeout: 5000 })
    await passwordInput.fill("Test-password-e2e-123")

    // Submit the login form
    await page.locator("button[type='submit']").click()

    // After login, the PDS may show a consent screen or auto-redirect.
    // Wait for either the consent page or the redirect back to HappyView.
    const consentOrCallback = await Promise.race([
      page.waitForURL(/127\.0\.0\.1:3200/, { timeout: 30000 }).then(() => "callback" as const),
      page.locator("text=/authorize/i").waitFor({ timeout: 10000 }).then(() => "consent" as const).catch(() => null),
    ])

    if (consentOrCallback === "consent") {
      // Click the authorize button on the consent page
      const authorizeButton = page.locator("button", { hasText: /authorize/i }).last()
      await authorizeButton.click()
      await page.waitForURL(/127\.0\.0\.1:3200/, { timeout: 15000 })
    }

    // We're back on HappyView after the OAuth callback.
    // The setup-attach-auth component detects the return via localStorage
    // and calls confirmAttachAuth to restore the admin session.
    // Then the wizard advances to the "verify" step.
    await expect(
      page.getByRole("tab", { name: "Verify", selected: true }),
    ).toBeVisible({ timeout: 15000 })
  })

  // Restore setup state for subsequent tests
  test.afterAll(async ({ browser }) => {
    const page = await browser.newPage()
    try {
      await page.goto("/setup")
      const notExposedCard = page.getByText(/not exposed/i)
      if (
        await notExposedCard.isVisible({ timeout: 3000 }).catch(() => false)
      ) {
        await notExposedCard.click()
        await page.getByRole("button", { name: /continue/i }).click()
        await expect(
          page.getByText("Setup Complete"),
        ).toBeVisible({ timeout: 5000 })
      }
    } finally {
      await page.close()
    }
  })
})
