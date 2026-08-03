import { test, expect } from "@playwright/test"
import pg from "pg"
import { loginAsTestAdmin } from "./auth-helper"

const DB_URL = "postgres://happyview:happyview@localhost:5434/happyview_test"

const TEST_NSID = "test.e2e.linkedrepos.item"
const TEST_LEXICON = {
  lexicon: 1,
  id: TEST_NSID,
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
      target_collection: TEST_NSID,
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
  await request.delete(`/admin/lexicons/${encodeURIComponent(TEST_NSID)}`)
}

// Defensive cleanup for the linked_repos rows this spec creates. The dashboard
// deletes a grant's session + auth-state rows transactionally when removed
// through the UI, but a test that fails mid-flow could leave one behind —
// so clean up by content (scopes referencing our seeded collection) rather
// than relying on the UI delete step having run.
async function cleanupLinkedRepos(): Promise<void> {
  const client = new pg.Client(DB_URL)
  await client.connect()
  try {
    const like = `%${TEST_NSID}%`
    await client.query(
      `DELETE FROM happyview_linked_repo_auth_state WHERE grant_id IN (
        SELECT id FROM happyview_linked_repos WHERE scopes LIKE $1
      )`,
      [like],
    )
    await client.query(
      `DELETE FROM happyview_linked_repo_sessions WHERE did IN (
        SELECT did FROM happyview_linked_repos WHERE scopes LIKE $1
      )`,
      [like],
    )
    await client.query("DELETE FROM happyview_linked_repos WHERE scopes LIKE $1", [
      like,
    ])
  } finally {
    await client.end()
  }
}

test.describe("Linked Repos", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page)
    await seedLexicon(page.request)
  })

  test.afterEach(async ({ page }) => {
    await cleanupLinkedRepos()
    await cleanupLexicon(page.request)
  })

  test("shows empty state when no linked repos exist", async ({ page }) => {
    await page.goto("/dashboard/settings/linked-repos")

    await expect(
      page.getByText("No linked repos yet."),
    ).toBeVisible({ timeout: 5000 })
  })

  test("creates an open grant via invite link, shows it pending, then deletes it", async ({
    page,
  }) => {
    await page.goto("/dashboard/settings/linked-repos")

    await page.getByRole("button", { name: "Link a repo" }).click()

    const dialog = page.getByRole("dialog")
    await expect(dialog).toBeVisible({ timeout: 3000 })

    // Leave the handle/DID field blank — this is an open grant. Build a scope
    // from the prefix + value pair: the builder no longer enumerates
    // collections, so nothing here depends on a lexicon being present.
    await dialog.getByLabel("Scope 1 prefix").fill("repo")
    await dialog.getByLabel("Scope 1 value").fill(`${TEST_NSID}?action=create`)

    const expectedScope = `repo:${TEST_NSID}?action=create`
    // `atproto` is mandatory in every AT Protocol authorization request, so the
    // builder always emits it — the preview must say so rather than the server
    // silently adding a scope the operator never saw.
    await expect(dialog.getByText(`atproto ${expectedScope}`)).toBeVisible()

    await dialog.getByRole("button", { name: "Create link" }).click()

    await expect(dialog.getByText("Invite link created")).toBeVisible({
      timeout: 5000,
    })
    const inviteUrlInput = dialog.locator("input[readonly]")
    await expect(inviteUrlInput).toBeVisible()
    await expect(inviteUrlInput).toHaveValue(
      /\/auth\/linked-repo\/start\?token=/,
    )

    await dialog.getByRole("button", { name: "Done" }).click()
    await expect(dialog).not.toBeVisible()

    const row = page.locator("table tbody tr", { hasText: expectedScope })
    await expect(row).toBeVisible({ timeout: 5000 })
    await expect(row.getByText("pending")).toBeVisible()
    await expect(row.getByText("1 link outstanding")).toBeVisible()
    await expect(row.getByText(expectedScope)).toBeVisible()

    // Row controls moved into a detail sheet: open it, then remove from there.
    await row.click()
    const detail = page.getByRole("dialog").filter({ hasText: "Invite links" })
    await expect(detail).toBeVisible({ timeout: 5000 })

    // The sheet shows the invite created above, and the scopes in full.
    await expect(detail.getByText("1 link outstanding")).toHaveCount(0)
    await expect(detail.getByText(expectedScope)).toBeVisible()
    await expect(
      detail.getByRole("button", { name: "Create a new invite link" }),
    ).toBeVisible()

    await detail.getByRole("button", { name: /remove this linked repo/i }).click()

    const confirm = page.getByRole("alertdialog")
    await expect(confirm).toBeVisible({ timeout: 3000 })
    await expect(confirm.getByText("Remove this linked repo?")).toBeVisible()
    await confirm.getByRole("button", { name: "Remove" }).click()

    await expect(
      page.getByText("No linked repos yet."),
    ).toBeVisible({ timeout: 5000 })
  })

  test("blocks submission when the raw scope field is malformed", async ({
    page,
  }) => {
    await page.goto("/dashboard/settings/linked-repos")

    await page.getByRole("button", { name: "Link a repo" }).click()

    const dialog = page.getByRole("dialog")
    await expect(dialog).toBeVisible({ timeout: 3000 })

    const prefix = dialog.getByLabel("Scope 1 prefix")
    const value = dialog.getByLabel("Scope 1 value")
    const createLinkButton = dialog.getByRole("button", { name: "Create link" })

    // Nothing entered: the builder still emits `atproto`, but that alone
    // authorizes nothing, so submission stays blocked.
    await expect(createLinkButton).toBeDisabled()

    // "foo" has too few NSID segments (the client validator is a
    // character-for-character port of the server's), so this is rejected before
    // the action list is even considered.
    await prefix.fill("repo")
    await value.fill("foo?action=frobnicate")
    await expect(dialog.getByText("invalid NSID: foo")).toBeVisible()
    await expect(createLinkButton).toBeDisabled()

    // "com" — also too few NSID segments, same rule, different token.
    await value.fill("com")
    await expect(dialog.getByText("invalid NSID: com")).toBeVisible()
    await expect(createLinkButton).toBeDisabled()

    // A prefix the server rejects outright.
    await prefix.fill("nonsense")
    await value.fill("thing")
    await expect(dialog.getByText("unknown scope prefix: nonsense")).toBeVisible()
    await expect(createLinkButton).toBeDisabled()

    // transition:generic is allowed, but warned about — the grant is long-lived
    // and its scopes can't be narrowed later.
    await prefix.fill("transition")
    await value.fill("generic")
    await expect(dialog.getByText(/grants broad write access/i)).toBeVisible()
    await expect(createLinkButton).toBeEnabled()
  })

  test("builds repo actions from checkboxes", async ({ page }) => {
    await page.goto("/dashboard/settings/linked-repos")
    await page.getByRole("button", { name: "Link a repo" }).click()

    const dialog = page.getByRole("dialog")
    await expect(dialog).toBeVisible({ timeout: 3000 })

    const createLinkButton = dialog.getByRole("button", { name: "Create link" })
    await dialog.getByLabel("Scope 1 prefix").fill("repo")
    await dialog.getByLabel("Scope 1 value").fill(TEST_NSID)

    // A collection with no actions serialises to `repo:<nsid>`, which the
    // server reads as create *and* update *and* delete. The builder refuses to
    // emit that by accident — scopes are immutable, so an over-broad grant made
    // by not choosing can only be fixed by re-linking the repo.
    await expect(dialog.getByText("select at least one action")).toBeVisible()
    await expect(createLinkButton).toBeDisabled()

    await dialog.getByLabel("create", { exact: true }).check()
    await expect(
      dialog.getByText(`atproto repo:${TEST_NSID}?action=create`),
    ).toBeVisible()
    await expect(createLinkButton).toBeEnabled()

    // Actions serialise in declaration order, not the order they were clicked,
    // so the same access always produces the same string.
    await dialog.getByLabel("delete", { exact: true }).check()
    await dialog.getByLabel("update", { exact: true }).check()
    await expect(
      dialog.getByText(`atproto repo:${TEST_NSID}?action=create,update,delete`),
    ).toBeVisible()

    // `update` without `create` can't upsert — flagged on the row that has the
    // problem rather than as a general note nobody reads.
    await dialog.getByLabel("create", { exact: true }).uncheck()
    await dialog.getByLabel("delete", { exact: true }).uncheck()
    await expect(dialog.getByText(/can't/).first()).toBeVisible()
    await expect(
      dialog.getByText(`atproto repo:${TEST_NSID}?action=update`),
    ).toBeVisible()
    // Still a legitimate grant, just a narrow one — warned about, not blocked.
    await expect(createLinkButton).toBeEnabled()
  })

  test("hoists a pasted action query into the checkboxes", async ({ page }) => {
    await page.goto("/dashboard/settings/linked-repos")
    await page.getByRole("button", { name: "Link a repo" }).click()

    const dialog = page.getByRole("dialog")
    await expect(dialog).toBeVisible({ timeout: 3000 })

    const value = dialog.getByLabel("Scope 1 value")
    await dialog.getByLabel("Scope 1 prefix").fill("repo")

    // Pasting a whole scope keeps working: the query is lifted into the boxes
    // and the field is left holding the collection alone, so there's never a
    // second, invisible copy of the action list.
    await value.fill(`${TEST_NSID}?action=create,update`)
    await expect(value).toHaveValue(TEST_NSID)
    await expect(dialog.getByLabel("create", { exact: true })).toBeChecked()
    await expect(dialog.getByLabel("update", { exact: true })).toBeChecked()
    await expect(dialog.getByLabel("delete", { exact: true })).not.toBeChecked()
    await expect(
      dialog.getByText(`atproto repo:${TEST_NSID}?action=create,update`),
    ).toBeVisible()

    // A malformed action list is left in the field so the error can name it,
    // rather than being silently swallowed by the checkboxes.
    await value.fill(`${TEST_NSID}?action=frobnicate`)
    await expect(value).toHaveValue(`${TEST_NSID}?action=frobnicate`)
    await expect(
      dialog.getByText("unknown repo action: frobnicate"),
    ).toBeVisible()
    await expect(
      dialog.getByRole("button", { name: "Create link" }),
    ).toBeDisabled()
  })
})
