import { test, expect } from "@playwright/test"

const PDS_URL = "http://localhost:3100"

async function createPdsAccount(): Promise<{ did: string; handle: string }> {
  const suffix = Date.now().toString(36)
  const handle = `testuser-${suffix}.localhost`

  const resp = await fetch(`${PDS_URL}/xrpc/com.atproto.server.createAccount`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      email: `testuser-${suffix}@example.com`,
      handle,
      password: "test-password-e2e-123",
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
  })

  test("attach_account flow reaches authenticate step", async ({ page }) => {
    // Reset identity by calling the admin API to trigger fresh setup
    await page.goto("/dashboard/settings/service-identity")

    const changeModeButton = page.getByRole("button", {
      name: /change mode/i,
    })
    if (await changeModeButton.isVisible({ timeout: 5000 }).catch(() => false)) {
      await changeModeButton.click()
      const confirmButton = page.getByRole("button", {
        name: /confirm|yes|continue/i,
      })
      await expect(confirmButton).toBeVisible()
      await confirmButton.click()
    }

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

    // Wait for identity resolution (debounced typeahead)
    // The dropdown might or might not appear depending on PLC state
    await page.waitForTimeout(500)

    // Click continue to submit the identity
    const continueButton = page.getByRole("button", { name: /continue/i })
    await expect(continueButton).toBeEnabled({ timeout: 5000 })
    await continueButton.click()

    // Should reach the authenticate step
    await expect(
      page.getByText(/authenticate attached account/i),
    ).toBeVisible({ timeout: 10000 })

    // Verify the correct account info is displayed
    await expect(page.getByText(account.did)).toBeVisible()

    // The "Authenticate as ..." button should be visible
    await expect(
      page.getByRole("button", { name: /authenticate as/i }),
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
        await expect(
          page.getByText(/complete|success|done/i),
        ).toBeVisible({ timeout: 5000 })
      }
    } finally {
      await page.close()
    }
  })
})
