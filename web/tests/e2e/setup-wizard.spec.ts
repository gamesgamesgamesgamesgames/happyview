import { test, expect } from "@playwright/test"
import { loginAsTestAdmin, resetServiceIdentity } from "./auth-helper"

test.describe("Setup Wizard", () => {
  test.beforeAll(async () => {
    await resetServiceIdentity()
  })

  test("did:web flow completes successfully", async ({ page }) => {
    await loginAsTestAdmin(page)
    await page.goto("/setup")

    await page.getByText("Use your domain").click()
    await page.getByRole("button", { name: /continue/i }).click()

    await expect(page.getByText("Review your domain identity")).toBeVisible({ timeout: 10000 })

    // Verify the wizard resumes at the correct step after reload
    await page.reload()
    await expect(page.getByText("Review your domain identity")).toBeVisible({ timeout: 10000 })

    const completeButton = page.getByRole("button", { name: /looks good/i })
    await expect(completeButton).toBeVisible({ timeout: 5000 })
    await completeButton.click()

    await expect(page.getByText("Your AppView is ready", { exact: true })).toBeVisible({ timeout: 5000 })
  })

  test("setup page redirects to dashboard after completion", async ({ page }) => {
    await loginAsTestAdmin(page)
    await page.goto("/setup")

    await expect(page).toHaveURL(/\/dashboard/, { timeout: 10000 })
  })
})
