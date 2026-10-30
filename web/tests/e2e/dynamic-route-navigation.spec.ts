import { test, expect, type Page } from "@playwright/test";
import { loginAsTestAdmin } from "./auth-helper";

// A dynamic route is exported once, under `_`, so a link to any real id is a
// path the export never wrote. The server has to answer the router's payload
// request for it with the shell's payload; anything else makes Next reload
// the whole page instead of navigating. A value on `window` survives a
// client-side navigation and is gone after a reload.

const LEXICON = {
  lexicon: 1,
  id: "test.e2e.navigation.item",
  defs: {
    main: {
      type: "record",
      key: "tid",
      record: {
        type: "object",
        properties: { title: { type: "string" } },
      },
    },
  },
};

async function markThePage(page: Page) {
  await page.evaluate(() => {
    (window as unknown as { __noReload?: boolean }).__noReload = true;
  });
}

async function stillTheSamePage(page: Page) {
  return page.evaluate(
    () => (window as unknown as { __noReload?: boolean }).__noReload === true,
  );
}

test.describe("Dynamic route navigation", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page);
  });

  test("opening an installed plugin does not reload the page", async ({
    page,
  }) => {
    await page.goto("/dashboard/settings/plugins/");
    const row = page.getByRole("link", { name: /^lua,/i });
    await expect(row).toBeVisible({ timeout: 10000 });
    await markThePage(page);

    await row.click();

    await expect(page).toHaveURL(
      /\/dashboard\/settings\/plugins\/happyview-lua\/?$/,
    );
    await expect(
      page.getByRole("heading", { name: /lua/i }).first(),
    ).toBeVisible();
    expect(await stillTheSamePage(page)).toBe(true);
  });

  test("opening a lexicon does not reload the page", async ({ page }) => {
    const resp = await page.request.post("/admin/lexicons", {
      data: { lexicon_json: LEXICON, backfill: false },
    });
    if (!resp.ok()) {
      const text = await resp.text();
      if (!text.includes("already exists")) {
        throw new Error(`Failed to seed lexicon: ${resp.status()} ${text}`);
      }
    }

    try {
      await page.goto("/dashboard/lexicons/");
      const link = page
        .locator("table tbody tr", { hasText: LEXICON.id })
        .getByRole("link", { name: "View lexicon" });
      await expect(link).toBeVisible({ timeout: 10000 });
      await markThePage(page);

      await link.click();

      await expect(page).toHaveURL(
        new RegExp(
          `/dashboard/lexicons/${LEXICON.id.replace(/\./g, "\\.")}/?$`,
        ),
      );
      await expect(page.getByText(LEXICON.id).first()).toBeVisible();
      expect(await stillTheSamePage(page)).toBe(true);
    } finally {
      await page.request.delete(`/admin/lexicons/${LEXICON.id}`);
    }
  });
});
