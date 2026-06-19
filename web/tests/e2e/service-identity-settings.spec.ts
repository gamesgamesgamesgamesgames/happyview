import { test, expect } from "@playwright/test"
import { loginAsTestAdmin } from "./auth-helper"

test.describe("Service Identity Settings", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page)
    await page.goto("/dashboard/settings/service-identity")
  })

  test("add and remove a service entry", async ({ page }) => {
    const fragmentInput = page.getByLabel(/fragment/i)
    const typeInput = page.getByLabel(/service type/i)
    await expect(fragmentInput).toBeVisible({ timeout: 5000 })

    await fragmentInput.fill("#testentry")
    await typeInput.fill("TestAppView")

    const addButton = page.getByRole("button", { name: "Add" })
    await expect(addButton).toBeEnabled({ timeout: 3000 })
    await addButton.click()

    await expect(page.getByText("#testentry")).toBeVisible({ timeout: 5000 })

    const deleteButton = page.getByRole("button", { name: /delete #testentry/i })
    await expect(deleteButton).toBeVisible({ timeout: 3000 })
    await deleteButton.click()

    await expect(page.getByText("#testentry")).not.toBeVisible({ timeout: 5000 })
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
})
