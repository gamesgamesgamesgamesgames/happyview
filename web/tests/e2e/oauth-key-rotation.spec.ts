import { execSync } from "child_process";
import { test, expect, type Page } from "@playwright/test";
import {
  loginAsTestAdmin,
  resetServiceIdentity,
  setServiceIdentityMode,
  getOauthSessionSigningKid,
  getOauthSessionTokenState,
  waitForRealAccessTokenExpiry,
  createFakeOauthSessionPinnedToKid,
} from "./auth-helper";

const PDS_URL = "http://localhost:3100";
const PDS_PASSWORD = "Test-password-e2e-123";
const HAPPYVIEW_CONTAINER = "happyview-e2e-happyview-1";

async function createPdsAccount(): Promise<{ did: string; handle: string }> {
  const suffix =
    Date.now().toString(36) + Math.random().toString(36).slice(2, 6);
  const handle = `keyrotation-${suffix}.test`;

  const resp = await fetch(`${PDS_URL}/xrpc/com.atproto.server.createAccount`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      email: `keyrotation-${suffix}@example.com`,
      handle,
      password: PDS_PASSWORD,
    }),
  });
  if (!resp.ok) {
    throw new Error(
      `PDS createAccount failed (${resp.status}): ${await resp.text()}`,
    );
  }
  const data = (await resp.json()) as { did: string; handle?: string };
  return { did: data.did, handle: data.handle ?? handle };
}

async function completeAttachAccountOAuth(
  page: Page,
  account: { did: string; handle: string },
): Promise<void> {
  await page.goto("/setup");
  await expect(page.getByText(/set up your service identity/i)).toBeVisible({
    timeout: 10000,
  });

  await page.getByText(/use an existing at protocol account/i).click();
  await page.getByRole("button", { name: /continue/i }).click();

  const identifierInput = page.getByLabel(/handle or did/i);
  await expect(identifierInput).toBeVisible({ timeout: 5000 });
  await identifierInput.fill(account.did);
  await page.keyboard.press("Escape");

  const continueButton = page.getByRole("button", { name: /continue/i });
  await expect(continueButton).toBeEnabled({ timeout: 5000 });
  await continueButton.click();

  await expect(page.getByText(/sign in to verify ownership/i)).toBeVisible({
    timeout: 10000,
  });

  const authButton = page.getByRole("button", { name: /authenticate as/i });
  await authButton.click();

  await page.waitForURL(/pds\.localhost/, { timeout: 60000 });
  await page.locator("#username").fill(account.handle);
  await page.locator("#password").fill(PDS_PASSWORD);
  await page.locator("button[type='submit']").click();

  // These waits were originally capped at 30s because the test
  // itself only had Playwright's 30s default. Waiting longer was pointless
  // because the test would die first. Now the describe allows 180s, so the cap can
  // reflect what a real OAuth round-trip against a containerised PDS on a
  // loaded machine actually needs. Nothing here asserts the flow is *fast*;
  // the assertion is that it completes.
  const authorizeButton = page.getByRole("button", { name: /^authorize$/i });
  const outcome = await Promise.any([
    page
      .waitForURL(/sslip\.io/, { timeout: 90000 })
      .then(() => "callback" as const),
    authorizeButton.waitFor({ timeout: 90000 }).then(() => "consent" as const),
  ]).catch(() => "neither consent nor callback within 90s");
  expect(
    outcome === "consent" || outcome === "callback",
    `expected the PDS consent screen or a redirect back, got ${outcome} at ${page.url()}`,
  ).toBeTruthy();
  if (outcome === "consent") {
    await authorizeButton.click();
    await page.waitForURL(/sslip\.io/, { timeout: 15000 });
  }

  await expect(
    page.getByRole("tab", { name: "Verify", selected: true }),
  ).toBeVisible({ timeout: 15000 });
}

test.describe("OAuth instance key rotation — live proof against a real PDS", () => {
  test.describe.configure({ timeout: 180_000 });

  let accountA: { did: string; handle: string };
  let accountB: { did: string; handle: string };
  let kidBeforeRotation: string;
  let kidAfterRotation: string;

  test.afterAll(async () => {
    await resetServiceIdentity();
  });

  test("a session pinned to the retiring key survives a forced refresh after rotation", async ({
    page,
  }) => {
    test.setTimeout(900_000);

    accountA = await createPdsAccount();
    await resetServiceIdentity();
    await loginAsTestAdmin(page);

    await completeAttachAccountOAuth(page, accountA);

    await setServiceIdentityMode("attach_account", {
      did: accountA.did,
      attachedAccountDid: accountA.did,
    });

    const pinned = await getOauthSessionSigningKid(accountA.did);
    expect(
      pinned,
      "the real OAuth callback must have pinned signing_kid on the session it created",
    ).toBeTruthy();
    kidBeforeRotation = pinned!;

    const rotateResp = await page.request.post(
      "/admin/oauth/instance-key/rotate",
    );
    expect(rotateResp.ok(), `rotate: ${await rotateResp.text()}`).toBeTruthy();
    const rotated = (await rotateResp.json()) as {
      kid: string;
      orphaned_sessions: number;
    };
    kidAfterRotation = rotated.kid;
    expect(kidAfterRotation).not.toBe(kidBeforeRotation);

    const { accessToken: accessTokenBefore } = await getOauthSessionTokenState(
      accountA.did,
    );

    await waitForRealAccessTokenExpiry(accountA.did);

    const triggerResp = await page.request.post(
      "/admin/service-entries/sync-plc/request",
      { data: {} },
    );
    const triggerBody = await triggerResp.text();

    expect(
      triggerResp.status(),
      `expected a 500 wrapping the PDS unit-deserialization failure (an authenticated-and-reached-the-PDS signature), got: ${triggerResp.status()} ${triggerBody}`,
    ).toBe(500);
    const correlationId = (
      JSON.parse(triggerBody) as { correlationId?: string }
    ).correlationId;
    expect(
      correlationId,
      `expected an AppError::Internal body with a correlationId, got: ${triggerBody}`,
    ).toBeTruthy();

    const logLine = execSync(
      `docker logs ${HAPPYVIEW_CONTAINER} 2>&1 | grep -F ${JSON.stringify(correlationId)} || true`,
    ).toString();
    expect(
      logLine,
      `expected the logged internal error for correlationId ${correlationId} to be the PDS unit-deserialization failure, got: ${logLine || "(no matching log line found)"}`,
    ).toContain("invalid type: map, expected unit");

    const after = await getOauthSessionTokenState(accountA.did);
    expect(after.accessToken).not.toBe(accessTokenBefore);
    expect(
      after.expiresAt && new Date(after.expiresAt).getTime() > Date.now(),
      `expected a future expiry after refresh, got ${after.expiresAt}`,
    ).toBeTruthy();

    const pinnedAfter = await getOauthSessionSigningKid(accountA.did);
    expect(
      pinnedAfter,
      "signing_kid must stay pinned to the key that established the session",
    ).toBe(kidBeforeRotation);
  });

  test("a freshly established session after rotation pins to the new current key", async ({
    page,
  }) => {
    test.skip(
      !kidAfterRotation,
      "requires the rotation from the previous test",
    );

    accountB = await createPdsAccount();
    await resetServiceIdentity();
    await loginAsTestAdmin(page);

    await completeAttachAccountOAuth(page, accountB);

    const pinnedB = await getOauthSessionSigningKid(accountB.did);
    expect(
      pinnedB,
      "a session established after rotation must pin to the new current key, not the retired one",
    ).toBe(kidAfterRotation);
    expect(pinnedB).not.toBe(kidBeforeRotation);
  });

  test("the same account logging in again after rotation moves the pin to the new key", async ({
    page,
  }) => {
    test.skip(
      !kidAfterRotation,
      "requires the rotation and accountA session from the first test",
    );

    await resetServiceIdentity();
    await loginAsTestAdmin(page);

    await completeAttachAccountOAuth(page, accountA);

    const pinnedAgain = await getOauthSessionSigningKid(accountA.did);
    expect(
      pinnedAgain,
      "a second real login for the same account must move the pin to the key current at the time of THIS exchange, not keep the stale pin from before rotation",
    ).toBe(kidAfterRotation);
    expect(pinnedAgain).not.toBe(kidBeforeRotation);
  });
});

test.describe("OAuth instance key dashboard — list and revoke", () => {
  test.beforeEach(async () => {
    await setServiceIdentityMode("not_exposed");
  });

  test.afterAll(async () => {
    await resetServiceIdentity();
  });

  test("lists current/retiring keys and revokes a retiring key from the UI", async ({
    page,
  }) => {
    await loginAsTestAdmin(page);

    const before = await page.request.get("/admin/oauth/instance-key");
    expect(before.ok(), `list: ${await before.text()}`).toBeTruthy();
    const beforeKeys = (
      (await before.json()) as {
        keys: { kid: string; status: string; session_count: number }[];
      }
    ).keys;
    const currentBefore = beforeKeys.find((k) => k.status === "current");
    expect(
      currentBefore,
      "expected an existing current instance key",
    ).toBeTruthy();
    const kid = currentBefore!.kid;
    const sessionCount = currentBefore!.session_count;

    await page.goto("/dashboard/settings/oauth-keys");
    await expect(page.getByText("Retiring and revoked keys")).toBeVisible();

    const rows = page.locator("div.rounded-lg.border");
    const currentRow = rows
      .filter({ hasText: kid })
      .filter({ hasText: "Current" });
    await expect(currentRow).toBeVisible({ timeout: 10000 });
    await expect(
      currentRow.getByRole("button", { name: /revoke now/i }),
    ).toHaveCount(0);

    await page.getByRole("button", { name: /^generate new key$/i }).click();
    await page
      .getByRole("alertdialog")
      .getByRole("button", { name: /^generate new key$/i })
      .click();
    await expect(
      page.getByText(/generated a new instance signing key/i),
    ).toBeVisible({ timeout: 10000 });

    const retiringRow = rows
      .filter({ hasText: kid })
      .filter({ hasText: "Retiring" });
    await expect(retiringRow).toBeVisible({ timeout: 10000 });
    await expect(
      retiringRow.getByText(new RegExp(`${sessionCount} live session`, "i")),
    ).toBeVisible();

    await retiringRow.getByRole("button", { name: /revoke now/i }).click();

    const dialog = page.getByRole("alertdialog");
    await expect(dialog).toBeVisible({ timeout: 3000 });
    await expect(dialog.getByText(/leaked or compromised/i)).toBeVisible();
    const expectedSessionCopy =
      sessionCount > 0
        ? new RegExp(`${sessionCount} live session.* will be destroyed`, "i")
        : /no live sessions are pinned to this key/i;
    await expect(dialog.getByText(expectedSessionCopy)).toBeVisible();

    await dialog.getByRole("button", { name: /^revoke now$/i }).click();
    await expect(page.getByText(/revoked instance signing key/i)).toBeVisible({
      timeout: 10000,
    });

    const revokedRow = rows
      .filter({ hasText: kid })
      .filter({ hasText: "Revoked" });
    await expect(revokedRow).toBeVisible({ timeout: 10000 });
    await expect(
      revokedRow.getByRole("button", { name: /revoke now/i }),
    ).toHaveCount(0);

    const after = await page.request.get("/admin/oauth/instance-key");
    const afterKeys = (
      (await after.json()) as {
        keys: { kid: string; status: string }[];
      }
    ).keys;
    expect(afterKeys.find((k) => k.kid === kid)?.status).toBe("revoked");
  });

  test("revoking a retiring key with live sessions shows the destructive copy and destroys them", async ({
    page,
  }) => {
    await loginAsTestAdmin(page);

    const rotate1 = await page.request.post("/admin/oauth/instance-key/rotate");
    expect(rotate1.ok(), `rotate: ${await rotate1.text()}`).toBeTruthy();
    const { kid: targetKid } = (await rotate1.json()) as { kid: string };

    const suffix =
      Date.now().toString(36) + Math.random().toString(36).slice(2, 6);
    const sessionDids = [
      `did:plc:e2e-fake-session-${suffix}-a`,
      `did:plc:e2e-fake-session-${suffix}-b`,
    ];
    for (const did of sessionDids) {
      await createFakeOauthSessionPinnedToKid(did, targetKid);
      const pinned = await getOauthSessionSigningKid(did);
      expect(
        pinned,
        `expected ${did}'s session to be pinned to ${targetKid}`,
      ).toBe(targetKid);
    }

    const rotate2 = await page.request.post("/admin/oauth/instance-key/rotate");
    expect(rotate2.ok(), `rotate: ${await rotate2.text()}`).toBeTruthy();

    const listResp = await page.request.get("/admin/oauth/instance-key");
    expect(listResp.ok(), `list: ${await listResp.text()}`).toBeTruthy();
    const listKeys = (
      (await listResp.json()) as {
        keys: { kid: string; status: string; session_count: number }[];
      }
    ).keys;
    const target = listKeys.find((k) => k.kid === targetKid);
    expect(
      target,
      "expected the key sessions were established against to still be listed",
    ).toBeTruthy();
    expect(target!.status).toBe("retiring");
    expect(
      target!.session_count,
      "expected exactly the 2 sessions planted above, before touching the UI — a mismatch here is a backend/count bug, not a UI rendering bug",
    ).toBe(2);

    await page.goto("/dashboard/settings/oauth-keys");
    const rows = page.locator("div.rounded-lg.border");
    const retiringRow = rows
      .filter({ hasText: targetKid })
      .filter({ hasText: "Retiring" });
    await expect(retiringRow).toBeVisible({ timeout: 10000 });
    await expect(retiringRow.getByText(/\b2 live sessions\b/i)).toBeVisible();

    await retiringRow.getByRole("button", { name: /revoke now/i }).click();

    const dialog = page.getByRole("alertdialog");
    await expect(dialog).toBeVisible({ timeout: 3000 });
    await expect(
      dialog.getByText(
        /^2 live sessions pinned to this key will be destroyed and their users signed out\.$/i,
      ),
    ).toBeVisible();

    await dialog.getByRole("button", { name: /^revoke now$/i }).click();
    await expect(
      page.getByText(/^2 sessions pinned to this key were destroyed\.$/i),
    ).toBeVisible({ timeout: 10000 });

    const revokedRow = rows
      .filter({ hasText: targetKid })
      .filter({ hasText: "Revoked" });
    await expect(revokedRow).toBeVisible({ timeout: 10000 });
    await expect(
      revokedRow.getByText(
        /\b2 sessions destroyed when this key was revoked\b/i,
      ),
    ).toBeVisible();

    const afterRevoke = await page.request.get("/admin/oauth/instance-key");
    const afterRevokeKeys = (
      (await afterRevoke.json()) as {
        keys: { kid: string; status: string; session_count: number }[];
      }
    ).keys;
    const revokedKey = afterRevokeKeys.find((k) => k.kid === targetKid);
    expect(revokedKey?.status).toBe("revoked");
    expect(
      revokedKey?.session_count,
      "session_counts_by_kid does not filter on key status, so this must still read 2 after revoke — it counts rows still pinned to this now-dead kid, not currently-usable sessions",
    ).toBe(2);
  });
});
