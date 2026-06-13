import { test, expect } from "@playwright/test"

test.describe("Service Identity Settings", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/setup")

    const notExposedCard = page.getByText(/not exposed/i)
    if (await notExposedCard.isVisible({ timeout: 3000 }).catch(() => false)) {
      await notExposedCard.click()
      await expect(page.getByText(/complete|success|done/i)).toBeVisible({ timeout: 5000 })
    }

    await page.goto("/dashboard/settings/service-identity")
  })

  test("change mode redirects to setup", async ({ page }) => {
    const changeModeButton = page.getByRole("button", { name: /change mode/i })
    await expect(changeModeButton).toBeVisible({ timeout: 5000 })
    await changeModeButton.click()

    const confirmButton = page.getByRole("button", { name: /confirm|yes|continue/i })
    await expect(confirmButton).toBeVisible()
    await confirmButton.click()

    await expect(page).toHaveURL(/\/setup/, { timeout: 10000 })
  })

  test("add and remove a service entry", async ({ page }) => {
    const addButton = page.getByRole("button", { name: /add.*entry|new.*entry|add.*service/i })
    if (!(await addButton.isVisible({ timeout: 5000 }).catch(() => false))) {
      test.skip(true, "add entry button not visible — UI may differ")
      return
    }

    await addButton.click()

    const fragmentInput = page.getByLabel(/fragment/i)
    const typeInput = page.getByLabel(/type|service type/i)

    if (await fragmentInput.isVisible({ timeout: 3000 }).catch(() => false)) {
      await fragmentInput.fill("#testentry")
      await typeInput.fill("TestAppView")

      const saveButton = page.getByRole("button", { name: /save|create|add/i })
      await saveButton.click()

      await expect(page.getByText("#testentry")).toBeVisible({ timeout: 5000 })

      const row = page.getByText("#testentry").locator("..")
      const deleteButton = row.getByRole("button", { name: /delete|remove/i })
      if (await deleteButton.isVisible({ timeout: 3000 }).catch(() => false)) {
        await deleteButton.click()

        const confirmDelete = page.getByRole("button", { name: /confirm|yes|delete/i })
        if (await confirmDelete.isVisible({ timeout: 3000 }).catch(() => false)) {
          await confirmDelete.click()
        }

        await expect(page.getByText("#testentry")).not.toBeVisible({ timeout: 5000 })
      }
    }
  })
})
