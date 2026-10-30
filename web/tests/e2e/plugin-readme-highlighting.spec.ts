import { test, expect } from "@playwright/test";
import { loginAsTestAdmin } from "./auth-helper";

/**
 * A publisher's readme is the only code an operator reads before deciding to
 * install, so its fences are highlighted. The language comes out of the fence
 * itself, which means a publisher can name anything at all: an info string
 * Shiki has no grammar for still has to render its text, because the page is
 * not theirs to break.
 */
test("a publisher's fenced code is highlighted, and an unknown language still renders", async ({
  page,
}) => {
  const description = [
    "Fetch a page:",
    "",
    "```lua",
    "local body = http.get(url).body",
    "```",
    "",
    "Pin it:",
    "",
    "```not-a-language",
    "pin = sha256:deadbeef",
    "```",
  ].join("\n");

  await page.route("**/admin/plugins/official", (route) =>
    route.fulfill({
      json: {
        plugins: [
          {
            id: "http",
            name: "HTTP",
            description: null,
            icon_url: null,
            publisher_did: "did:plc:mo7vq4jbioxwafy5zkcc3eux",
            plugin_type: "library",
            latest_version: "1.0.0",
            channels: {
              latest: {
                version: "1.0.0",
                state: "active",
                installable: true,
                availability: "available",
              },
            },
            lifecycle_state: "active",
            official: true,
            banner_url: null,
            releases: [],
            sections: { description },
          },
        ],
        last_refreshed_at: null,
      },
    }),
  );
  // The page renders nothing until the manifest preview resolves: the
  // catalogue supplies the prose, the preview supplies the plugin.
  await page.route("**/admin/plugins/preview", (route) =>
    route.fulfill({
      json: {
        id: "http",
        name: "HTTP",
        version: "1.0.0",
        description: "Outbound HTTP for scripts.",
        icon_url: null,
        banner_url: null,
        auth_type: "none",
        required_secrets: [],
        manifest_url: "https://example.com/http/manifest.json",
        wasm_url: "https://example.com/http/http.wasm",
        plugin_type: "library",
        namespace: "happyview.http",
        dependencies: [],
        allowed_hosts: [],
        capabilities: {
          declared: [],
          required_by_imports: [],
          undeclared: [],
        },
        sha256: "a".repeat(64),
      },
    }),
  );
  await page.route(/\/admin\/plugins(\?.*)?$/, (route) =>
    route.fulfill({ json: { plugins: [], encryption_configured: true } }),
  );

  await loginAsTestAdmin(page);
  await page.goto("/dashboard/settings/plugins/add/http");

  // Scoped to the rendered markdown: the "Use it" panel on this same page is
  // highlighted too, and one of its tabs is also Lua.
  const highlighted = page.locator(".prose pre.shiki");
  await expect(highlighted).toHaveCount(1);
  await expect(highlighted).toContainText("http.get(url)");

  // The fence Shiki could not read keeps its text instead of going with the
  // grammar that failed to load, and stays the only unhighlighted one.
  await expect(page.getByText("pin = sha256:deadbeef")).toBeVisible();
});
