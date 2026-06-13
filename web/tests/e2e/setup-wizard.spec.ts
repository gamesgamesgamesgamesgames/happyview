import { test, expect } from "@playwright/test"

test.describe("Setup Wizard", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/setup")
  })

  test("did:web flow completes successfully", async ({ page }) => {
    await page.getByText("did:web").click()

    await expect(page.getByText("Configure")).toBeVisible()

    const continueButton = page.getByRole("button", { name: /continue|next|save/i })
    if (await continueButton.isVisible()) {
      await continueButton.click()
    }

    await expect(page.getByText(/verify|review/i)).toBeVisible({ timeout: 10000 })

    await expect(page.getByText("did:web:")).toBeVisible({ timeout: 5000 })

    const completeButton = page.getByRole("button", { name: /looks good|complete|continue/i })
    await expect(completeButton).toBeVisible({ timeout: 5000 })
    await completeButton.click()

    await expect(page.getByText(/complete|success|done/i)).toBeVisible({ timeout: 5000 })
  })

  test("not_exposed skips to complete", async ({ page }) => {
    await page.getByText(/not exposed/i).click()

    await expect(page.getByText(/complete|success|done/i)).toBeVisible({ timeout: 5000 })
  })

  test("setup resumes at correct step after page reload", async ({ page }) => {
    await page.getByText("did:web").click()

    await expect(page.getByText("Configure")).toBeVisible()

    const continueButton = page.getByRole("button", { name: /continue|next|save/i })
    if (await continueButton.isVisible({ timeout: 3000 }).catch(() => false)) {
      await continueButton.click()
    }

    await expect(page.getByText(/verify|review/i)).toBeVisible({ timeout: 10000 })

    await page.reload()

    await expect(page.getByText(/verify|review/i)).toBeVisible({ timeout: 10000 })
  })
})
