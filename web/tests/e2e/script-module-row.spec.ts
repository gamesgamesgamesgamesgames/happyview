import { test, expect } from "@playwright/test";
import { loginAsTestAdmin } from "./auth-helper";

/**
 * A compiled module's handler comes from a project build, which this stack
 * does not have, so the row is served from a route stub. What is under test
 * is the rendering: a module row is read-only and says which module, path,
 * function and build it is.
 */
const ID = "xrpc.query:app.test.echo";
const ROW = {
  id: ID,
  script_type: "rust",
  body: "",
  description: null,
  outbound_xrpcs: null,
  created_at: null,
  updated_at: null,
  recreatable: true,
  needs_migration: [],
  runnable: true,
  layer: "project",
  path: "modules/registry",
  shadows_stored: false,
  module: {
    module: "registry",
    path: "modules/registry",
    handler: "echo",
    hash: "9f2c1e7d4b8a3f60c5e2d1b0a9f8e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6",
  },
};

test.describe("A compiled module's handler", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page);
    await page.route(
      (url) =>
        url.pathname.endsWith(`/admin/scripts/${encodeURIComponent(ID)}`),
      (route) =>
        route.request().isNavigationRequest()
          ? route.continue()
          : route.fulfill({ json: ROW }),
    );
  });

  test("renders read-only with its module, path, function and hash", async ({
    page,
  }) => {
    await page.goto(`/dashboard/settings/scripts/${encodeURIComponent(ID)}`);

    const panel = page.getByRole("region", { name: "Compiled module" });
    await expect(panel).toBeVisible();
    await expect(panel.locator("dd")).toHaveText([
      "registry",
      "modules/registry",
      "echo",
      ROW.module.hash,
    ]);

    await expect(page.getByRole("button", { name: /^save/i })).toHaveCount(0);
    await expect(
      page.getByRole("button", { name: /delete script/i }),
    ).toHaveCount(0);
    await expect(page.getByText(/No interpreter is installed/)).toHaveCount(0);
  });

  test("says when the build did not record the handler's function", async ({
    page,
  }) => {
    await page.route(
      (url) =>
        url.pathname.endsWith(`/admin/scripts/${encodeURIComponent(ID)}`),
      (route) =>
        route.request().isNavigationRequest()
          ? route.continue()
          : route.fulfill({
              json: { ...ROW, module: { ...ROW.module, handler: "" } },
            }),
    );
    await page.goto(`/dashboard/settings/scripts/${encodeURIComponent(ID)}`);

    const handler = page
      .getByRole("region", { name: "Compiled module" })
      .locator("dd")
      .nth(2);
    await expect(handler).toHaveText("—not recorded");
    await expect(handler.locator(".sr-only")).toHaveText("not recorded");
  });
});
