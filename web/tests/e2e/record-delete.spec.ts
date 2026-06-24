import { test, expect } from "@playwright/test"
import { loginAsTestAdmin } from "./auth-helper"
import pg from "pg"

const DB_URL = "postgres://happyview:happyview@localhost:5434/happyview_test"
const COLLECTION = "test.e2e.recorddelete.item"

const TEST_LEXICON = {
  lexicon: 1,
  id: COLLECTION,
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

async function seedRecords(
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

  const client = new pg.Client(DB_URL)
  await client.connect()
  try {
    for (let i = 1; i <= 3; i++) {
      const uri = `at://did:plc:e2e-test-admin/${COLLECTION}/record-${i}`
      const now = new Date().toISOString()
      await client.query(
        `INSERT INTO records (uri, did, collection, rkey, record, cid, indexed_at, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (uri) DO NOTHING`,
        [
          uri,
          "did:plc:e2e-test-admin",
          COLLECTION,
          `record-${i}`,
          JSON.stringify({ title: `Test Record ${i}` }),
          `cid-e2e-${i}`,
          now,
          now,
        ],
      )
    }
  } finally {
    await client.end()
  }
}

async function cleanupRecords(
  request: import("@playwright/test").APIRequestContext,
) {
  const client = new pg.Client(DB_URL)
  await client.connect()
  try {
    await client.query("DELETE FROM records WHERE collection = $1", [COLLECTION])
  } finally {
    await client.end()
  }
  await request.delete(`/admin/lexicons/${COLLECTION}`)
}

test.describe("Record Delete", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page)
    await seedRecords(page.request)
  })

  test.afterEach(async ({ page }) => {
    await cleanupRecords(page.request)
  })

  test("single record delete shows AlertDialog and toasts on success", async ({ page }) => {
    await page.goto(`/dashboard/records?collection=${encodeURIComponent(COLLECTION)}`)

    const firstRow = page.locator("table tbody tr").first()
    await expect(firstRow).toBeVisible({ timeout: 5000 })
    await firstRow.click()

    const sheet = page.locator("[data-slot='sheet-content']")
    await expect(sheet).toBeVisible({ timeout: 5000 })

    const deleteRecordButton = sheet.getByRole("button", { name: /delete record/i })
    await deleteRecordButton.click()

    const dialog = page.getByRole("alertdialog")
    await expect(dialog).toBeVisible({ timeout: 3000 })
    await expect(dialog.getByText("Delete record?")).toBeVisible()

    await dialog.getByRole("button", { name: "Cancel" }).click()
    await expect(dialog).not.toBeVisible()

    await deleteRecordButton.click()
    await expect(dialog).toBeVisible()
    await dialog.getByRole("button", { name: "Delete" }).click()

    await expect(page.getByText("Record deleted")).toBeVisible({ timeout: 5000 })
  })

  test("bulk delete uses Promise.allSettled with success toast", async ({ page }) => {
    await page.goto(`/dashboard/records?collection=${encodeURIComponent(COLLECTION)}`)

    await expect(page.locator("table tbody tr").first()).toBeVisible({ timeout: 5000 })

    const selectAll = page.locator("table thead").getByRole("checkbox", { name: /select all/i })
    await selectAll.click()

    const actionsButton = page.getByRole("button", { name: "Actions" })
    await actionsButton.click()

    const deleteMenuItem = page.getByRole("menuitem", { name: /delete/i })
    await deleteMenuItem.click()

    const dialog = page.locator("[data-slot='dialog-content'], [role='dialog']").first()
    await expect(dialog).toBeVisible({ timeout: 3000 })

    const confirmButton = dialog.getByRole("button", { name: /^delete$/i })
    await confirmButton.click()

    await expect(page.getByText(/deleted 3 records/i)).toBeVisible({ timeout: 10000 })
  })
})
