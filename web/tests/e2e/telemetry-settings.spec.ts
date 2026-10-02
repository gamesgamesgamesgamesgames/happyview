import { expect, test } from "@playwright/test";

import { loginAsTestAdmin } from "./auth-helper";

test.describe("telemetry settings", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page);
    // The suite runs serially against one shared database
    // (playwright.config.ts: workers: 1, fullyParallel: false), so without
    // this reset a mutation from an earlier test — e.g. "the preview omits
    // lexicon names until the toggle is on" turning mode to "manual" and
    // lexicon_names on — leaks into the next test's starting state. Reset via
    // the API, before navigating, so every test starts from the same clean
    // baseline regardless of run order.
    await page.request.put("/admin/settings/telemetry", {
      data: {
        mode: "off",
        contact: "",
        lexicon_names: false,
        lexicon_structure: false,
        lexicon_documents: false,
      },
    });
    await page.goto("/dashboard/settings/telemetry");
  });

  test("defaults to off with the lexicon toggles disabled", async ({ page }) => {
    await expect(page.getByRole("radio", { name: /don't send/i })).toBeChecked();
    // Nothing may be shared while the mode is off.
    await expect(page.getByRole("switch", { name: /lexicon names/i })).toBeDisabled();
  });

  test("shows the exact payload before anything is enabled", async ({ page }) => {
    // The operator must be able to read what would be sent *before* consenting.
    await expect(page.getByTestId("telemetry-preview")).toContainText("schema_version");
    await expect(page.getByTestId("telemetry-preview")).toContainText("totals");
  });

  test("the preview omits lexicon names until the toggle is on", async ({ page }) => {
    await expect(page.getByTestId("telemetry-preview")).not.toContainText('"names"');

    await page.getByRole("radio", { name: /review each/i }).click();
    await page.getByRole("switch", { name: /lexicon names/i }).click();
    await expect(page.getByTestId("telemetry-preview")).toContainText('"names"');
  });

  test("manual mode reveals the send button", async ({ page }) => {
    await expect(page.getByRole("button", { name: /send and compare/i })).toHaveCount(0);

    await page.getByRole("radio", { name: /review each/i }).click();
    await expect(page.getByRole("button", { name: /send and compare/i })).toBeVisible();
  });

  test("the contact input is disabled while mode is off", async ({ page }) => {
    // Nothing may be shared while the mode is off, and contact info is no
    // exception — it attaches an operator's identity to their reports.
    await expect(page.getByLabel(/contact/i)).toBeDisabled();
  });

  test("a contact value persists across a reload", async ({ page }) => {
    await page.getByRole("radio", { name: /review each/i }).click();

    const contactInput = page.getByLabel(/contact/i);
    await expect(contactInput).toBeEnabled();
    await contactInput.fill("ops@example.com");

    // Commits on blur, not on keystroke — trigger it explicitly and wait for
    // the PATCH to land before reloading, so the reload can't race the write.
    const patched = page.waitForResponse(
      (res) =>
        res.url().includes("/admin/settings/telemetry") &&
        res.request().method() === "PUT",
    );
    await contactInput.blur();
    await patched;

    await page.reload();
    await expect(page.getByLabel(/contact/i)).toHaveValue("ops@example.com");
  });

  test("a send with no reachable collector reports a failure, not a silent no-op", async ({ page }) => {
    await page.getByRole("radio", { name: /review each/i }).click();
    await page.getByRole("button", { name: /send and compare/i }).click();

    // The e2e stack has no collector. Either outcome is acceptable; silence is
    // not — the operator pressed a button and must learn what happened.
    await expect(
      page.getByText(/couldn't reach|not enough comparable instances/i),
    ).toBeVisible({ timeout: 20_000 });
  });
});
