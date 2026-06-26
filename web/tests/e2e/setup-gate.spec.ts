import { test, expect } from "@playwright/test"
import { loginAsTestAdmin } from "./auth-helper"

test.describe("Setup Gate", () => {
  test("unauthenticated user is redirected to /login", async ({ page }) => {
    await page.goto("/dashboard")

    await expect(page).toHaveURL(/\/login/, { timeout: 10000 })
  })

  test("authenticated user is redirected to /setup when no identity configured", async ({ page }) => {
    await loginAsTestAdmin(page)
    await page.goto("/dashboard")

    await expect(page).toHaveURL(/\/setup/, { timeout: 10000 })
  })

  test("authenticated user on settings page is redirected to /setup", async ({ page }) => {
    await loginAsTestAdmin(page)
    await page.goto("/dashboard/settings/service-identity")

    await expect(page).toHaveURL(/\/setup/, { timeout: 10000 })
  })
})
