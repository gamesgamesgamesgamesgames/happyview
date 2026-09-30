import { test, expect } from "@playwright/test"
import { loginAsTestAdmin } from "./auth-helper"

const IDENTITIES: Record<string, { did: string; handle: string }> = {
  "alice.test": { did: "did:plc:e2ealice", handle: "alice.test" },
  "did:plc:e2ebob": { did: "did:plc:e2ebob", handle: "bob.test" },
}

test.describe("Backfill for specific accounts", () => {
  test.beforeEach(async ({ page }) => {
    await page.route(
      "**/xrpc/tech.waow.typeahead.searchActors**",
      (route) =>
        route.fulfill({
          json: {
            actors: [
              {
                did: "did:plc:e2ealice",
                handle: "alice.test",
                displayName: "Alice",
              },
            ],
          },
        }),
    )
    await page.route("**/admin/identity/resolve?**", (route) => {
      const identifier = new URL(route.request().url()).searchParams.get(
        "identifier",
      )
      const identity = identifier ? IDENTITIES[identifier] : undefined
      return identity
        ? route.fulfill({ json: identity })
        : route.fulfill({
            status: 400,
            json: { error: `could not resolve handle ${identifier}` },
          })
    })

    await loginAsTestAdmin(page)
    await page.goto("/dashboard/backfill")
  })

  test("creates one job for a suggested handle and a typed DID", async ({
    page,
  }) => {
    await page.getByRole("button", { name: "Create Backfill Job" }).click()

    const input = page.getByLabel("Accounts (optional)")
    await input.fill("ali")
    await page.getByRole("option", { name: /Alice/ }).click()

    await input.fill("did:plc:e2ebob")
    await input.press("Enter")

    const chips = page.locator('[data-slot="combobox-chip"]')
    await expect(
      page.locator('[data-slot="combobox-chip"][data-status="resolved"]'),
    ).toHaveCount(2)
    await expect(chips.nth(0)).toContainText("@alice.test")
    await expect(chips.nth(0)).toContainText("did:plc:e2ealice")
    await expect(chips.nth(1)).toContainText("@bob.test")

    const request = page.waitForRequest(
      (r) => r.url().endsWith("/admin/backfill") && r.method() === "POST",
    )
    await page.getByRole("button", { name: "Create", exact: true }).click()
    expect((await request).postDataJSON().dids).toEqual([
      "did:plc:e2ealice",
      "did:plc:e2ebob",
    ])

    await expect(page.getByText("Backfill job created")).toBeVisible()
    await expect(page.locator("table tbody tr").first()).toContainText(
      "2 accounts",
    )
  })

  test("an account that cannot be resolved blocks creation", async ({
    page,
  }) => {
    await page.getByRole("button", { name: "Create Backfill Job" }).click()

    const input = page.getByLabel("Accounts (optional)")
    await input.fill("nobody.invalid")
    await input.press("Enter")

    await expect(page.locator('[data-status="error"]')).toHaveCount(1)
    await expect(
      page.getByRole("button", { name: "Create", exact: true }),
    ).toBeDisabled()
  })

  test("choosing the same suggested account twice leaves one tag", async ({
    page,
  }) => {
    await page.getByRole("button", { name: "Create Backfill Job" }).click()

    const input = page.getByLabel("Accounts (optional)")
    await input.fill("ali")
    await page.getByRole("option", { name: /Alice/ }).click()

    await input.fill("ali")
    await page.getByRole("option", { name: /Alice/ }).click()

    await expect(
      page.locator('[data-slot="combobox-chip"][data-status="resolved"]'),
    ).toHaveCount(1)
  })

  test("Escape while reopening suggestions does not drop an already-added account", async ({
    page,
  }) => {
    await page.getByRole("button", { name: "Create Backfill Job" }).click()

    const input = page.getByLabel("Accounts (optional)")
    await input.fill("ali")
    await page.getByRole("option", { name: /Alice/ }).click()
    await expect(
      page.locator('[data-slot="combobox-chip"][data-status="resolved"]'),
    ).toHaveCount(1)

    await input.fill("ali")
    await expect(page.getByRole("option", { name: /Alice/ })).toBeVisible()
    await input.press("Escape")

    await expect(
      page.locator('[data-slot="combobox-chip"][data-status="resolved"]'),
    ).toHaveCount(1)
  })
})
