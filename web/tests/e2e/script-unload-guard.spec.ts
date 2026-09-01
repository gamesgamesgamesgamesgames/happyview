import { test, expect } from "@playwright/test";
import { loginAsTestAdmin } from "./auth-helper";

const JOB_TYPE = "test.e2e.unloadguard";
const TRIGGER_ID = `job.run:${JOB_TYPE}`;

/**
 * Asks the page whether its `beforeunload` guard is currently armed, without
 * involving a real navigation. A guard that calls `preventDefault()` is what
 * makes Chrome raise the "Leave site? Changes you made may not be saved."
 * prompt, so `defaultPrevented` is the prompt's precondition.
 *
 * Testing it this way rather than through a real navigation is deliberate:
 * Playwright auto-accepts beforeunload prompts, so a navigation-based test
 * would pass whether or not the guard was armed.
 */
async function unloadGuardArmed(page: import("@playwright/test").Page) {
  return page.evaluate(() => {
    const event = new Event("beforeunload", { cancelable: true });
    window.dispatchEvent(event);
    return event.defaultPrevented;
  });
}

async function fillNewJobScript(page: import("@playwright/test").Page) {
  await page.goto("/dashboard/settings/scripts/new");
  await page.locator("#source-pick").click();
  await page.getByRole("option", { name: /Job/ }).click();
  await page.locator("#job-type-input").fill(JOB_TYPE);
}

test.describe("New script unload guard", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page);
  });

  test.afterEach(async ({ page }) => {
    await page.request.delete(
      `/admin/scripts/${encodeURIComponent(TRIGGER_ID)}`,
    );
  });

  test("guards unsaved changes against leaving the page", async ({ page }) => {
    await page.goto("/dashboard/settings/scripts/new");
    expect(await unloadGuardArmed(page)).toBe(false);

    await fillNewJobScript(page);
    expect(await unloadGuardArmed(page)).toBe(true);
  });

  test("does not warn about losing work while saving it", async ({ page }) => {
    await fillNewJobScript(page);

    // Under `output: "export"` the [id] detail route has no prerendered
    // payload, so the post-save router.push is a full page load. It has to be
    // stopped for this page to stay inspectable, and it must be stopped by
    // *aborting* it — a failed navigation leaves the current document in
    // place, which is the state an operator is left in when they answer
    // "Cancel" to the browser's prompt.
    await page.route(
      (url) => url.pathname.includes(encodeURIComponent(TRIGGER_ID)),
      async (route) => {
        if (route.request().isNavigationRequest()) {
          await route.abort();
        } else {
          await route.continue();
        }
      },
    );

    const saved = page.waitForResponse(
      (r) =>
        r.url().endsWith("/admin/scripts") && r.request().method() === "POST",
    );
    // Matches the button through its "Creating..." label too, so the assertions
    // below distinguish "still spinning" from "gone".
    const createButton = page
      .locator("footer button")
      .filter({ hasText: /Creat/ });
    await expect(createButton).toBeEnabled({ timeout: 3000 });
    await createButton.click();
    expect((await saved).ok()).toBe(true);

    // Both facts are read in a single round-trip on purpose. The held
    // navigation keeps this document alive but not indefinitely, and two
    // separate assertions leave a window for it to go away in between.
    await expect
      .poll(
        () =>
          page.evaluate(() => {
            const event = new Event("beforeunload", { cancelable: true });
            window.dispatchEvent(event);
            const button = Array.from(
              document.querySelectorAll("footer button"),
            ).find((b) => /Creat/.test(b.textContent ?? ""));
            return {
              // The work is persisted, so nothing is at risk and the browser's
              // "Leave site?" prompt would be warning about nothing.
              warnsOnLeave: event.defaultPrevented,
              // ...and the button must not be left spinning forever.
              stillSaving: button?.textContent?.includes("Creating") ?? null,
            };
          }),
        { timeout: 5000 },
      )
      .toEqual({ warnsOnLeave: false, stillSaving: false });
  });
});
