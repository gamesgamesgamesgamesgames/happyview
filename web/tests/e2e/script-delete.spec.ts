import { test, expect } from "@playwright/test"
import { loginAsTestAdmin } from "./auth-helper"

const TEST_LEXICON = {
  lexicon: 1,
  id: "test.e2e.scriptdelete.item",
  defs: {
    main: {
      type: "record",
      key: "tid",
      record: {
        type: "object",
        properties: {
          title: { type: "string" },
        },
      },
    },
  },
}

const TEST_SCRIPT_SUFFIX = "test.e2e.scriptdelete.item"
const TEST_SCRIPT_ID = `before_create:${TEST_SCRIPT_SUFFIX}`

async function seedScript(
  request: import("@playwright/test").APIRequestContext,
) {
  const lexResp = await request.post("/admin/lexicons", {
    data: { lexicon_json: TEST_LEXICON, backfill: false },
  })
  if (!lexResp.ok()) {
    const text = await lexResp.text()
    if (!text.includes("already exists")) {
      throw new Error(`Failed to seed lexicon: ${lexResp.status()} ${text}`)
    }
  }

  const resp = await request.post("/admin/scripts", {
    data: {
      id: TEST_SCRIPT_ID,
      code: 'return record',
      language: "lua",
    },
  })
  if (!resp.ok()) {
    const text = await resp.text()
    if (!text.includes("already exists")) {
      throw new Error(`Failed to seed script: ${resp.status()} ${text}`)
    }
  }
}

async function cleanupScript(
  request: import("@playwright/test").APIRequestContext,
) {
  await request.delete(`/admin/scripts/${encodeURIComponent(TEST_SCRIPT_ID)}`)
  await request.delete(`/admin/lexicons/${TEST_LEXICON.id}`)
}

test.describe("Script Delete", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page)
    await seedScript(page.request)
  })

  test.afterEach(async ({ page }) => {
    await cleanupScript(page.request)
  })

  test("delete shows AlertDialog instead of window.confirm", async ({ page }) => {
    await page.goto("/dashboard/settings/scripts")

    const row = page.locator("table tbody tr", { hasText: TEST_SCRIPT_SUFFIX })
    await expect(row).toBeVisible({ timeout: 5000 })

    const deleteButton = row.getByRole("button", { name: /delete script/i })
    await deleteButton.click()

    const dialog = page.getByRole("alertdialog")
    await expect(dialog).toBeVisible({ timeout: 3000 })
    await expect(dialog.getByText("Delete script?")).toBeVisible()
    await expect(dialog.getByText(/permanently remove/i)).toBeVisible()

    await dialog.getByRole("button", { name: "Cancel" }).click()
    await expect(dialog).not.toBeVisible()
    await expect(row).toBeVisible()

    await deleteButton.click()
    await expect(dialog).toBeVisible()
    await dialog.getByRole("button", { name: "Delete" }).click()

    await expect(page.getByText("Script deleted")).toBeVisible({ timeout: 5000 })
    await expect(row).not.toBeVisible({ timeout: 5000 })
  })
})
