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

async function submitIdentifier(
  page: import("@playwright/test").Page,
  identifier: string,
) {
  await page.getByLabel("Handle or DID").fill(identifier);
  await page.getByRole("button", { name: "Add", exact: true }).click();
}

test.describe("Add User", () => {
  test.beforeEach(async ({ page }) => {
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
    await expect(page.getByText(did, { exact: true })).toBeVisible();
  });

  test("a handle that cannot be resolved is refused, and no user is created", async ({
    page,
  }) => {
    // `.invalid` is reserved as permanently non-resolvable (RFC 2606), so this
    // fails at resolution rather than depending on what happens to be
    // registered.
    const handle = "nonexistent-handle.invalid";

    await openAddUserDialog(page);
    await submitIdentifier(page, handle);

    await expect(page.getByText("Failed to add user")).toBeVisible();

    // The regression itself: before the fix this row existed, listing the
    // handle where a DID belongs, and the account could never sign in.
    await page.reload();
    await expect(page.getByText(handle, { exact: true })).toHaveCount(0);
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
