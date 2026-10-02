import { test, expect } from "@playwright/test";
import { loginAsTestAdmin } from "./auth-helper";

/**
 * Regression coverage for issue #85.
 *
 * The Add User field is named `did`, and before the fix it was stored verbatim
 * — so typing a handle produced a row whose `did` column held a handle. Nothing
 * failed at that point: the user appeared in the list, and the damage only
 * surfaced later at login, where authorization matches the DID in the OAuth
 * session exactly and the row could never match.
 *
 * That is why these tests assert on what the row *becomes*, not merely that the
 * request was accepted. An assertion that "Add succeeded" is exactly the signal
 * the original bug produced.
 */

async function openAddUserDialog(page: import("@playwright/test").Page) {
  await page.getByRole("button", { name: "Add User" }).click();
  await expect(page.getByRole("heading", { name: "Add User" })).toBeVisible();
}

/**
 * Synthetic DIDs have no DID document, so the real resolve endpoint refuses
 * them. Handles fall through to the server so resolution failures stay real.
 */
async function mockSyntheticDidResolution(page: import("@playwright/test").Page) {
  await page.route("**/admin/identity/resolve?**", (route) => {
    const identifier = new URL(route.request().url()).searchParams.get("identifier");
    if (identifier?.startsWith("did:plc:e2e")) {
      return route.fulfill({ json: { did: identifier, handle: null } });
    }
    return route.continue();
  });
}

async function enterIdentifier(
  page: import("@playwright/test").Page,
  identifier: string,
) {
  const input = page.getByLabel("Handle or DID");
  await input.fill(identifier);
  await input.press("Enter");
}

async function submitIdentifier(
  page: import("@playwright/test").Page,
  identifier: string,
) {
  await enterIdentifier(page, identifier);
  await expect(page.locator('[data-status="resolved"]')).toHaveCount(1);
  await page.getByRole("button", { name: "Add", exact: true }).click();
}

test.describe("Add User", () => {
  test.beforeEach(async ({ page }) => {
    await mockSyntheticDidResolution(page);
    await loginAsTestAdmin(page);
    await page.goto("/dashboard/settings/users");
  });

  test("the field accepts a handle, not only a DID", async ({ page }) => {
    await openAddUserDialog(page);

    // The label is the whole discoverability fix: operators typed handles into
    // a field labelled "DID" because nothing told them not to.
    await expect(page.getByLabel("Handle or DID")).toBeVisible();
    await expect(
      page.getByPlaceholder("alice.bsky.social or did:plc:..."),
    ).toBeVisible();
  });

  test("adding by DID stores that exact DID", async ({ page }) => {
    const did = `did:plc:e2eadduser${Date.now()}`;

    await openAddUserDialog(page);
    await submitIdentifier(page, did);

    await expect(page.getByText("User added")).toBeVisible();

    // Scoped to the table: the dialog's closing animation can briefly leave
    // its own account chip, which shows the same DID, still in the DOM.
    await expect(
      page.locator("table").getByText(did, { exact: true }),
    ).toBeVisible();
  });

  test("a handle that cannot be resolved is refused, and no user is created", async ({
    page,
  }) => {
    // `.invalid` is reserved as permanently non-resolvable (RFC 2606), so this
    // fails at resolution rather than depending on what happens to be
    // registered.
    const handle = "nonexistent-handle.invalid";

    await openAddUserDialog(page);
    await enterIdentifier(page, handle);

    // Resolution happens as the account is entered, so the refusal shows
    // on the tag and the dialog cannot submit it.
    await expect(page.locator('[data-status="error"]')).toHaveCount(1);
    await expect(page.getByRole("button", { name: "Add", exact: true })).toBeDisabled();

    await page.keyboard.press("Escape");
    await page.reload();
    await expect(page.getByText(handle, { exact: true })).toHaveCount(0);
  });

  test("only one account can be entered", async ({ page }) => {
    await openAddUserDialog(page);
    await enterIdentifier(page, `did:plc:e2esingle${Date.now()}`);
    await expect(page.locator('[data-status="resolved"]')).toHaveCount(1);
    await expect(page.getByLabel("Handle or DID")).toBeHidden();
  });

  test("adding the same account twice reports a conflict rather than a server error", async ({
    page,
  }) => {
    const did = `did:plc:e2eduplicate${Date.now()}`;

    await openAddUserDialog(page);
    await submitIdentifier(page, did);
    await expect(page.getByText("User added")).toBeVisible();

    await openAddUserDialog(page);
    await submitIdentifier(page, did);

    // `toastError` collapses anything naming "already exists" into this copy;
    // a 500 from the UNIQUE constraint, which is what this returned before,
    // would fall through to the generic branch instead.
    await expect(page.getByText("Failed to add user: already exists")).toBeVisible();
  });
});
