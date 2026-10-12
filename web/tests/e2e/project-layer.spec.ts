import { test, expect } from "@playwright/test"
import { loginAsTestAdmin } from "./auth-helper"

// The e2e stack runs no project, so the API's answer for a project-owned entry
// is supplied here. What this pins is how the dashboard renders an entry the
// API says a file owns.

const SCRIPT_ID = "xrpc.query:test.e2e.project.thing"
const SCRIPT_PATH = "src/lexicons/test/e2e/project/thing.lua"
const LEXICON_ID = "test.e2e.project.thing"
const LEXICON_PATH = "src/lexicons/test/e2e/project/thing.json"

const projectScript = {
  id: SCRIPT_ID,
  script_type: "lua",
  body: "function handle(input, ctx)\n  return {}\nend\n",
  description: null,
  outbound_xrpcs: null,
  created_at: null,
  updated_at: null,
  recreatable: true,
  needs_migration: [],
  runnable: true,
  layer: "project",
  path: SCRIPT_PATH,
  shadows_stored: true,
}

const projectLexicon = {
  id: LEXICON_ID,
  revision: 0,
  lexicon_type: "query",
  backfill: false,
  action: null,
  target_collection: null,
  source: "project",
  layer: "project",
  path: LEXICON_PATH,
  shadows_stored: false,
  authority_did: null,
  last_fetched_at: null,
  created_at: null,
  updated_at: null,
  token_cost: null,
  lexicon_json: {
    lexicon: 1,
    id: LEXICON_ID,
    defs: { main: { type: "query", output: { encoding: "application/json" } } },
  },
}

test.describe("Project-owned entries", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page)
  })

  test("a project script renders read-only with its file path", async ({ page }) => {
    await page.route(
      (url) => url.pathname.endsWith(`/admin/scripts/${encodeURIComponent(SCRIPT_ID)}`),
      async (route) => {
        if (route.request().method() !== "GET") return route.continue()
        await route.fulfill({ json: projectScript })
      },
    )

    await page.goto(`/dashboard/settings/scripts/${encodeURIComponent(SCRIPT_ID)}`)

    const footer = page.locator("footer")
    await expect(footer.getByText(SCRIPT_PATH)).toBeVisible({ timeout: 5000 })
    await expect(footer.getByRole("button", { name: /^save/i })).toHaveCount(0)
    await expect(footer.getByRole("button", { name: /delete script/i })).toHaveCount(0)
    await expect(page.locator("#description")).toHaveAttribute("readonly", "")
    await expect(page.getByText(/dashboard version of this script is kept/i)).toBeVisible()
  })

  test("the scripts list offers no delete for a project script", async ({ page }) => {
    await page.route(
      (url) => url.pathname.endsWith("/admin/scripts") && !url.searchParams.has("suffix"),
      async (route) => {
        if (route.request().method() !== "GET") return route.continue()
        await route.fulfill({ json: [projectScript] })
      },
    )

    await page.goto("/dashboard/settings/scripts")

    const row = page.locator("table tbody tr", { hasText: "test.e2e.project.thing" })
    await expect(row).toBeVisible({ timeout: 5000 })
    await expect(row.getByText("Project file")).toBeVisible()
    await expect(row.getByRole("button", { name: /delete script/i })).toHaveCount(0)
  })

  test("a project lexicon renders read-only with its file path", async ({ page }) => {
    await page.route(
      (url) => url.pathname.endsWith(`/admin/lexicons/${LEXICON_ID}`),
      async (route) => {
        if (route.request().method() !== "GET") return route.continue()
        await route.fulfill({ json: projectLexicon })
      },
    )
    await page.route(
      (url) => url.pathname.endsWith("/admin/scripts") && url.searchParams.has("suffix"),
      (route) => route.fulfill({ json: [] }),
    )

    await page.goto(`/dashboard/lexicons/${LEXICON_ID}`)

    const footer = page.locator("footer")
    await expect(footer.getByText(LEXICON_PATH)).toBeVisible({ timeout: 5000 })
    await expect(footer.getByRole("button", { name: /^save/i })).toHaveCount(0)
    await expect(footer.getByRole("button", { name: /delete lexicon/i })).toHaveCount(0)
    await expect(page.locator("#token-cost")).toBeDisabled()
  })

  test("the lexicons list offers no delete for a project lexicon", async ({ page }) => {
    await page.route(
      (url) => url.pathname.endsWith("/admin/lexicons"),
      async (route) => {
        if (route.request().method() !== "GET") return route.continue()
        await route.fulfill({ json: [projectLexicon] })
      },
    )

    await page.goto("/dashboard/lexicons")

    const row = page.locator("table tbody tr", { hasText: LEXICON_ID })
    await expect(row).toBeVisible({ timeout: 5000 })
    await expect(row.getByRole("button", { name: /delete/i })).toHaveCount(0)
  })
})
