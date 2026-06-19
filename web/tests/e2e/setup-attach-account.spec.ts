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
