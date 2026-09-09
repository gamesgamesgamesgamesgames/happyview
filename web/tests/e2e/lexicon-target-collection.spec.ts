import { test, expect } from "@playwright/test";
import { loginAsTestAdmin } from "./auth-helper";

const QUERY_NSID = "test.e2e.targetcollection.listrecords";
const TARGET_COLLECTION = "org.hypercerts.claim.activity";

async function replaceLexiconJson(
  page: import("@playwright/test").Page,
  value: unknown,
) {
  await page.locator(".monaco-editor").first().click();
  await page.keyboard.press("ControlOrMeta+KeyA");
  await page.evaluate((text) => {
    const data = new DataTransfer();
    data.setData("text/plain", text);
    document.activeElement?.dispatchEvent(
      new ClipboardEvent("paste", {
        clipboardData: data,
        bubbles: true,
        cancelable: true,
      }),
    );
  }, JSON.stringify(value));
}

function queryLexicon() {
  return {
    lexicon: 1,
    id: QUERY_NSID,
    defs: {
      main: {
        type: "query",
        parameters: {
          type: "params",
          properties: {
            limit: { type: "integer" },
          },
        },
        output: {
          encoding: "application/json",
        },
      },
    },
  };
}

test.describe("Local lexicon target collection", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page);
    await page.request.delete(`/admin/lexicons/${QUERY_NSID}`);
    await page.goto("/dashboard/lexicons/new");
  });

  test.afterEach(async ({ page }) => {
    await page.request.delete(`/admin/lexicons/${QUERY_NSID}`);
  });

  test("persists an optional target collection for a native query", async ({
    page,
  }) => {
    await expect(page.getByLabel("Record Collection (optional)")).toBeHidden();

    await replaceLexiconJson(page, queryLexicon());

    const targetCollection = page.getByLabel("Record Collection (optional)");
    await expect(targetCollection).toBeVisible();
    await targetCollection.fill(TARGET_COLLECTION);
    await page.getByRole("button", { name: "Upload" }).click();

    await expect(page).toHaveURL(/\/dashboard\/lexicons\/?$/);

    const response = await page.request.get(
      `/admin/lexicons/${encodeURIComponent(QUERY_NSID)}`,
    );
    expect(response.ok()).toBeTruthy();
    expect((await response.json()).target_collection).toBe(TARGET_COLLECTION);
  });
});
