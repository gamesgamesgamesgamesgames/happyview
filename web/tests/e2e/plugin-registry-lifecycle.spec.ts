import { test, expect, type Page } from "@playwright/test";
import { loginAsTestAdmin } from "./auth-helper";

const OFFICIAL_DID = "did:plc:mo7vq4jbioxwafy5zkcc3eux";
const STRANGER_URL = "https://stranger.example/lua/manifest.json";

function release(
  version: string,
  state = "active",
  reason: string | null = null,
) {
  return {
    version,
    name: `lua-v${version}`,
    published_at: "2026-10-01T00:00:00Z",
    body: "- faster",
    state,
    reason,
    suspect: false,
    installable: state === "active" || state === "deprecated",
    availability: "available",
    version_uri: `at://${OFFICIAL_DID}/software.release.version/${version}`,
    manifest_cid: "bafymanifest",
    package_cid: "bafypackage",
    manifest_blob_cid: "bafyblob",
  };
}

function registryInstall(overrides: Record<string, unknown> = {}) {
  return {
    id: "lua",
    name: "Lua",
    version: "1.0.0",
    description: null,
    icon_url: null,
    banner_url: null,
    source: "url",
    url: null,
    sha256: null,
    enabled: true,
    auth_type: "none",
    required_secrets: [],
    secrets_configured: true,
    loaded_at: null,
    update_available: true,
    latest_version: "1.1.0",
    latest_version_installable: true,
    latest_state: "active",
    pending_releases: [release("1.1.0")],
    registry: {
      did: OFFICIAL_DID,
      name: "lua",
      channel: "latest",
      version_uri: `at://${OFFICIAL_DID}/software.release.version/1.0.0`,
      installed_state: "active",
      installed_reason: null,
      channel_state: "active",
      unresolved: false,
    },
    plugin_type: "interpreter",
    namespace: null,
    dependencies: [],
    allowed_hosts: [],
    capabilities: { declared: [], required_by_imports: [], undeclared: [] },
    ...overrides,
  };
}

const UNRESTRICTED = {
  name: "network:request:unrestricted",
  risk: "high",
  description: "Reach any host",
};

/** What `POST /admin/plugins/preview` answers for one version. */
function previewOf(
  version: string,
  declared: Record<string, unknown>[] = [],
  registry: Record<string, unknown> | null = null,
) {
  return {
    id: "lua",
    name: "Lua",
    version,
    description: null,
    icon_url: null,
    banner_url: null,
    auth_type: "none",
    required_secrets: [],
    manifest_url: "https://registry.example/manifest",
    wasm_url: "https://registry.example/wasm",
    plugin_type: "interpreter",
    namespace: null,
    dependencies: [],
    allowed_hosts: [],
    capabilities: { declared, required_by_imports: [], undeclared: [] },
    sha256: "a".repeat(64),
    ...(registry ? { registry } : {}),
  };
}

async function serve(
  page: Page,
  installed: Record<string, unknown>,
  catalogue: Record<string, unknown>[],
) {
  await page.route(/\/admin\/plugins(\?.*)?$/, (route) =>
    route.request().method() === "GET"
      ? route.fulfill({
          json: { plugins: [installed], encryption_configured: true },
        })
      : route.continue(),
  );
  await page.route("**/admin/plugins/official", (route) =>
    route.fulfill({ json: { plugins: catalogue, last_refreshed_at: null } }),
  );
}

/**
 * A registry install updates along the channel it follows, and the server
 * resolves that version. The listing here holds a stranger's `lua`, as a
 * stale or unchecked listing can, and the stranger's URL must reach neither
 * the preview nor the reload. The new version adds a permission, which the
 * operator agrees to before the update, and the reload names it so the
 * server grants nothing the dialog didn't show.
 */
test("the update dialog moves a registry install along its channel", async ({
  page,
}) => {
  const installed = registryInstall();
  await serve(page, installed, [
    {
      id: "lua",
      publisher_did: "did:plc:stranger",
      name: "Lua",
      description: null,
      icon_url: null,
      plugin_type: "interpreter",
      latest_version: "9.0.0",
      channels: {},
      lifecycle_state: "active",
      manifest_url: STRANGER_URL,
      official: false,
      banner_url: null,
      releases: [],
      sections: {},
    },
  ]);
  const previewed: unknown[] = [];
  await page.route("**/admin/plugins/preview", (route) => {
    previewed.push(route.request().postDataJSON());
    return route.fulfill({ json: previewOf("1.1.0", [UNRESTRICTED]) });
  });
  const reloaded: unknown[] = [];
  await page.route("**/admin/plugins/lua/reload", (route) => {
    reloaded.push(route.request().postDataJSON());
    return route.fulfill({ json: { ...installed, version: "1.1.0" } });
  });

  await loginAsTestAdmin(page);
  await page.goto("/dashboard/settings/plugins/lua");
  await page.getByRole("button", { name: "Update", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  const update = dialog.getByRole("button", { name: "Update to v1.1.0" });
  await expect(dialog.getByText(UNRESTRICTED.name)).toBeVisible();
  await expect(update).toBeDisabled();
  await dialog
    .getByRole("checkbox", { name: "Allow these permissions" })
    .check();
  await update.click();

  await expect
    .poll(() => reloaded)
    .toEqual([{ registry: {}, accepted_capabilities: [UNRESTRICTED.name] }]);
  expect(JSON.stringify(previewed)).not.toContain(STRANGER_URL);
  expect(previewed).toContainEqual({
    registry: { did: OFFICIAL_DID, name: "lua", channel: "latest" },
  });
});

/**
 * A retired version keeps running, so the plugin's page is where an operator
 * learns of it. A channel that moved somewhere uninstallable offers nothing,
 * and says so rather than looking current.
 */
test("a retired install is flagged and its stuck channel explained", async ({
  page,
}) => {
  await serve(
    page,
    registryInstall({
      update_available: false,
      latest_version: "1.2.0",
      latest_version_installable: false,
      latest_state: "unavailable",
      pending_releases: [],
      registry: {
        did: OFFICIAL_DID,
        name: "lua",
        channel: "latest",
        version_uri: `at://${OFFICIAL_DID}/software.release.version/1.0.0`,
        installed_state: "retired",
        installed_reason: "Leaks file handles",
        channel_state: "unavailable",
        unresolved: false,
      },
    }),
    [],
  );

  await loginAsTestAdmin(page);
  await page.goto("/dashboard/settings/plugins/lua");
  await expect(
    page.getByText("This version is retired by its publisher"),
  ).toBeVisible();
  await expect(page.getByText("Leaks file handles").first()).toBeVisible();
  await expect(
    page.getByText(
      "The latest channel points to a version that isn't available",
    ),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Update", exact: true }),
  ).toHaveCount(0);
});

/** Switching channel names the channel; the server resolves its version. */
test("switch channel reloads the plugin onto the chosen channel", async ({
  page,
}) => {
  const installed = registryInstall({
    update_available: false,
    latest_version: "1.0.0",
    pending_releases: [],
  });
  await serve(page, installed, [
    {
      id: "lua",
      publisher_did: OFFICIAL_DID,
      name: "Lua",
      description: null,
      icon_url: null,
      plugin_type: "interpreter",
      latest_version: "1.0.0",
      channels: {
        latest: {
          version: "1.0.0",
          state: "active",
          installable: true,
          availability: "available",
        },
        beta: {
          version: "1.1.0-beta.1",
          state: "active",
          installable: true,
          availability: "available",
        },
      },
      lifecycle_state: "active",
      manifest_url: "https://example.com/lua/manifest.json",
      official: true,
      banner_url: null,
      releases: [release("1.1.0-beta.1"), release("1.0.0")],
      sections: {},
    },
  ]);
  const previewed: unknown[] = [];
  await page.route("**/admin/plugins/preview", (route) => {
    previewed.push(route.request().postDataJSON());
    return route.fulfill({ json: previewOf("1.1.0-beta.1") });
  });
  const reloaded: unknown[] = [];
  await page.route("**/admin/plugins/lua/reload", (route) => {
    reloaded.push(route.request().postDataJSON());
    return route.fulfill({ json: { ...installed, version: "1.1.0-beta.1" } });
  });

  await loginAsTestAdmin(page);
  await page.goto("/dashboard/settings/plugins/lua");
  await page.getByRole("button", { name: "Switch channel" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("combobox", { name: "Channel" }).click();
  await page.getByRole("option", { name: /beta/ }).click();
  await dialog
    .getByRole("button", { name: "Switch to beta (v1.1.0-beta.1)" })
    .click();

  await expect
    .poll(() => reloaded)
    .toEqual([{ registry: { channel: "beta" }, accepted_capabilities: [] }]);
  expect(previewed).toEqual([
    { registry: { did: OFFICIAL_DID, name: "lua", channel: "beta" } },
  ]);
});

const LISTED_URL = "https://example.com/lua/1.1.0/manifest.json";

function listing(latest: string) {
  return {
    id: "lua",
    publisher_did: OFFICIAL_DID,
    name: "Lua",
    description: null,
    icon_url: null,
    plugin_type: "interpreter",
    latest_version: latest,
    channels: {
      latest: {
        version: latest,
        state: "active",
        installable: true,
        availability: "available",
      },
    },
    lifecycle_state: "active",
    manifest_url: LISTED_URL,
    official: true,
    banner_url: null,
    releases: [release(latest)],
    sections: {},
  };
}

/**
 * A URL install has no channel to move along. Until the catalogue stops
 * carrying a manifest URL, its update reads the listing's, but only when the
 * listing offers the version the server approved.
 */
for (const [label, registry] of [
  ["with no registry project", null],
  [
    "whose project is unresolved",
    {
      did: null,
      name: null,
      channel: null,
      version_uri: null,
      installed_state: null,
      installed_reason: null,
      channel_state: null,
      unresolved: true,
    },
  ],
] as const) {
  test(`a URL install ${label} updates from the listing's manifest`, async ({
    page,
  }) => {
    const installed = registryInstall({
      url: "https://example.com/lua/1.0.0/manifest.json",
      registry,
    });
    await serve(page, installed, [listing("1.1.0")]);
    const previewed: unknown[] = [];
    await page.route("**/admin/plugins/preview", (route) => {
      previewed.push(route.request().postDataJSON());
      return route.fulfill({ json: previewOf("1.1.0") });
    });
    const reloaded: unknown[] = [];
    await page.route("**/admin/plugins/lua/reload", (route) => {
      reloaded.push(route.request().postDataJSON());
      return route.fulfill({ json: { ...installed, version: "1.1.0" } });
    });

    await loginAsTestAdmin(page);
    await page.goto("/dashboard/settings/plugins/lua");
    await page.getByRole("button", { name: "Update", exact: true }).click();
    await page
      .getByRole("dialog")
      .getByRole("button", { name: "Update to v1.1.0" })
      .click();

    await expect
      .poll(() => reloaded)
      .toEqual([{ url: LISTED_URL, accepted_capabilities: [] }]);
    expect(previewed).toEqual([{ url: LISTED_URL }]);
  });

  test(`a URL install ${label} can't update from a listing that moved on`, async ({
    page,
  }) => {
    await serve(
      page,
      registryInstall({
        url: "https://example.com/lua/1.0.0/manifest.json",
        registry,
      }),
      [listing("1.2.0")],
    );
    const previewed: unknown[] = [];
    await page.route("**/admin/plugins/preview", (route) => {
      previewed.push(route.request().postDataJSON());
      return route.fulfill({ json: previewOf("1.2.0") });
    });

    await loginAsTestAdmin(page);
    await page.goto("/dashboard/settings/plugins/lua");
    await page.getByRole("button", { name: "Update", exact: true }).click();
    const dialog = page.getByRole("dialog");
    await expect(
      dialog.getByText("The catalogue no longer offers this update."),
    ).toBeVisible();
    await expect(
      dialog.getByRole("button", { name: "Update to v1.1.0" }),
    ).toBeDisabled();
    expect(previewed).toEqual([]);
  });
}

/**
 * A catalogue plugin installs by its registry project and the chosen
 * channel, never from the listing's manifest URL.
 */
test("the catalogue installs by project and channel", async ({ page }) => {
  await page.route("**/admin/plugins/official", (route) =>
    route.fulfill({
      json: {
        plugins: [
          {
            id: "lua",
            publisher_did: OFFICIAL_DID,
            name: "Lua",
            description: null,
            icon_url: null,
            plugin_type: "interpreter",
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
            manifest_url: STRANGER_URL,
            official: true,
            banner_url: null,
            releases: [release("1.0.0")],
            sections: {},
          },
        ],
        last_refreshed_at: null,
      },
    }),
  );
  await page.route("**/admin/plugins/preview", (route) =>
    route.fulfill({
      json: {
        id: "lua",
        name: "Lua",
        version: "1.0.0",
        description: null,
        icon_url: null,
        banner_url: null,
        auth_type: "none",
        required_secrets: [],
        manifest_url: "https://registry.example/manifest",
        wasm_url: "https://registry.example/wasm",
        plugin_type: "interpreter",
        namespace: null,
        dependencies: [],
        allowed_hosts: [],
        capabilities: { declared: [], required_by_imports: [], undeclared: [] },
        sha256: "a".repeat(64),
        registry: {
          did: OFFICIAL_DID,
          name: "lua",
          channel: "latest",
          version: "1.0.0",
          version_uri: `at://${OFFICIAL_DID}/software.release.version/1.0.0`,
          state: "active",
        },
      },
    }),
  );
  const added: unknown[] = [];
  await page.route(/\/admin\/plugins(\?.*)?$/, (route) => {
    if (route.request().method() === "POST") {
      added.push(route.request().postDataJSON());
      return route.fulfill({ json: registryInstall() });
    }
    return route.fulfill({
      json: { plugins: [], encryption_configured: true },
    });
  });

  await loginAsTestAdmin(page);
  await page.goto("/dashboard/settings/plugins/add/lua");
  await expect(page.getByRole("combobox", { name: "Channel" })).toContainText(
    "latest",
  );
  await page.getByRole("button", { name: "Install" }).click();

  await expect
    .poll(() => added)
    .toEqual([
      {
        registry: { did: OFFICIAL_DID, name: "lua", channel: "latest" },
        accepted_capabilities: [],
      },
    ]);
});
