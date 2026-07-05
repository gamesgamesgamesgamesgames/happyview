import { test, expect } from "@playwright/test"
import { randomUUID } from "crypto"
import pg from "pg"
import { loginAsTestAdmin } from "./auth-helper"

const DB_URL = "postgres://happyview:happyview@localhost:5434/happyview_test"
const TEST_DID = "did:plc:e2e-test-admin"

async function seedJob(
  status: string,
  jobType = "test.e2e.export",
): Promise<string> {
  const client = new pg.Client(DB_URL)
  await client.connect()
  try {
    const id = randomUUID()
    const now = new Date().toISOString()
    await client.query(
      `INSERT INTO happyview_jobs (id, job_type, status, input, progress, created_by, created_at)
       VALUES ($1, $2, $3, $4, $5, $6, $7)`,
      [
        id,
        jobType,
        status,
        JSON.stringify({ source: "e2e-test" }),
        JSON.stringify({}),
        TEST_DID,
        now,
      ],
    )
    return id
  } finally {
    await client.end()
  }
}

async function cleanupJobs(): Promise<void> {
  const client = new pg.Client(DB_URL)
  await client.connect()
  try {
    await client.query(
      "DELETE FROM happyview_jobs WHERE created_by = $1",
      [TEST_DID],
    )
  } finally {
    await client.end()
  }
}

test.describe("Jobs Dashboard", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page)
  })

  test.afterEach(async () => {
    await cleanupJobs()
  })

  test("shows empty state when no jobs exist", async ({ page }) => {
    await page.goto("/dashboard/jobs")

    await expect(
      page.getByText("No background jobs yet"),
    ).toBeVisible({ timeout: 5000 })
  })

  test("lists seeded jobs in the table", async ({ page }) => {
    await seedJob("pending", "test.e2e.alpha")
    await seedJob("running", "test.e2e.beta")

    await page.goto("/dashboard/jobs")

    const rows = page.locator("table tbody tr")
    await expect(rows).toHaveCount(2, { timeout: 5000 })

    await expect(page.getByText("test.e2e.alpha")).toBeVisible()
    await expect(page.getByText("test.e2e.beta")).toBeVisible()
  })

  test("filters jobs by status", async ({ page }) => {
    await seedJob("pending", "test.e2e.pending-job")
    await seedJob("completed", "test.e2e.completed-job")

    await page.goto("/dashboard/jobs")

    const rows = page.locator("table tbody tr")
    await expect(rows).toHaveCount(2, { timeout: 5000 })

    await page.getByRole("combobox").click()
    await page.getByRole("option", { name: "Completed" }).click()

    await expect(rows).toHaveCount(1, { timeout: 5000 })
    await expect(page.getByText("test.e2e.completed-job")).toBeVisible()
    await expect(page.getByText("test.e2e.pending-job")).not.toBeVisible()
  })

  test("opens detail sheet when clicking a job row", async ({ page }) => {
    const id = await seedJob("pending", "test.e2e.detail")

    await page.goto("/dashboard/jobs")

    const row = page.locator("table tbody tr", {
      hasText: "test.e2e.detail",
    })
    await expect(row).toBeVisible({ timeout: 5000 })
    await row.click()

    const sheet = page.locator("[data-state='open'][role='dialog']")
    await expect(sheet).toBeVisible({ timeout: 3000 })

    await expect(sheet.getByText("Job Details")).toBeVisible()
    await expect(sheet.getByText(id)).toBeVisible()
    await expect(sheet.getByText("test.e2e.detail")).toBeVisible()
    await expect(sheet.getByText("pending")).toBeVisible()
  })

  test("shows cancel button for running job and cancels it", async ({
    page,
  }) => {
    const id = await seedJob("running", "test.e2e.cancel")

    await page.goto("/dashboard/jobs")

    const row = page.locator("table tbody tr", {
      hasText: "test.e2e.cancel",
    })
    await expect(row).toBeVisible({ timeout: 5000 })
    await row.click()

    const sheet = page.locator("[data-state='open'][role='dialog']")
    await expect(sheet).toBeVisible({ timeout: 3000 })

    const cancelButton = sheet.getByRole("button", { name: "Cancel Job" })
    await expect(cancelButton).toBeVisible()
    await cancelButton.click()

    await expect(page.getByText("Job cancelled")).toBeVisible({
      timeout: 5000,
    })
  })

  test("shows pause button for running job", async ({ page }) => {
    await seedJob("running", "test.e2e.pause")

    await page.goto("/dashboard/jobs")

    const row = page.locator("table tbody tr", {
      hasText: "test.e2e.pause",
    })
    await expect(row).toBeVisible({ timeout: 5000 })
    await row.click()

    const sheet = page.locator("[data-state='open'][role='dialog']")
    await expect(sheet).toBeVisible({ timeout: 3000 })

    await expect(
      sheet.getByRole("button", { name: "Pause Job" }),
    ).toBeVisible()
  })

  test("shows resume button for paused job", async ({ page }) => {
    await seedJob("paused", "test.e2e.resume")

    await page.goto("/dashboard/jobs")

    const row = page.locator("table tbody tr", {
      hasText: "test.e2e.resume",
    })
    await expect(row).toBeVisible({ timeout: 5000 })
    await row.click()

    const sheet = page.locator("[data-state='open'][role='dialog']")
    await expect(sheet).toBeVisible({ timeout: 3000 })

    await expect(
      sheet.getByRole("button", { name: "Resume Job" }),
    ).toBeVisible()
  })

  test("shows error section for failed job", async ({ page }) => {
    const client = new pg.Client(DB_URL)
    await client.connect()
    try {
      const id = randomUUID()
      const now = new Date().toISOString()
      await client.query(
        `INSERT INTO happyview_jobs (id, job_type, status, input, progress, error, created_by, created_at, completed_at)
         VALUES ($1, $2, 'failed', $3, $4, $5, $6, $7, $7)`,
        [
          id,
          "test.e2e.failed",
          JSON.stringify({}),
          JSON.stringify({}),
          "something went wrong",
          TEST_DID,
          now,
        ],
      )
    } finally {
      await client.end()
    }

    await page.goto("/dashboard/jobs")

    const row = page.locator("table tbody tr", {
      hasText: "test.e2e.failed",
    })
    await expect(row).toBeVisible({ timeout: 5000 })
    await row.click()

    const sheet = page.locator("[data-state='open'][role='dialog']")
    await expect(sheet).toBeVisible({ timeout: 3000 })

    await expect(sheet.getByText("something went wrong")).toBeVisible()
  })
})
