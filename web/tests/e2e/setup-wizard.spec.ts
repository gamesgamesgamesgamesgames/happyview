import { test, expect } from "@playwright/test"

test.describe("Setup Wizard", () => {
  test("did:web flow completes successfully", async ({ page }) => {
    await page.goto("/setup")

    await page.getByText("did:web").click()
    await page.getByRole("button", { name: /continue/i }).click()

    await expect(page.getByText("Configure did:web")).toBeVisible({ timeout: 5000 })
    await page.getByRole("button", { name: /continue/i }).click()

    await expect(page.getByText("Verify DID Document")).toBeVisible({ timeout: 10000 })

    // Verify the wizard resumes at the correct step after reload
    await page.reload()
    await expect(page.getByText("Verify DID Document")).toBeVisible({ timeout: 10000 })

    const completeButton = page.getByRole("button", { name: /looks good/i })
    await expect(completeButton).toBeVisible({ timeout: 5000 })
    await completeButton.click()

    await expect(page.getByText("Setup Complete")).toBeVisible({ timeout: 5000 })
  })

  test("setup page redirects to dashboard after completion", async ({ page }) => {
    await page.goto("/setup")

    await expect(page).toHaveURL(/\/dashboard/, { timeout: 10000 })
  })
})
