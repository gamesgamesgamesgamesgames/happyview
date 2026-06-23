import { test, expect } from "@playwright/test"
import { loginAsTestAdmin, resetServiceIdentity } from "./auth-helper"

test.describe("Setup - did:plc", () => {
  test.beforeAll(async () => {
    await resetServiceIdentity()
  })

  test("did:plc flow completes successfully", async ({ page }) => {
    await loginAsTestAdmin(page)
    await page.goto("/setup")

    // Select "Create a new network identity"
    await expect(
      page.getByText(/set up your service identity/i),
    ).toBeVisible({ timeout: 10000 })

    await page.getByText("Create a new network identity").click()
    await page.getByRole("button", { name: /continue/i }).click()

    // Wait for either registration in progress or the result
    const registeringText = page.getByText("Registering your identity")
    const saveKeyText = page.getByText("Save your rotation key")
    const registrationFailed = page.getByText("Registration failed")

    await expect(
      registeringText.or(saveKeyText).or(registrationFailed),
    ).toBeVisible({ timeout: 10000 })

    // If registration is in progress, wait for it to finish
    if (await registeringText.isVisible().catch(() => false)) {
      await expect(
        saveKeyText.or(registrationFailed),
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
    await expect(page.getByText("Your AppView is ready")).toBeVisible({ timeout: 5000 })
  })

  // Restore setup state for subsequent tests
  test.afterAll(async ({ browser }) => {
    await resetServiceIdentity()
    const page = await browser.newPage()
    try {
      await loginAsTestAdmin(page)
      await page.goto(
        (process.env.PLAYWRIGHT_BASE_URL || "http://127.0.0.1:3200") + "/setup",
      )
      const skipCard = page.getByText(/skip for now/i)
      if (
        await skipCard.isVisible({ timeout: 5000 }).catch(() => false)
      ) {
        await skipCard.click()
        await page.getByRole("button", { name: /continue/i }).click()
        await expect(
          page.getByText("Your AppView is ready"),
        ).toBeVisible({ timeout: 5000 })
      }
    } finally {
      await page.close()
    }
  })
})
