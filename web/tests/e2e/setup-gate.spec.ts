import { test, expect } from "@playwright/test"

test.describe("Setup Gate", () => {
  test("dashboard redirects to /setup when no identity configured", async ({ page }) => {
    await page.goto("/dashboard")

    await expect(page).toHaveURL(/\/setup/, { timeout: 10000 })
  })

  test("dashboard settings page redirects to /setup", async ({ page }) => {
    await page.goto("/dashboard/settings/service-identity")

    await expect(page).toHaveURL(/\/setup/, { timeout: 10000 })
  })
})
