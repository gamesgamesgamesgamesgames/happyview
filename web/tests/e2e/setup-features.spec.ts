import { test, expect } from "@playwright/test"
import { loginAsTestAdmin, resetServiceIdentity } from "./auth-helper"

test.describe("Setup - Features", () => {
  test.beforeEach(async () => {
    await resetServiceIdentity()
  })

  test("skip for now completes setup", async ({ page }) => {
    await loginAsTestAdmin(page)
    await page.goto("/setup")
    await expect(page.getByText(/set up your service identity/i)).toBeVisible({ timeout: 10000 })

    await page.getByText(/skip for now/i).click()
    await page.getByRole("button", { name: /continue/i }).click()

    await expect(page.getByText("Your AppView is ready", { exact: true })).toBeVisible({ timeout: 5000 })
    await expect(page.getByText(/skipped/i)).toBeVisible()
  })

  test("stepper identity click resets wizard state", async ({ page }) => {
    await loginAsTestAdmin(page)
    await page.goto("/setup")
    await expect(page.getByText(/set up your service identity/i)).toBeVisible({ timeout: 10000 })

    // Select did:web and advance to the verify step
    await page.getByText("Use your domain").click()
    await page.getByRole("button", { name: /continue/i }).click()
    await expect(page.getByText("Review your domain identity")).toBeVisible({ timeout: 10000 })

    // Click the Identity stepper step to go back and reset
    await page.getByRole("tab", { name: "Identity" }).click()

    // Should be back at mode selection with state reset
    await expect(page.getByText(/set up your service identity/i)).toBeVisible({ timeout: 5000 })

    // Verify we can select a different mode (state was fully reset)
    await page.getByText("Create a new network identity").click()
    await expect(page.getByRole("button", { name: /continue/i })).toBeEnabled()
  })

  test("did:web shows continue anyway when document fetch fails", async ({ page }) => {
    await page.route("**/.well-known/did.json", (route) =>
      route.fulfill({ status: 500 }),
    )

    await loginAsTestAdmin(page)
    await page.goto("/setup")
    await expect(page.getByText(/set up your service identity/i)).toBeVisible({ timeout: 10000 })

    await page.getByText("Use your domain").click()
    await page.getByRole("button", { name: /continue/i }).click()

    await expect(page.getByText("Review your domain identity")).toBeVisible({ timeout: 10000 })

    // Should show the fetch error alert
    await expect(page.getByText(/could not load your did document/i)).toBeVisible({ timeout: 5000 })

    // Button should say "Continue anyway" instead of "Looks good"
    const continueButton = page.getByRole("button", { name: /continue anyway/i })
    await expect(continueButton).toBeVisible()
    await expect(continueButton).toBeEnabled()
  })

  test("focus moves to step content on transitions", async ({ page }) => {
    await loginAsTestAdmin(page)
    await page.goto("/setup")
    await expect(page.getByText(/set up your service identity/i)).toBeVisible({ timeout: 10000 })

    await page.getByText("Use your domain").click()
    await page.getByRole("button", { name: /continue/i }).click()

    await expect(page.getByText("Review your domain identity")).toBeVisible({ timeout: 10000 })

    // The step content container (div with tabIndex=-1) should receive focus
    const focused = await page.evaluate(() => {
      const el = document.activeElement
      return el?.getAttribute("tabindex")
    })
    expect(focused).toBe("-1")
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
          page.getByText("Your AppView is ready", { exact: true }),
        ).toBeVisible({ timeout: 5000 })
      }
    } finally {
      await page.close()
    }
  })
})
