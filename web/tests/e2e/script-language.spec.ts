import { test, expect } from "@playwright/test";
import { loginAsTestAdmin } from "./auth-helper";

// The stack installs one interpreter, Lua, so the form has nothing to choose
// between: it names the language rather than offering a picker of one.
test.describe("Script language", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page);
  });

  test("the installed languages are what the API lists", async ({ page }) => {
    const resp = await page.request.get("/admin/scripts/languages");
    expect(resp.ok()).toBe(true);
    expect(await resp.json()).toEqual(["lua"]);
  });

  test("with one language installed, the form names it and offers no picker", async ({
    page,
  }) => {
    await page.goto("/dashboard/settings/scripts/new");
    await expect(page.locator("#source-pick")).toBeVisible({ timeout: 5000 });

    await expect(page.getByText("Lua body", { exact: true })).toBeVisible();
    await expect(page.locator("#language")).toHaveCount(0);
  });
});
