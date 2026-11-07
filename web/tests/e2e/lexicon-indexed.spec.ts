import { test, expect } from "@playwright/test";
import { loginAsTestAdmin } from "./auth-helper";

function lexicon(id: string) {
  return {
    lexicon: 1,
    id,
    defs: {
      main: {
        type: "record",
        key: "tid",
        record: {
          type: "object",
          properties: {
            title: { type: "string" },
          },
        },
      },
    },
  };
}

async function storedLexicon(
  request: import("@playwright/test").APIRequestContext,
  id: string,
) {
  const resp = await request.get(`/admin/lexicons/${id}`);
  expect(resp.ok()).toBeTruthy();
  return (await resp.json()) as {
    indexed: boolean;
    backfill: boolean;
    lexicon_json: unknown;
  };
}

test.describe("Lexicon indexed toggle", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page);
  });

  test("the detail page turns indexing off and on, keeping the rest", async ({
    page,
  }) => {
    const id = "test.e2e.lexiconindexed.toggle";
    await page.request.delete(`/admin/lexicons/${id}`);
    const created = await page.request.post("/admin/lexicons", {
      data: { lexicon_json: lexicon(id), backfill: false, indexed: true },
    });
    expect(created.status()).toBe(201);

    await page.goto(`/dashboard/lexicons/${id}`);
    const toggle = page.getByRole("switch", { name: "Indexed" });
    await expect(toggle).toBeChecked();

    await toggle.click();
    await expect(toggle).not.toBeChecked();
    await expect
      .poll(async () => (await storedLexicon(page.request, id)).indexed)
      .toBe(false);
    const off = await storedLexicon(page.request, id);
    expect(off.backfill).toBe(false);
    expect(off.lexicon_json).toEqual(lexicon(id));

    await page.reload();
    await expect(toggle).not.toBeChecked();

    await toggle.click();
    await expect(toggle).toBeChecked();
    await expect
      .poll(async () => (await storedLexicon(page.request, id)).indexed)
      .toBe(true);

    await page.request.delete(`/admin/lexicons/${id}`);
  });
});
