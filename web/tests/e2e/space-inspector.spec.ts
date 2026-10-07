import { test, expect, type Page } from "@playwright/test";
import pg from "pg";
import { loginAsTestAdmin } from "./auth-helper";

const DB_URL = "postgres://happyview:happyview@localhost:5434/happyview_test";

async function setSetting(key: string, value: string | null) {
  const client = new pg.Client(DB_URL);
  await client.connect();
  try {
    if (value === null) {
      await client.query("DELETE FROM happyview_instance_settings WHERE key = $1", [key]);
    } else {
      await client.query(
        `INSERT INTO happyview_instance_settings (key, value, updated_at)
         VALUES ($1, $2, $3)
         ON CONFLICT (key) DO UPDATE SET value = $2, updated_at = $3`,
        [key, value, new Date().toISOString()],
      );
    }
  } finally {
    await client.end();
  }
}

async function createSpace(page: Page): Promise<{ uri: string; id: string }> {
  const resp = await page.request.post("/xrpc/com.atproto.simplespace.createSpace", {
    data: {
      spaceType: "com.example.inspector",
      skey: `inspector-${Date.now()}`,
      readPolicy: { $type: "com.atproto.simplespace.defs#memberListPolicy" },
      writePolicy: { $type: "com.atproto.simplespace.defs#memberListPolicy" },
    },
  });
  expect(resp.ok()).toBe(true);
  const { uri } = await resp.json();
  const list = await (await page.request.get("/admin/spaces?limit=100")).json();
  const space = list.spaces.find((s: { uri: string }) => s.uri === uri);
  return { uri, id: space.id };
}

test.describe("Space inspector", () => {
  let spaceUri: string | null = null;

  test.beforeEach(async ({ page }) => {
    await setSetting("feature.spaces_enabled", "true");
    await loginAsTestAdmin(page);
  });

  test.afterEach(async ({ page }) => {
    await setSetting("feature.space_inspector_enabled", null);
    if (spaceUri) {
      await page.request.post("/xrpc/com.atproto.simplespace.deleteSpace", { data: { space: spaceUri } });
      spaceUri = null;
    }
  });

  test("records stay locked while the inspector is off", async ({ page }) => {
    const { uri, id } = await createSpace(page);
    spaceUri = uri;
    await page.goto(`/dashboard/spaces/${encodeURIComponent(id)}/`);
    await expect(page.getByText("The space inspector is turned off")).toBeVisible();
    await expect(page.getByRole("button", { name: "Request access" })).toHaveCount(0);
  });

  test("requesting access unlocks records and ending it locks them again", async ({ page }) => {
    await setSetting("feature.space_inspector_enabled", "true");
    const { uri, id } = await createSpace(page);
    spaceUri = uri;
    await page.goto(`/dashboard/spaces/${encodeURIComponent(id)}/`);

    await page.getByRole("button", { name: "Request access" }).click();
    const dialog = page.getByRole("dialog");
    await expect(dialog.getByRole("button", { name: "Grant access" })).toBeDisabled();
    await dialog.getByLabel("Reason").fill("Report #1: checking a flagged thread");
    await dialog.getByRole("button", { name: "Grant access" }).click();

    await expect(page.getByText("Report #1: checking a flagged thread")).toBeVisible();
    await expect(page.getByText(/remaining/)).toBeVisible();
    await expect(page.getByText("No records match.")).toBeVisible();

    await page.getByRole("button", { name: "End access" }).click();
    await expect(page.getByRole("button", { name: "Request access" })).toBeVisible();
  });

  test("account view lists spaces before access and records after", async ({ page }) => {
    await setSetting("feature.space_inspector_enabled", "true");
    const { uri } = await createSpace(page);
    spaceUri = uri;
    const creatorDid = uri.split("/")[2];

    await page.goto("/dashboard/spaces/");
    await page.getByLabel("Account DID").fill(creatorDid);
    await page.getByRole("button", { name: "Open account" }).click();
    await expect(page).toHaveURL(/\/dashboard\/spaces\/account\/\?did=/);
    await expect(page.getByText(uri)).toBeVisible();

    await page.getByRole("button", { name: "Request access" }).click();
    await page.getByRole("dialog").getByLabel("Reason").fill("Report #2");
    await page.getByRole("dialog").getByRole("button", { name: "Grant access" }).click();
    await expect(page.getByText(`Access to ${creatorDid}`)).toBeVisible();
  });
});
