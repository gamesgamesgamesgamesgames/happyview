import { test, expect } from "@playwright/test";
import { loginAsTestAdmin } from "./auth-helper";

/**
 * A labeler declared in happyview.toml belongs to the file: the API refuses
 * to delete it, so the page must not offer to, and must say where to change
 * it instead.
 */

const DECLARED = "did:plc:e2edeclaredlabeler";
const DASHBOARD = "did:plc:e2edashboardlabeler";
const STAMP = "2026-10-11T00:00:00+00:00";

test.describe("Labelers declared in happyview.toml", () => {
  test.beforeEach(async ({ page }) => {
    await page.route("**/admin/labelers", (route) => {
      if (route.request().method() !== "GET") return route.continue();
      return route.fulfill({
        json: [
          {
            did: DECLARED,
            status: "active",
            cursor: null,
            created_at: STAMP,
            updated_at: STAMP,
            declared: true,
          },
          {
            did: DASHBOARD,
            status: "active",
            cursor: null,
            created_at: STAMP,
            updated_at: STAMP,
            declared: false,
          },
        ],
      });
    });
    await loginAsTestAdmin(page);
    await page.goto("/dashboard/settings/labelers");
  });

  test("a declared labeler names the file and offers no delete", async ({
    page,
  }) => {
    const declared = page.getByRole("row").filter({ hasText: DECLARED });
    await expect(declared.getByText("happyview.toml")).toBeVisible();
    await expect(
      declared.getByRole("button", { name: "Delete labeler" }),
    ).toHaveCount(0);
    await expect(
      declared.getByRole("button", { name: "Pause labeler" }),
    ).toBeVisible();

    const own = page.getByRole("row").filter({ hasText: DASHBOARD });
    await expect(own.getByText("happyview.toml")).toHaveCount(0);
    await expect(
      own.getByRole("button", { name: "Delete labeler" }),
    ).toBeVisible();
  });
});
