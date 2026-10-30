import { test, expect } from "@playwright/test";

// The dashboard loads `/config` before anything else. These answer it in
// place of the server to show what each failure looks like.

test.describe("Instance unavailable", () => {
  test("shows a connecting state while /config is in flight", async ({
    page,
  }) => {
    let release!: () => void;
    const held = new Promise<void>((resolve) => (release = resolve));
    await page.route("**/config", async (route) => {
      await held;
      await route.continue();
    });

    await page.goto("/login");
    const connecting = page.getByText("Connecting to HappyView…");
    await expect(connecting).toBeVisible();
    release();
    await expect(connecting).toBeHidden();
  });

  test("retries an unreachable instance and opens when it answers", async ({
    page,
  }) => {
    let failures = 2;
    await page.route("**/config", async (route) => {
      if (failures > 0) {
        failures -= 1;
        await route.fulfill({ status: 503, body: "starting" });
        return;
      }
      await route.continue();
    });

    await page.goto("/login");
    const unreachable = page.getByText("Can’t reach this HappyView instance");
    await expect(unreachable).toBeVisible();
    await expect(page.getByText("The server answered 503.")).toBeVisible();

    // The first retry comes a second later and fails too; the second, two
    // seconds after that, gets the real config.
    await expect(unreachable).toBeHidden({ timeout: 10_000 });
    expect(failures).toBe(0);
  });

  test("retries at once when asked", async ({ page }) => {
    // Refused until the button is clicked. The click waits for the first
    // automatic retry to fail, so the next one is two seconds off and only
    // the button can open the page within one.
    let requests = 0;
    let clicked = false;
    await page.route("**/config", async (route) => {
      requests += 1;
      if (!clicked) {
        await route.abort("connectionrefused");
        return;
      }
      await route.continue();
    });

    await page.goto("/login");
    const unreachable = page.getByText("Can’t reach this HappyView instance");
    await expect(unreachable).toBeVisible();
    await expect.poll(() => requests).toBe(2);
    clicked = true;
    await page.getByRole("button", { name: "Try again now" }).click();
    await expect(unreachable).toBeHidden({ timeout: 1_000 });
    expect(requests).toBe(3);
  });

  test("names a host the instance does not serve, and does not retry", async ({
    page,
  }) => {
    let requests = 0;
    await page.route("**/config", async (route) => {
      requests += 1;
      await route.fulfill({ status: 421, body: "Unknown host" });
    });

    await page.goto("/login");
    await expect(
      page.getByText("This instance doesn’t answer on"),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Try again now" }),
    ).toHaveCount(0);
    await page.waitForTimeout(2_500);
    expect(requests).toBe(1);
  });
});
