import { test, expect } from "@playwright/test";
import { loginAsTestAdmin } from "./auth-helper";

/**
 * The catalogue lists every publisher's plugins. Only the official
 * publisher's carry the badge, so a plugin anyone could have published never
 * reads as HappyProto's.
 */
test("the catalogue badges the official plugin only", async ({ page }) => {
  await page.route("**/admin/plugins/official", (route) =>
    route.fulfill({
      json: {
        plugins: [
          {
            id: "lua",
            name: "Lua",
            description: null,
            icon_url: null,
            latest_version: "1.2.0",
            manifest_url: "https://example.com/lua/manifest.json",
            official: true,
            banner_url: null,
            releases: [],
            sections: {},
          },
          {
            id: "extra",
            name: "Extra",
            description: null,
            icon_url: null,
            latest_version: "9.0.0",
            manifest_url: "https://example.com/extra/manifest.json",
            official: false,
            banner_url: null,
            releases: [],
            sections: {},
          },
        ],
        last_refreshed_at: null,
      },
    }),
  );
  // Nothing installed, so the catalogue hides none of it: the page leaves
  // out what is already installed unless asked to show it.
  await page.route(/\/admin\/plugins(\?.*)?$/, (route) =>
    route.fulfill({ json: { plugins: [], encryption_configured: true } }),
  );
  await loginAsTestAdmin(page);
  await page.goto("/dashboard/settings/plugins");
  await page.getByRole("link", { name: "Add plugin" }).click();
  await expect(page).toHaveURL(/\/dashboard\/settings\/plugins\/add\/?$/);

  const official = page.getByRole("link", { name: /Lua/ });
  const other = page.getByRole("link", { name: /Extra/ });
  await expect(official).toBeVisible();
  await expect(other).toBeVisible();
  await expect(official.getByText("Official", { exact: true })).toBeVisible();
  await expect(other.getByText("Official", { exact: true })).toHaveCount(0);
});
