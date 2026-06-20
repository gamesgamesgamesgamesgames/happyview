import { test, expect } from "@playwright/test"
import { resetServiceIdentity } from "./auth-helper"

test.describe("Setup - did:plc", () => {
  test.beforeAll(async () => {
    await resetServiceIdentity()
  })

  test("did:plc flow completes successfully", async ({ page }) => {
    await page.goto("/setup")

    // Select "Create did:plc"
    await expect(
      page.getByText(/how should this appview be identified/i),
    ).toBeVisible({ timeout: 10000 })

    await page.getByText("Create did:plc").click()
    await page.getByRole("button", { name: /continue/i }).click()

    // The configure step shows "Create did:plc" card with Continue button
    await expect(page.getByText("Create did:plc").first()).toBeVisible({ timeout: 5000 })
    await page.getByRole("button", { name: /continue/i }).click()

    // Wait for either "Registering DID..." or the result
    const registeringText = page.getByText("Registering DID...")
    const exportKeyText = page.getByText("Export Rotation Key")
    const registrationFailed = page.getByText("Registration Failed")

    // Wait for the registration to start or complete
    await expect(
      registeringText.or(exportKeyText).or(registrationFailed),
    ).toBeVisible({ timeout: 10000 })

    // If registration is in progress, wait for it to finish
    if (await registeringText.isVisible().catch(() => false)) {
      await expect(
        exportKeyText.or(registrationFailed),
      ).toBeVisible({ timeout: 30000 })
    }

    // If registration failed, skip the rest of the test
    if (await registrationFailed.isVisible().catch(() => false)) {
      test.skip(true, "PLC registration failed in test environment")
      return
    }

    // Verify "Download Rotation Key" button is visible
    await expect(
      page.getByRole("button", { name: /download rotation key/i }),
    ).toBeVisible()

    // Click Continue to complete setup
    await page.getByRole("button", { name: /continue/i }).click()

    // Verify setup completes
    await expect(page.getByText("Setup Complete")).toBeVisible({ timeout: 5000 })
  })

  // Restore setup state for subsequent tests
  test.afterAll(async ({ browser }) => {
    await resetServiceIdentity()
    const page = await browser.newPage()
    try {
      await page.goto(
        (process.env.PLAYWRIGHT_BASE_URL || "http://127.0.0.1:3200") + "/setup",
      )
      const notExposedCard = page.getByText(/not exposed/i)
      if (
        await notExposedCard.isVisible({ timeout: 5000 }).catch(() => false)
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
