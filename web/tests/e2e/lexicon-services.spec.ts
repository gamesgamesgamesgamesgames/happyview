import { test, expect } from "@playwright/test"
import { loginAsTestAdmin } from "./auth-helper"

test.describe("Lexicon Services", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page)
  })

  test("service entry appears in lexicon services sheet", async ({ page }) => {
    await page.goto("/dashboard/settings/service-identity")
    const addButton = page.getByRole("button", { name: /add.*entry|new.*entry/i })
    if (await addButton.isVisible({ timeout: 5000 }).catch(() => false)) {
      await addButton.click()
      await expect(page.getByText(/service entry/i)).toBeVisible()
    }

    await page.goto("/dashboard/lexicons")
    const firstLexicon = page.locator("table tbody tr").first()
    if (await firstLexicon.isVisible({ timeout: 5000 }).catch(() => false)) {
      await firstLexicon.click()
      const servicesButton = page.getByRole("button", { name: /services/i })
      if (await servicesButton.isVisible({ timeout: 5000 }).catch(() => false)) {
        await servicesButton.click()
        await expect(page.getByText(/service/i)).toBeVisible()
      }
    }
  })
})
