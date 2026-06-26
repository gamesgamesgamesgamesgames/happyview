import { test, expect } from "@playwright/test"
import { loginAsTestAdmin } from "./auth-helper"

test.describe("Proxy Config Settings", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page)
    await page.goto("/dashboard/settings/xrpc-proxy")
  })

  test("displays current proxy mode", async ({ page }) => {
    const openRadio = page.locator("input[type='radio'][value='open']")
    await expect(openRadio).toBeVisible({ timeout: 5000 })
    await expect(openRadio).toBeChecked()
  })

  test("switch to allowlist mode and add pattern", async ({ page }) => {
    // Select Allowlist radio
    const allowlistRadio = page.locator("input[type='radio'][value='allowlist']")
    await expect(allowlistRadio).toBeVisible({ timeout: 5000 })
    await allowlistRadio.check()

    // Verify NSID input appears
    const nsidInput = page.getByPlaceholder("com.example.feed.*")
    await expect(nsidInput.first()).toBeVisible({ timeout: 3000 })

    // Fill with a pattern
    await nsidInput.first().fill("com.example.*")

    // Save
    const saveButton = page.getByRole("button", { name: "Save changes" })
    await saveButton.click()

    // Wait for success notice
    await expect(page.getByText("Proxy settings saved.")).toBeVisible({ timeout: 5000 })

    // Reload and verify persistence
    await page.reload()

    const allowlistAfterReload = page.locator("input[type='radio'][value='allowlist']")
    await expect(allowlistAfterReload).toBeChecked({ timeout: 5000 })

    // Verify the pattern persisted
    const nsidInputAfterReload = page.getByPlaceholder("com.example.feed.*").first()
    await expect(nsidInputAfterReload).toHaveValue("com.example.*")
  })

  test("switch to disabled mode", async ({ page }) => {
    // Select Disabled radio
    const disabledRadio = page.locator("input[type='radio'][value='disabled']")
    await expect(disabledRadio).toBeVisible({ timeout: 5000 })
    await disabledRadio.check()

    // Save
    const saveButton = page.getByRole("button", { name: "Save changes" })
    await saveButton.click()

    // Wait for success notice
    await expect(page.getByText("Proxy settings saved.")).toBeVisible({ timeout: 5000 })

    // Reload and verify persistence
    await page.reload()

    const disabledAfterReload = page.locator("input[type='radio'][value='disabled']")
    await expect(disabledAfterReload).toBeChecked({ timeout: 5000 })

    // Restore to Open mode for subsequent tests
    const openRadio = page.locator("input[type='radio'][value='open']")
    await openRadio.check()
    await page.getByRole("button", { name: "Save changes" }).click()
    await expect(page.getByText("Proxy settings saved.")).toBeVisible({ timeout: 5000 })
  })
})
