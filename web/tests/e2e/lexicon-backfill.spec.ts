import { test, expect } from "@playwright/test"
import { loginAsTestAdmin } from "./auth-helper"

function lexicon(id: string) {
  return {
    lexicon: 1,
    id,
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
}

/// Backfill jobs are never deleted with their lexicon, so counts accumulate
/// across runs. Always compare against a count taken in the same test.
async function jobCountForCollection(
  request: import("@playwright/test").APIRequestContext,
  collection: string,
) {
  const resp = await request.get("/admin/backfill/status")
  expect(resp.ok()).toBeTruthy()
  const jobs = (await resp.json()) as { collection: string | null }[]
  return jobs.filter((j) => j.collection === collection).length
}

test.describe("Lexicon-triggered backfill", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page)
  })

  test("uploading a record lexicon creates a job on the Backfill page", async ({
    page,
  }) => {
    const id = "test.e2e.lexiconbackfill.created"
    // A first upload (revision 1) is what starts a backfill, so clear any
    // lexicon left behind by an earlier run.
    await page.request.delete(`/admin/lexicons/${id}`)

    const resp = await page.request.post("/admin/lexicons", {
      data: { lexicon_json: lexicon(id), backfill: true },
    })
    expect(resp.status()).toBe(201)

    const jobId = (await resp.json()).backfill_job_id as string
    expect(jobId).toBeTruthy()

    await page.goto("/dashboard/backfill")

    const row = page.locator("table tbody tr", { hasText: jobId.slice(0, 8) })
    await expect(row).toBeVisible({ timeout: 10000 })
    await expect(row).toContainText(id)

    await page.request.delete(`/admin/lexicons/${id}`)
  })

  test("re-uploading the same lexicon does not add a second job", async ({
    page,
  }) => {
    const id = "test.e2e.lexiconbackfill.reupload"
    await page.request.delete(`/admin/lexicons/${id}`)

    const first = await page.request.post("/admin/lexicons", {
      data: { lexicon_json: lexicon(id), backfill: true },
    })
    expect(first.status()).toBe(201)
    expect((await first.json()).backfill_job_id).toBeTruthy()

    const before = await jobCountForCollection(page.request, id)

    const second = await page.request.post("/admin/lexicons", {
      data: { lexicon_json: lexicon(id), backfill: true },
    })
    expect(second.status()).toBe(200)
    expect((await second.json()).backfill_job_id).toBeNull()

    expect(await jobCountForCollection(page.request, id)).toBe(before)

    await page.request.delete(`/admin/lexicons/${id}`)
  })
})
