import { test, expect } from "@playwright/test"
import { loginAsTestAdmin } from "./auth-helper"

test.describe("Service Identity Settings", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page)
    await page.goto("/dashboard/settings/service-identity")
  })

  test("manage service entry access mode and xrpcs", async ({ page }) => {
    // Create an entry to work with
    const fragmentInput = page.getByLabel(/fragment/i)
    const typeInput = page.getByLabel(/service type/i)
    await expect(fragmentInput).toBeVisible({ timeout: 5000 })

    await fragmentInput.fill("#e2esheet")
    await typeInput.fill("TestView")

    const mainAddButton = page.getByRole("button", { name: "Add" })
    await expect(mainAddButton).toBeEnabled({ timeout: 3000 })
    await mainAddButton.click()

    // Wait for the entry to appear in the table
    await expect(page.getByText("#e2esheet")).toBeVisible({ timeout: 5000 })

    // Click the fragment ID link to open the sheet
    await page.getByText("#e2esheet").click()

    // Verify the sheet opens with the correct title
    const sheet = page.locator("[data-slot='sheet-content']")
    await expect(sheet).toBeVisible({ timeout: 5000 })
    await expect(sheet.getByText("#e2esheet")).toBeVisible()

    // Click "Specific XRPCs" to change access mode
    const specificButton = sheet.getByRole("button", { name: "Specific XRPCs" })
    await specificButton.click()

    // Fill the XRPC input (placeholder "games.birb.chess.getGame")
    const xrpcInput = sheet.getByPlaceholder("games.birb.chess.getGame")
    await expect(xrpcInput).toBeVisible({ timeout: 3000 })
    await xrpcInput.fill("com.example.test.query")

    // Click the "Add" button inside the sheet
    const sheetAddButton = sheet.getByRole("button", { name: "Add" })
    await sheetAddButton.click()

    // Verify the XRPC appears in the sheet's table
    await expect(sheet.getByText("com.example.test.query")).toBeVisible({ timeout: 5000 })

    // Click Save
    const saveButton = sheet.getByRole("button", { name: "Save" })
    await saveButton.click()

    // Wait for the sheet to close
    await expect(sheet).not.toBeVisible({ timeout: 5000 })

    // Reload and reopen to verify persistence
    await page.reload()
    await expect(page.getByText("#e2esheet")).toBeVisible({ timeout: 5000 })
    await page.getByText("#e2esheet").click()

    const sheetAfterReload = page.locator("[data-slot='sheet-content']")
    await expect(sheetAfterReload).toBeVisible({ timeout: 5000 })
    await expect(sheetAfterReload.getByText("com.example.test.query")).toBeVisible({ timeout: 5000 })

    // Clean up: delete the entry using the button in the sheet footer
    const deleteButton = sheetAfterReload.getByRole("button", { name: /delete service/i })
    await deleteButton.click()

    // Wait for sheet to close, then verify entry is removed from the table
    await expect(sheetAfterReload).not.toBeVisible({ timeout: 5000 })
    await expect(page.getByRole("button", { name: /delete #e2esheet/i })).not.toBeVisible({ timeout: 5000 })
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
