import { expect, test, type Page } from "@playwright/test";

import { loginAsTestAdmin } from "./auth-helper";

const TOAST = /help shape happyview/i;

/**
 * Stub the consent read.
 *
 * The suite runs serially against one shared database and there is no API to
 * *un*-answer the telemetry question — by design, since "never again" is the
 * whole point. Driving the prompt from real state would therefore work exactly
 * once, and only in whichever test happened to run first. Stubbing the read
 * makes each case independent of run order; the dismissal write below is left
 * unstubbed so it still exercises the real endpoint.
 */
async function withPrompted(page: Page, prompted: boolean) {
  await page.route(
    (url) => url.pathname === "/admin/settings/telemetry",
    async (route) => {
      if (route.request().method() !== "GET") return route.fallback();
      const res = await route.fetch();
      const body = await res.json();
      await route.fulfill({ json: { ...body, prompted } });
    },
  );
}

test.describe("telemetry prompt", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page);
  });

  test("asks an instance that has never been asked", async ({ page }) => {
    await withPrompted(page, false);
    await page.goto("/dashboard");

    await expect(page.getByText(TOAST)).toBeVisible();
    // The motivation has to be on the toast itself — this is the only time it
    // is ever shown, so "click through to find out why" is one click too many.
    await expect(page.getByText(/no records, handles, or dids/i)).toBeVisible();
  });

  test("stays quiet once the question has been answered", async ({ page }) => {
    await withPrompted(page, true);

    // Absence is only meaningful once the read the toast depends on has
    // actually landed — otherwise this passes by checking too early, which is
    // exactly how a broken guard would slip through. Wait for that response
    // specifically, then let the page settle so React has flushed.
    const consentRead = page.waitForResponse(
      (res) =>
        new URL(res.url()).pathname === "/admin/settings/telemetry" &&
        res.request().method() === "GET",
    );
    await page.goto("/dashboard");
    await consentRead;
    await page.waitForLoadState("networkidle");

    await expect(page.getByText(TOAST)).toHaveCount(0);
  });

  test("Review records the answer and opens the telemetry page", async ({
    page,
  }) => {
    await withPrompted(page, false);
    await page.goto("/dashboard");
    await expect(page.getByText(TOAST)).toBeVisible();

    const dismissed = page.waitForResponse(
      (res) =>
        res.url().includes("/admin/settings/telemetry/dismiss") &&
        res.request().method() === "POST",
    );
    await page.getByRole("button", { name: /^review$/i }).click();

    await dismissed;
    await expect(page).toHaveURL(/\/dashboard\/settings\/telemetry\/?$/);
  });

  test("dismissing records the answer, so it is never asked again", async ({
    page,
  }) => {
    await withPrompted(page, false);
    await page.goto("/dashboard");
    await expect(page.getByText(TOAST)).toBeVisible();

    const dismissed = page.waitForResponse(
      (res) =>
        res.url().includes("/admin/settings/telemetry/dismiss") &&
        res.request().method() === "POST",
    );
    await page.getByRole("button", { name: /close toast/i }).click();

    // The write is what makes it stick across browsers and admins; a toast
    // that merely disappears would be back on the next page load.
    const response = await dismissed;
    expect(response.ok()).toBe(true);
    await expect(page.getByText(TOAST)).toHaveCount(0);
  });
});
