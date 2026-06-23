import { test, expect } from "@playwright/test"
import { loginAsTestAdmin } from "./auth-helper"

const TEST_LEXICON = {
  lexicon: 1,
  id: "test.e2e.lexicondelete.item",
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

async function seedLexicon(
  request: import("@playwright/test").APIRequestContext,
) {
  const resp = await request.post("/admin/lexicons", {
    data: {
      lexicon_json: TEST_LEXICON,
      backfill: false,
    },
  })
  if (!resp.ok()) {
    const text = await resp.text()
    if (!text.includes("already exists")) {
      throw new Error(`Failed to seed lexicon: ${resp.status()} ${text}`)
    }
  }
}

async function cleanupLexicon(
  request: import("@playwright/test").APIRequestContext,
) {
  await request.delete(`/admin/lexicons/${TEST_LEXICON.id}`)
}

test.describe("Lexicon Delete", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page)
    await seedLexicon(page.request)
  })

  test.afterEach(async ({ page }) => {
    await cleanupLexicon(page.request)
  })

  test("delete shows AlertDialog confirmation and succeeds", async ({ page }) => {
    await page.goto("/dashboard/lexicons")

    const row = page.locator("table tbody tr", { hasText: TEST_LEXICON.id })
    await expect(row).toBeVisible({ timeout: 5000 })

    const deleteButton = row.getByRole("button", { name: /delete lexicon/i })
    await deleteButton.click()

    const dialog = page.getByRole("alertdialog")
    await expect(dialog).toBeVisible({ timeout: 3000 })
    await expect(dialog.getByText("Delete lexicon?")).toBeVisible()
    await expect(dialog.getByText(/permanently remove/i)).toBeVisible()

    await dialog.getByRole("button", { name: "Cancel" }).click()
    await expect(dialog).not.toBeVisible()
    await expect(row).toBeVisible()

    await deleteButton.click()
    await expect(dialog).toBeVisible()
    await dialog.getByRole("button", { name: "Delete" }).click()

    await expect(page.getByText("Lexicon deleted")).toBeVisible({ timeout: 5000 })
    await expect(row).not.toBeVisible({ timeout: 5000 })
  })
})
