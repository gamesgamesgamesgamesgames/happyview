import { test, expect, type Page } from "@playwright/test";
import { loginAsTestAdmin } from "./auth-helper";

const RELEASE =
  "https://github.com/gamesgamesgamesgamesgames/happyview/releases/tag/v2.17.4";
const GUIDE = "https://happyview.dev/docs/guides/upgrading";

function status(overrides: Record<string, unknown> = {}) {
  return {
    state: "available",
    current: "2.17.3",
    channel: "v2",
    available: {
      version: "2.17.4",
      channel: "v2",
      releaseUrl: RELEASE,
      upgradeGuideUrl: GUIDE,
      severity: "routine",
    },
    otherLine: null,
    checkedAt: "2026-11-12T00:00:00Z",
    lastError: null,
    managed: false,
    ...overrides,
  };
}

async function serve(page: Page, body: unknown) {
  await page.route("**/admin/update-status", (route) =>
    route.fulfill({ json: body }),
  );
}

/**
 * A newer HappyView on the instance's channel is announced at the top of the
 * dashboard, with the release notes and the upgrade guide, until dismissed.
 */
test("a newer HappyView is announced until dismissed", async ({ page }) => {
  await serve(page, status());
  await loginAsTestAdmin(page);
  await page.goto("/dashboard/about");

  const notice = page.getByTestId("happyview-update-notice");
  await expect(notice).toContainText(
    "HappyView 2.17.4 is available. You're on 2.17.3.",
  );
  await expect(
    notice.getByRole("link", { name: "Release notes" }),
  ).toHaveAttribute("href", RELEASE);
  await expect(
    notice.getByRole("link", { name: "How to upgrade" }),
  ).toHaveAttribute("href", GUIDE);

  await notice.getByRole("button", { name: "Dismiss" }).click();
  await expect(notice).toHaveCount(0);
  await page.reload();
  await expect(
    page.getByText("2.17.4 is available on the v2 channel."),
  ).toBeVisible();
  await expect(page.getByTestId("happyview-update-notice")).toHaveCount(0);
});

/** A managed instance is upgraded by its host, so no self-hosting guide. */
test("a managed instance is not pointed at the upgrade guide", async ({
  page,
}) => {
  await serve(page, status({ managed: true }));
  await loginAsTestAdmin(page);
  await page.goto("/dashboard/about");

  const notice = page.getByTestId("happyview-update-notice");
  await expect(notice).toContainText("Your hosting provider handles upgrades");
  await expect(
    notice.getByRole("link", { name: "How to upgrade" }),
  ).toHaveCount(0);
});

/** Nothing newer, nothing said. */
test("an up-to-date instance shows no notice", async ({ page }) => {
  await serve(page, status({ state: "current", available: null }));
  await loginAsTestAdmin(page);
  await page.goto("/dashboard/about");

  await expect(page.getByText("Up to date on the v2 channel.")).toBeVisible();
  await expect(page.getByTestId("happyview-update-notice")).toHaveCount(0);
});
