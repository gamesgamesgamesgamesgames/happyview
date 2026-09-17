import { test, expect, type Page } from "@playwright/test";
import { loginAsTestAdmin } from "./auth-helper";

/**
 * Coverage for issue #101: duplicating a client so a near-identical one does
 * not have to be retyped.
 *
 * `client_id_url` is UNIQUE, so a true 1:1 copy can never be saved. The
 * duplicate therefore mirrors every other field and leaves that one blank for
 * the operator to fill — which is the behaviour these tests pin down. A test
 * that only asserted "the sheet opened" would pass against a Duplicate button
 * that copied nothing at all.
 */

const SOURCE_NAME = "Prefill Source";
const SOURCE_SCOPE = "repo:com.example.post";

const SOURCE = {
  name: SOURCE_NAME,
  client_id_url:
    "https://duplicate-source.e2e.invalid/oauth-client-metadata.json",
  client_uri: "https://duplicate-source.e2e.invalid",
  redirect_uris: ["https://duplicate-source.e2e.invalid/callback"],
  scopes: `atproto ${SOURCE_SCOPE}`,
  client_type: "public",
  allowed_origins: ["https://duplicate-source.e2e.invalid"],
  rate_limit_capacity: 42,
  rate_limit_refill_rate: 3.5,
};

const COPY_CLIENT_ID_URL =
  "https://duplicate-copy.e2e.invalid/oauth-client-metadata.json";

async function deleteClientsNamed(page: Page, prefix: string) {
  const resp = await page.request.get("/admin/api-clients");
  if (!resp.ok()) return;
  const clients = (await resp.json()) as { id: string; name: string }[];
  for (const client of clients) {
    if (client.name.startsWith(prefix)) {
      await page.request.delete(`/admin/api-clients/${client.id}`);
    }
  }
}

test.describe("Duplicate API client", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page);
    await deleteClientsNamed(page, SOURCE_NAME);

    const created = await page.request.post("/admin/api-clients", {
      data: SOURCE,
    });
    expect(created.status()).toBe(201);

    await page.goto("/dashboard/settings/api-clients/");
    await expect(page.getByRole("cell", { name: SOURCE_NAME })).toBeVisible();
  });

  test.afterEach(async ({ page }) => {
    await deleteClientsNamed(page, SOURCE_NAME);
  });

  async function openDuplicateSheet(page: Page) {
    const row = page.getByRole("row", { name: new RegExp(SOURCE_NAME) });
    await row.getByRole("button", { name: "Duplicate", exact: true }).click();
    await expect(
      page.getByRole("heading", { name: "Duplicate API Client" }),
    ).toBeVisible();
  }

  test("prefills every field from the source client", async ({ page }) => {
    await openDuplicateSheet(page);

    await expect(page.getByLabel("Name", { exact: true })).toHaveValue(
      `${SOURCE_NAME} (copy)`,
    );
    await expect(page.getByLabel("Client URI")).toHaveValue(SOURCE.client_uri);
    await expect(page.locator("#redirect-uris")).toHaveValue(
      SOURCE.redirect_uris[0],
    );
    await expect(page.locator("#scopes")).toHaveValue(SOURCE_SCOPE);
    await expect(page.locator("#allowed-origins")).toHaveValue(
      SOURCE.allowed_origins[0],
    );
    await expect(page.getByLabel("Bucket Size")).toHaveValue(
      String(SOURCE.rate_limit_capacity),
    );
    await expect(page.getByLabel("Refill Rate")).toHaveValue(
      String(SOURCE.rate_limit_refill_rate),
    );
    await expect(page.getByRole("radio", { name: "Public" })).toBeChecked();
  });

  test("leaves the Client ID URL blank, since it must be unique", async ({
    page,
  }) => {
    await openDuplicateSheet(page);

    await expect(page.getByLabel("Client ID URL")).toHaveValue("");
  });

  test("saves a client carrying the source's settings", async ({ page }) => {
    await openDuplicateSheet(page);

    await page.getByLabel("Client ID URL").fill(COPY_CLIENT_ID_URL);
    await page.getByRole("button", { name: "Create", exact: true }).click();

    await expect(
      page.getByRole("heading", { name: "API Client Created" }),
    ).toBeVisible();
    await page.getByRole("button", { name: "Done" }).click();

    const copyRow = page.getByRole("row", {
      name: new RegExp(`${SOURCE_NAME} \\(copy\\)`),
    });
    await expect(copyRow).toBeVisible();
    await expect(copyRow.getByText("Public")).toBeVisible();
    await expect(copyRow.getByText(SOURCE_SCOPE)).toBeVisible();
    await expect(copyRow.getByText(COPY_CLIENT_ID_URL)).toBeVisible();
  });
});
