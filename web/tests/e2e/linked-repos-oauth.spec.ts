import { test, expect } from "@playwright/test";
import {
  awaitConsentOrCallback,
  loginAsTestAdmin,
  submitPdsLogin,
} from "./auth-helper";

const PDS_URL = "http://localhost:3100";
const PDS_PASSWORD = "Test-password-e2e-123";

const TEST_NSID = "test.e2e.linkedoauth.note";
const TEST_LEXICON = {
  lexicon: 1,
  id: TEST_NSID,
  defs: {
    main: {
      type: "record",
      key: "tid",
      record: { type: "object", properties: { text: { type: "string" } } },
    },
  },
};

async function createPdsAccount(): Promise<{ did: string; handle: string }> {
  const suffix = Date.now().toString(36);
  const handle = `linkedrepo-${suffix}.test`;
  const resp = await fetch(`${PDS_URL}/xrpc/com.atproto.server.createAccount`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      email: `linkedrepo-${suffix}@example.com`,
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

/**
 * The one path nothing else covers: an invite link followed all the way through
 * a REAL PDS authorization. Every other test either calls `flow::complete`
 * directly or asserts an expected failure, which is exactly how a missing
 * `atproto` scope in the authorization request survived twelve reviews — the
 * first thing a real user does had never been exercised.
 */
test.describe("Linked Repos — real OAuth authorization", () => {
  test.describe.configure({ timeout: 180_000 });
  let account: { did: string; handle: string };
  let grantId: string;
  let inviteToken: string;

  test.beforeAll(async ({ browser }) => {
    account = await createPdsAccount();

    const page = await browser.newPage();
    await loginAsTestAdmin(page);

    const lex = await page.request.post("/admin/lexicons", {
      data: {
        lexicon_json: TEST_LEXICON,
        backfill: false,
        target_collection: TEST_NSID,
      },
    });
    if (!lex.ok() && !(await lex.text()).includes("already exists")) {
      throw new Error(`lexicon seed failed: ${lex.status()}`);
    }

    const created = await page.request.post("/admin/linked-repos", {
      data: {
        reason: "e2e real-oauth grant",
        scopes: `repo:${TEST_NSID}?action=create`,
      },
    });
    expect(created.ok(), `create grant: ${await created.text()}`).toBeTruthy();
    grantId = (await created.json()).id;

    const invited = await page.request.post(
      `/admin/linked-repos/${grantId}/invite`,
      { data: {} },
    );
    expect(invited.ok()).toBeTruthy();
    inviteToken = (await invited.json()).invite_url.split("token=")[1];

    await page.close();
  });

  test.afterAll(async ({ browser }) => {
    if (!grantId) return;
    const page = await browser.newPage();
    await loginAsTestAdmin(page);
    await page.request.delete(`/admin/linked-repos/${grantId}`);
    await page.close();
  });

  test("an invite link authorizes a real repo end to end", async ({ page }) => {
    // Deliberately unauthenticated: this is a stranger following a link.
    await page.goto(`/auth/linked-repo/start?token=${inviteToken}`);
    await expect(page).toHaveURL(/\/link\/start\/?\?token=/);

    // The landing page must say what is being asked before asking for anything.
    await expect(page.getByText(TEST_NSID).first()).toBeVisible({
      timeout: 15000,
    });

    // The DID, not the handle: `.test` domains cannot be resolved from inside
    // the Docker network, and `resolve_identifier` accepts either. The same
    // constraint is documented in setup-attach-account.spec.ts.
    await page.getByPlaceholder("you.bsky.social").fill(account.did);
    await page.getByRole("button", { name: /continue/i }).click();

    // -> PDS login
    await page.waitForURL(/pds\.localhost/, { timeout: 60000 });
    await submitPdsLogin(page, account.handle, PDS_PASSWORD);

    const authorizeButton = page.getByRole("button", { name: /^authorize$/i });
    const outcome = await awaitConsentOrCallback(page);
    expect(
      outcome === "consent" || outcome === "callback",
      `expected the PDS consent screen or a redirect back, got ${outcome} at ${page.url()}`,
    ).toBeTruthy();
    if (outcome === "consent") {
      await authorizeButton.click();
      await page.waitForURL(/sslip\.io/, { timeout: 60000 });
    }

    // The invitee lands on the PUBLIC result page — never the admin dashboard,
    // which they cannot view.
    await page.waitForURL(/\/link\/result/, { timeout: 60000 });
    const url = new URL(page.url());
    expect(
      url.searchParams.get("status"),
      `expected success, got ${page.url()}`,
    ).toBe("success");
    // This grant was created open (no handle), so the callback reports the DID
    // it bound rather than a handle.
    await expect(page.getByText(account.did).first()).toBeVisible();
  });

  test("the grant is now active and bound to the authorized DID", async ({
    browser,
  }) => {
    const page = await browser.newPage();
    await loginAsTestAdmin(page);

    const resp = await page.request.get("/admin/linked-repos");
    const { linked_repos } = await resp.json();
    const grant = linked_repos.find((g: { id: string }) => g.id === grantId);

    expect(grant, "the grant should still exist").toBeTruthy();
    expect(grant.status).toBe("active");
    expect(grant.did).toBe(account.did);
    expect(grant.last_error).toBeNull();

    await page.close();
  });

  test("linking retires the invite", async ({ browser }) => {
    const page = await browser.newPage();
    await loginAsTestAdmin(page);

    const resp = await page.request.get(
      `/admin/linked-repos/${grantId}/invites`,
    );
    expect(resp.ok()).toBeTruthy();
    const { invites } = await resp.json();
    expect(
      invites,
      "a completed link must leave no outstanding invites",
    ).toHaveLength(0);

    await page.close();
  });
});
