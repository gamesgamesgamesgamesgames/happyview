import { test, expect, type Page } from "@playwright/test";
import pg from "pg";
import { loginAsTestAdmin } from "./auth-helper";

const DB_URL = "postgres://happyview:happyview@localhost:5434/happyview_test";

async function getSetting(key: string): Promise<string | null> {
  const client = new pg.Client(DB_URL);
  await client.connect();
  try {
    const { rows } = await client.query(
      "SELECT value FROM happyview_instance_settings WHERE key = $1",
      [key],
    );
    return rows[0]?.value ?? null;
  } finally {
    await client.end();
  }
}

// Ends every grant still active, so one test's grant can't unlock the next
// test's space (every space here shares the test admin as creator).
async function revokeOpenGrants() {
  const client = new pg.Client(DB_URL);
  await client.connect();
  try {
    await client.query(
      "UPDATE happyview_space_access_grants SET revoked_at = $1 WHERE revoked_at IS NULL",
      [new Date().toISOString()],
    );
  } finally {
    await client.end();
  }
}

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

async function createSpace(
  page: Page,
): Promise<{ uri: string; id: string; creatorDid: string }> {
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
  return { uri, ...(await spaceRowFor(uri)) };
}

/**
 * A space's internal id and creator, read from the database. The admin spaces
 * API is closed while the inspector is off, and some tests create a space in
 * that state. The creator differs from the DID in the URI, which is the space's
 * authority.
 */
async function spaceRowFor(uri: string): Promise<{ id: string; creatorDid: string }> {
  const [did, , typeNsid, skey] = uri.replace(/^at:\/\//, "").split("/");
  const client = new pg.Client(DB_URL);
  await client.connect();
  try {
    const { rows } = await client.query(
      "SELECT id, creator_did FROM happyview_spaces WHERE did = $1 AND type_nsid = $2 AND skey = $3",
      [did, typeNsid, skey],
    );
    return { id: rows[0].id, creatorDid: rows[0].creator_did };
  } finally {
    await client.end();
  }
}

test.describe("Space inspector", () => {
  let spaceUri: string | null = null;
  let spacesSetting: string | null = null;

  test.beforeAll(async () => {
    spacesSetting = await getSetting("feature.spaces_enabled");
  });

  test.afterAll(async () => {
    await setSetting("feature.spaces_enabled", spacesSetting);
  });

  test.beforeEach(async ({ page }) => {
    await setSetting("feature.spaces_enabled", "true");
    await loginAsTestAdmin(page);
  });

  test.afterEach(async ({ page }) => {
    await setSetting("feature.space_inspector_enabled", null);
    await revokeOpenGrants();
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
    const { uri, creatorDid } = await createSpace(page);
    spaceUri = uri;

    await page.goto("/dashboard/spaces/");
    await page.getByLabel("Account DID").fill(creatorDid);
    await page.getByRole("button", { name: "Open account" }).click();
    await expect(page).toHaveURL(/\/dashboard\/spaces\/account\/\?did=/);
    await expect(page.getByText(uri)).toBeVisible();

    await page.getByRole("button", { name: "Request access" }).click();
    await page.getByRole("dialog").getByLabel("Reason").fill("Report #2");
    await page.getByRole("dialog").getByRole("button", { name: "Grant access" }).click();
    const banner = page
      .locator("div")
      .filter({ hasText: "Access to" })
      .filter({ has: page.getByRole("button", { name: "End access" }) })
      .last();
    await expect(banner).toBeVisible();
    await expect(banner).toContainText(creatorDid);
  });

  test("a grant's event lists the reads made under it", async ({ page }) => {
    await setSetting("feature.space_inspector_enabled", "true");
    const { uri, id } = await createSpace(page);
    spaceUri = uri;
    await page.goto(`/dashboard/spaces/${encodeURIComponent(id)}/`);
    await page.getByRole("button", { name: "Request access" }).click();
    await page.getByRole("dialog").getByLabel("Reason").fill("Report #3");
    await page.getByRole("dialog").getByRole("button", { name: "Grant access" }).click();
    await expect(page.getByText("No records match.")).toBeVisible();

    await page.goto("/dashboard/events/");
    await page.getByText("space.access_granted").first().click();
    await expect(page.getByText("Reads under this grant")).toBeVisible();
    await expect(page.getByText("list_records")).toBeVisible();
  });
});
