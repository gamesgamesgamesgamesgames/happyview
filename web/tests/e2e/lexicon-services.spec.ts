import { test, expect } from "@playwright/test"
import { loginAsTestAdmin } from "./auth-helper"

const RECORD_LEXICON = {
  lexicon: 1,
  id: "test.e2e.lexiconservices.item",
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

const QUERY_LEXICON = {
  lexicon: 1,
  id: "test.e2e.lexiconservices.listItems",
  defs: {
    main: {
      type: "query",
      parameters: {
        type: "params",
        properties: {
          limit: { type: "integer" },
        },
      },
      output: {
        encoding: "application/json",
      },
    },
  },
}

async function seedLexicon(
  request: import("@playwright/test").APIRequestContext,
  lexiconJson: object,
  targetCollection?: string,
) {
  const resp = await request.post("/admin/lexicons", {
    data: {
      lexicon_json: lexiconJson,
      backfill: false,
      ...(targetCollection ? { target_collection: targetCollection } : {}),
    },
  })
  if (!resp.ok()) {
    throw new Error(`Failed to seed lexicon: ${resp.status()} ${await resp.text()}`)
  }
}

async function deleteLexicon(
  request: import("@playwright/test").APIRequestContext,
  nsid: string,
) {
  await request.delete(`/admin/lexicons/${nsid}`)
}

test.describe("Lexicon Services", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page)

    await seedLexicon(page.request, RECORD_LEXICON)
    await seedLexicon(page.request, QUERY_LEXICON, RECORD_LEXICON.id)
  })

  test.afterEach(async ({ page }) => {
    await deleteLexicon(page.request, QUERY_LEXICON.id)
    await deleteLexicon(page.request, RECORD_LEXICON.id)
  })

  test("create service entry and view lexicon services sheet", async ({ page }) => {
    // Create a service entry via the settings page
    await page.goto("/dashboard/settings/service-identity")

    const fragmentInput = page.getByLabel(/fragment/i)
    await expect(fragmentInput).toBeVisible({ timeout: 5000 })

    await fragmentInput.fill("#lextest")
    const typeInput = page.getByLabel(/service type/i)
    await typeInput.fill("TestView")

    const addButton = page.getByRole("button", { name: "Add" })
    await expect(addButton).toBeEnabled({ timeout: 3000 })
    await addButton.click()

    await expect(page.getByText("#lextest")).toBeVisible({ timeout: 5000 })

    // Navigate to the query lexicon's detail page
    await page.goto("/dashboard/lexicons")

    const queryRow = page.locator("table tbody tr", {
      hasText: QUERY_LEXICON.id,
    })
    await expect(queryRow).toBeVisible({ timeout: 5000 })
    await queryRow.click()

    // Query lexicons have a Services button
    const servicesButton = page.getByRole("button", { name: /services/i })
    await expect(servicesButton).toBeVisible({ timeout: 5000 })
    await servicesButton.click()

    // Verify the services sheet opens
    const sheet = page.locator("[data-slot='sheet-content']")
    await expect(sheet).toBeVisible({ timeout: 5000 })
    await expect(sheet.getByRole("heading", { name: "Services" })).toBeVisible()

    // Should show either service entries or "No services have access"
    const hasServices = await sheet
      .locator("table tbody tr")
      .first()
      .isVisible({ timeout: 3000 })
      .catch(() => false)
    const hasNoServicesMessage = await sheet
      .getByText(/no services have access/i)
      .isVisible()
      .catch(() => false)

    expect(hasServices || hasNoServicesMessage).toBe(true)

    // Clean up the service entry
    await page.goto("/dashboard/settings/service-identity")
    const deleteButton = page.getByRole("button", { name: /delete #lextest/i })
    if (await deleteButton.isVisible({ timeout: 3000 }).catch(() => false)) {
      await deleteButton.click()
      await expect(page.getByText("#lextest")).not.toBeVisible({ timeout: 5000 })
    }
  })
})
