import { test, expect } from "@playwright/test";
import { loginAsTestAdmin } from "./auth-helper";

/**
 * The catalogue lists every publisher's plugins. Only the official
 * publisher's carry the badge, so a plugin anyone could have published never
 * reads as HappyProto's. A project its publisher retired is not offered.
 */
test("the catalogue badges the official plugin only and hides retired ones", async ({
  page,
}) => {
  await page.route("**/admin/plugins/official", (route) =>
    route.fulfill({
      json: {
        plugins: [
          {
            id: "lua",
            name: "Lua",
            description: null,
            icon_url: null,
            publisher_did: "did:plc:mo7vq4jbioxwafy5zkcc3eux",
            plugin_type: "interpreter",
            latest_version: "1.2.0",
            channels: {
              latest: {
                version: "1.2.0",
                state: "active",
                installable: true,
                availability: "available",
              },
            },
            lifecycle_state: "active",
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
            publisher_did: "did:plc:someoneelse",
            plugin_type: "library",
            // A project whose `latest` points nowhere still lists.
            latest_version: null,
            channels: {
              beta: {
                version: "9.0.0-beta.1",
                state: "active",
                installable: true,
                availability: "available",
              },
            },
            lifecycle_state: "active",
            manifest_url: "https://example.com/extra/manifest.json",
            official: false,
            banner_url: null,
            releases: [],
            sections: {},
          },
          {
            id: "gone",
            name: "Gone",
            description: null,
            icon_url: null,
            publisher_did: "did:plc:mo7vq4jbioxwafy5zkcc3eux",
            plugin_type: "library",
            latest_version: "1.0.0",
            channels: {
              latest: {
                version: "1.0.0",
                state: "retired",
                installable: false,
                availability: "available",
              },
            },
            lifecycle_state: "retired",
            manifest_url: "https://example.com/gone/manifest.json",
            official: true,
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
  // Two of these lead to the catalogue while nothing is installed: the
  // header action and the empty state's own call to action.
  await page.getByRole("link", { name: "Add plugin" }).first().click();
  await expect(page).toHaveURL(/\/dashboard\/settings\/plugins\/add\/?$/);

  const official = page.getByRole("link", { name: /Lua/ });
  const other = page.getByRole("link", { name: /Extra/ });
  await expect(official).toBeVisible();
  await expect(other).toBeVisible();
  await expect(official.getByText("Official", { exact: true })).toBeVisible();
  await expect(other.getByText("Official", { exact: true })).toHaveCount(0);
  await expect(other.getByText("No latest release")).toBeVisible();
  await expect(page.getByRole("link", { name: /Gone/ })).toHaveCount(0);
});
