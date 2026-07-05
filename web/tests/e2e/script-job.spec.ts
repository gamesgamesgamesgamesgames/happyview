import { test, expect } from "@playwright/test"
import { loginAsTestAdmin } from "./auth-helper"

const JOB_TYPE = "test.e2e.myjob"
const TRIGGER_ID = `job.run:${JOB_TYPE}`

async function seedScript(
  request: import("@playwright/test").APIRequestContext,
) {
  const resp = await request.post("/admin/scripts", {
    data: {
      id: TRIGGER_ID,
      body: "function handle()\n  return { ok = true }\nend",
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
  await request.delete(`/admin/scripts/${encodeURIComponent(TRIGGER_ID)}`)
}

test.describe("Job Script Creation", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page)
  })

  test.afterEach(async ({ page }) => {
    await cleanupScript(page.request)
  })

  test("selecting Job source shows job type input and composes trigger id", async ({
    page,
  }) => {
    await page.goto("/dashboard/settings/scripts/new")

    const sourceSelect = page.locator("#source-pick")
    await expect(sourceSelect).toBeVisible({ timeout: 5000 })

    await sourceSelect.click()
    await page.getByRole("option", { name: /Job/ }).click()

    const jobTypeInput = page.locator("#job-type-input")
    await expect(jobTypeInput).toBeVisible()

    await expect(page.locator("#action-pick")).not.toBeVisible()

    await jobTypeInput.fill(JOB_TYPE)

    await expect(page.getByText(TRIGGER_ID)).toBeVisible()
  })

  test("creating a job script navigates to detail page", async ({ page }) => {
    await page.goto("/dashboard/settings/scripts/new")

    await page.locator("#source-pick").click()
    await page.getByRole("option", { name: /Job/ }).click()

    await page.locator("#job-type-input").fill(JOB_TYPE)

    const createButton = page.getByRole("button", { name: "Create script" })
    await expect(createButton).toBeEnabled({ timeout: 3000 })
    await createButton.click()

    await page.waitForURL(
      `**/dashboard/settings/scripts/${encodeURIComponent(TRIGGER_ID)}`,
      { timeout: 10000 },
    )

    await expect(
      page.getByText("Job runner", { exact: true }),
    ).toBeVisible()
    await expect(
      page.getByText(TRIGGER_ID, { exact: true }),
    ).toBeVisible()
  })

  test("job script has job-specific template body", async ({ page }) => {
    await page.goto("/dashboard/settings/scripts/new")

    await page.locator("#source-pick").click()
    await page.getByRole("option", { name: /Job/ }).click()

    await expect(page.getByText("job.input").first()).toBeVisible({
      timeout: 3000,
    })
    await expect(page.getByText("job.should_stop").first()).toBeVisible()
  })

  test("job script appears in scripts list with Job runners family", async ({
    page,
  }) => {
    await seedScript(page.request)

    await page.goto("/dashboard/settings/scripts")

    const row = page.locator("table tbody tr", { hasText: JOB_TYPE })
    await expect(row).toBeVisible({ timeout: 5000 })

    await expect(row.getByText("Job runner", { exact: true })).toBeVisible()
    await expect(row.getByText("Job runners", { exact: true })).toBeVisible()
  })
})
