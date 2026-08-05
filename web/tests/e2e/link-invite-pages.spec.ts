import { test, expect } from "@playwright/test"
import { loginAsTestAdmin } from "./auth-helper"

const TEST_NSID = "test.e2e.linkinvite.item"
const TEST_LEXICON = {
  lexicon: 1,
  id: TEST_NSID,
  defs: {
    main: {
      type: "record",
      key: "tid",
      record: { type: "object", properties: { title: { type: "string" } } },
    },
  },
}

/**
 * These pages are what somebody OUTSIDE this instance sees when an admin asks
 * them for repo access. They deliberately sit outside /dashboard, so they must
 * render with no session at all — every test here runs unauthenticated except
 * the setup that mints the invite.
 */
test.describe("Linked Repos — invitee-facing pages", () => {
  let inviteToken: string
  let grantId: string

  test.beforeAll(async ({ browser }) => {
    const page = await browser.newPage()
    await loginAsTestAdmin(page)

    const lex = await page.request.post("/admin/lexicons", {
      data: {
        lexicon_json: TEST_LEXICON,
        backfill: false,
        target_collection: TEST_NSID,
      },
    })
    if (!lex.ok() && !(await lex.text()).includes("already exists")) {
      throw new Error(`lexicon seed failed: ${lex.status()}`)
    }

    const created = await page.request.post("/admin/linked-repos", {
      data: {
        reason: "Mirror published notes",
        scopes: `atproto repo:${TEST_NSID}?action=create,update`,
      },
    })
    expect(created.ok()).toBeTruthy()
    const grant = await created.json()

    const invited = await page.request.post(
      `/admin/linked-repos/${grant.id}/invite`,
      { data: {} },
    )
    expect(invited.ok()).toBeTruthy()
    const { invite_url } = await invited.json()
    inviteToken = invite_url.split("token=")[1]
    expect(inviteToken).toBeTruthy()

    grantId = grant.id
    await page.close()
  })

  test.afterAll(async ({ browser }) => {
    if (!grantId) return
    const page = await browser.newPage()
    await loginAsTestAdmin(page)
    await page.request.delete(`/admin/linked-repos/${grantId}`)
    await page.close()
  })

  test("the invite link lands on a page explaining who is asking and for what", async ({
    page,
  }) => {
    // No login — this is a stranger following a link out of a message.
    await page.goto(`/auth/linked-repo/start?token=${inviteToken}`)

    // The server hands off to the human-facing landing page rather than
    // rendering a bare form.
    await expect(page).toHaveURL(/\/link\/start\/?\?token=/)

    // It must name the collection being requested in readable terms, not just
    // dump the raw scope string.
    await expect(page.getByText(TEST_NSID).first()).toBeVisible({
      timeout: 10000,
    })
    // The admin's stated reason is shown.
    await expect(page.getByText("Mirror published notes")).toBeVisible()
    // The grant is open, so it must ask which account to link.
    await expect(page.getByPlaceholder("you.bsky.social")).toBeVisible()
  })

  test("an unusable token explains itself instead of erroring", async ({
    page,
  }) => {
    await page.goto("/link/start?token=not-a-real-token")
    await expect(
      page.getByText(/no longer usable|expired|already been used/i).first(),
    ).toBeVisible({ timeout: 10000 })
    // Nothing to type into — retrying cannot help.
    await expect(page.getByPlaceholder("you.bsky.social")).toHaveCount(0)
  })

  test("the success result names the linked account", async ({ page }) => {
    await page.goto("/link/result?status=success&handle=alice.test")
    await expect(page.getByRole("heading", { name: "Repo linked" })).toBeVisible()
    await expect(page.getByText("alice.test").first()).toBeVisible()
  })

  test("the mismatch result tells them to retry with the right account", async ({
    page,
  }) => {
    await page.goto("/link/result?status=mismatch")
    await expect(
      page.getByRole("heading", { name: "Wrong account authorized" }),
    ).toBeVisible()
  })

  test("an unknown status falls back to generic failure copy", async ({
    page,
  }) => {
    await page.goto("/link/result?status=wat")
    await expect(
      page.getByRole("heading", { name: "Something went wrong" }),
    ).toBeVisible()
  })
})
