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

  await expect(
    page.getByRole("region", { name: "HappyView update" }),
  ).toBeVisible();

  await notice.getByRole("button", { name: "Dismiss" }).click();
  await expect(notice).toHaveCount(0);
  // Focus lands on the page heading, not on <body>.
  await expect(page.getByRole("heading", { level: 1 })).toBeFocused();
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

/** A check that has never succeeded says so, and why the last one failed. */
test("a failed check is shown on the About page", async ({ page }) => {
  await serve(
    page,
    status({
      state: "unknown",
      available: null,
      checkedAt: null,
      lastError: "could not resolve the publisher's PDS: HTTP 503",
    }),
  );
  await loginAsTestAdmin(page);
  await page.goto("/dashboard/about");

  await expect(page.getByText("No check has succeeded yet.")).toBeVisible();
  await expect(
    page.getByText(
      "The last check failed: could not resolve the publisher's PDS: HTTP 503",
    ),
  ).toBeVisible();
  await expect(page.getByTestId("happyview-update-notice")).toHaveCount(0);
});

/** A newer major is mentioned on the About page, never offered. */
test("another line is mentioned, not offered", async ({ page }) => {
  await serve(
    page,
    status({
      state: "current",
      available: null,
      otherLine: {
        version: "3.0.0",
        channel: "latest",
        releaseUrl:
          "https://github.com/gamesgamesgamesgamesgames/happyview/releases/tag/v3.0.0",
        upgradeGuideUrl: GUIDE,
        severity: "routine",
      },
    }),
  );
  await loginAsTestAdmin(page);
  await page.goto("/dashboard/about");

  await expect(
    page.getByText("HappyView 3.0.0 is out on the latest channel."),
  ).toBeVisible();
  await expect(page.getByTestId("happyview-update-notice")).toHaveCount(0);
});

for (const [state, text] of [
  ["disabled", "Update checks are off (UPDATE_CHECK=off)."],
  ["dev", "A development build, so there are no updates to check for."],
] as const) {
  test(`the About page explains a ${state} check`, async ({ page }) => {
    await serve(
      page,
      status({ state, channel: null, available: null, checkedAt: null }),
    );
    await loginAsTestAdmin(page);
    await page.goto("/dashboard/about");

    await expect(page.getByText(text)).toBeVisible();
    await expect(page.getByTestId("happyview-update-notice")).toHaveCount(0);
  });
}
