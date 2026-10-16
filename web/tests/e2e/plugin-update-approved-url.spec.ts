import { test, expect } from "@playwright/test";
import { loginAsTestAdmin } from "./auth-helper";

const OFFICIAL_URL =
  "https://github.com/happyproto/plugins/releases/download/lua-v1.1.0/manifest.json";
const STRANGER_URL = "https://stranger.example/lua/manifest.json";

/**
 * The update dialog installs the manifest the server's update gate approved
 * for the install, never the catalogue listing's entry under the same id.
 * The listing here holds a stranger's `lua`, as a stale or unchecked
 * listing can, and the stranger's URL must reach neither the preview nor
 * the reload.
 */
test("the update dialog installs the approved entry, not the listing's", async ({
  page,
}) => {
  const installed = {
    id: "lua",
    name: "Lua",
    version: "1.0.0",
    source: "url",
    url: "https://github.com/happyproto/plugins/releases/download/lua-v1.0.0/manifest.json",
    sha256: null,
    enabled: true,
    auth_type: "none",
    required_secrets: [],
    secrets_configured: true,
    loaded_at: null,
    update_available: true,
    latest_version: "1.1.0",
    pending_releases: [
      {
        version: "1.1.0",
        name: "lua-v1.1.0",
        published_at: "2026-10-01T00:00:00Z",
        body: "- faster",
      },
    ],
    latest_manifest_url: OFFICIAL_URL,
    plugin_type: "interpreter",
    namespace: null,
    dependencies: [],
    allowed_hosts: [],
    capabilities: { declared: [], required_by_imports: [], undeclared: [] },
  };

  await page.route(/\/admin\/plugins(\?.*)?$/, (route) =>
    route.request().method() === "GET"
      ? route.fulfill({
          json: { plugins: [installed], encryption_configured: true },
        })
      : route.continue(),
  );
  await page.route("**/admin/plugins/official", (route) =>
    route.fulfill({
      json: {
        plugins: [
          {
            id: "lua",
            name: "Lua",
            description: null,
            icon_url: null,
            latest_version: "9.0.0",
            manifest_url: STRANGER_URL,
            official: false,
          },
        ],
        last_refreshed_at: null,
      },
    }),
  );
  const previewed: string[] = [];
  await page.route("**/admin/plugins/preview", (route) => {
    previewed.push(route.request().postDataJSON().url);
    return route.fulfill({ status: 400, json: { error: "not served here" } });
  });
  const reloaded: string[] = [];
  await page.route("**/admin/plugins/lua/reload", (route) => {
    reloaded.push(route.request().postDataJSON().url);
    return route.fulfill({ json: { ...installed, version: "1.1.0" } });
  });

  await loginAsTestAdmin(page);
  // The update lives on the plugin's own page now; the ledger links to it.
  await page.goto("/dashboard/settings/plugins/lua");
  await page.getByRole("button", { name: "Update", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "Update to v1.1.0" }).click();

  await expect.poll(() => reloaded).toEqual([OFFICIAL_URL]);
  expect(previewed).not.toContain(STRANGER_URL);
});
