import { test, expect, type Page } from "@playwright/test";
import {
  getOauthSessionTokenState,
  loginAsTestAdmin,
  resetServiceIdentity,
  restoreServiceIdentity,
  setServiceIdentityMode,
  snapshotServiceIdentity,
} from "./auth-helper";

const PDS_URL = "http://localhost:3100";
const SCHEMA = "com.atproto.lexicon.schema";
const SCHEMA_SCOPE = `repo:${SCHEMA}?action=create&action=update&action=delete`;

async function createPdsAccount(): Promise<{ did: string; handle: string }> {
  const suffix = Date.now().toString(36);
  const handle = `testuser-${suffix}.test`;

  const resp = await fetch(`${PDS_URL}/xrpc/com.atproto.server.createAccount`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      email: `testuser-${suffix}@example.com`,
      handle,
      password: "Test-password-e2e-123",
    }),
  });

  if (!resp.ok) {
    const body = await resp.text();
    throw new Error(`PDS createAccount failed (${resp.status}): ${body}`);
  }

  const data = (await resp.json()) as { did: string; handle: string };
  return { did: data.did, handle: data.handle ?? handle };
}

async function getSchemaRecord(did: string, nsid: string) {
  const url = new URL(`${PDS_URL}/xrpc/com.atproto.repo.getRecord`);
  url.searchParams.set("repo", did);
  url.searchParams.set("collection", SCHEMA);
  url.searchParams.set("rkey", nsid);
  return fetch(url);
}

function lexiconRow(page: Page, nsid: string) {
  return page.locator("table tbody tr", {
    has: page.getByText(nsid, { exact: true }),
  });
}

test.describe("Setup - Attach Account", () => {
  // The publishing test reuses the session the OAuth test leaves behind.
  test.describe.configure({ mode: "serial" });

  let account: { did: string; handle: string };

  test.beforeAll(async () => {
    account = await createPdsAccount();
    await resetServiceIdentity();
  });

  test("attach_account flow reaches authenticate step", async ({ page }) => {
    await loginAsTestAdmin(page);
    await page.goto("/setup");
    await expect(page.getByText(/set up your service identity/i)).toBeVisible({
      timeout: 10000,
    });

    // Select "Use an existing AT Protocol account"
    await page.getByText(/use an existing at protocol account/i).click();
    await page.getByRole("button", { name: /continue/i }).click();

    // Configure step: enter the PDS account DID
    const identifierInput = page.getByLabel(/handle or did/i);
    await expect(identifierInput).toBeVisible({ timeout: 5000 });
    await identifierInput.fill(account.did);

    // Wait for the typeahead dropdown and select the suggestion
    const suggestion = page.getByRole("option").first();
    await expect(suggestion).toBeVisible({ timeout: 5000 });
    await suggestion.click();

    // Click continue to submit the identity
    const continueButton = page.getByRole("button", { name: /continue/i });
    await expect(continueButton).toBeEnabled({ timeout: 5000 });
    await continueButton.click();

    // Should reach the authenticate step
    await expect(page.getByText(/sign in to verify ownership/i)).toBeVisible({
      timeout: 10000,
    });

    // Verify the authenticate button contains the account handle
    await expect(
      page.getByRole("button", {
        name: new RegExp(`authenticate as.*${account.handle}`, "i"),
      }),
    ).toBeVisible();
  });

  test("full OAuth flow completes through PDS authorization", async ({
    page,
  }) => {
    await resetServiceIdentity();
    await loginAsTestAdmin(page);
    await page.goto("/setup");

    // Select "Use an existing AT Protocol account"
    await expect(page.getByText(/set up your service identity/i)).toBeVisible({
      timeout: 10000,
    });
    await page.getByText(/use an existing at protocol account/i).click();
    await page.getByRole("button", { name: /continue/i }).click();

    // Enter the DID directly without selecting from the typeahead so
    // attachedHandle stays null and the OAuth flow uses the DID.
    // Handle resolution for .test domains won't work from inside Docker.
    const identifierInput = page.getByLabel(/handle or did/i);
    await expect(identifierInput).toBeVisible({ timeout: 5000 });
    await identifierInput.fill(account.did);

    // Dismiss any typeahead dropdown that appears
    await page.keyboard.press("Escape");

    const continueButton = page.getByRole("button", { name: /continue/i });
    await expect(continueButton).toBeEnabled({ timeout: 5000 });
    await continueButton.click();

    // Reach the authenticate step
    await expect(page.getByText(/sign in to verify ownership/i)).toBeVisible({
      timeout: 10000,
    });

    // Click "Authenticate as @handle" — this triggers the OAuth flow:
    // 1. Frontend fetches /auth/login?handle=<did> to get the authorization URL
    // 2. HappyView's backend makes a PAR request to the PDS via Caddy (HTTPS)
    // 3. Frontend redirects to the PDS OAuth login page
    const authButton = page.getByRole("button", {
      name: /authenticate as/i,
    });
    // The attached account also publishes the instance's lexicons, so it is
    // asked for the schema scope as well as identity:*.
    const login = page.waitForRequest(/\/auth\/login\?/);
    await authButton.click();
    const requested = new URL((await login).url()).searchParams.get("scope");
    expect(requested?.split(" ")).toEqual(
      expect.arrayContaining(["atproto", "identity:*", SCHEMA_SCOPE]),
    );

    // Wait for redirect to PDS OAuth login page (served via Caddy at pds.localhost)
    await page.waitForURL(/pds\.localhost/, { timeout: 30000 });

    // Fill in credentials on the PDS OAuth login form
    // The PDS login page has: username input (#username), password input (#password), submit button
    const usernameInput = page.locator("#username");
    await expect(usernameInput).toBeVisible({ timeout: 10000 });
    await usernameInput.fill(account.handle);

    const passwordInput = page.locator("#password");
    await expect(passwordInput).toBeVisible({ timeout: 5000 });
    await passwordInput.fill("Test-password-e2e-123");

    // Submit the login form
    await page.locator("button[type='submit']").click();

    const authorizeButton = page.getByRole("button", { name: /^authorize$/i });
    const consentOrCallback = await Promise.race([
      page
        .waitForURL(/sslip\.io/, { timeout: 30000 })
        .then(() => "callback" as const),
      authorizeButton
        .waitFor({ timeout: 20000 })
        .then(() => "consent" as const)
        .catch((e: unknown) => `error: ${e}`),
    ]);
    expect(
      consentOrCallback === "consent" || consentOrCallback === "callback",
      `expected the PDS consent screen or a redirect back, got ${consentOrCallback} at ${page.url()}`,
    ).toBeTruthy();

    if (consentOrCallback === "consent") {
      await authorizeButton.click();
      await page.waitForURL(/sslip\.io/, { timeout: 15000 });
    }

    // We're back on HappyView after the OAuth callback.
    // The setup-attach-auth component detects the return via localStorage
    // and calls confirmAttachAuth to restore the admin session.
    // Then the wizard advances to the "verify" step.
    await expect(
      page.getByRole("tab", { name: "Verify", selected: true }),
    ).toBeVisible({ timeout: 15000 });

    // What the PDS granted, which is what it read from its copy of the client
    // metadata: a copy cached without the schema scope would leave it out.
    const { scope } = await getOauthSessionTokenState(account.did);
    expect(scope?.split(" ")).toContain(SCHEMA_SCOPE);
  });

  test("a lexicon publishes to the attached account and unpublishes from it", async ({
    page,
  }) => {
    test.setTimeout(120_000);
    const nsid = `test.e2e.attached${Date.now().toString(36)}.note`;
    // The wizard is still on its Verify step, which keeps the dashboard
    // behind the setup gate; completing it here skips the steps after it.
    const identity = await snapshotServiceIdentity();
    let ruleId: string | undefined;
    try {
      await setServiceIdentityMode("attach_account", {
        did: account.did,
        attachedAccountDid: account.did,
      });
      await loginAsTestAdmin(page);

      const seeded = await page.request.post("/admin/lexicons", {
        data: {
          lexicon_json: {
            lexicon: 1,
            id: nsid,
            defs: {
              main: {
                type: "record",
                key: "tid",
                record: { type: "object", properties: {} },
              },
            },
          },
          backfill: false,
        },
      });
      expect(seeded.ok(), await seeded.text()).toBeTruthy();

      const created = await page.request.post(
        "/admin/lexicon-publishing/rules",
        {
          data: { pattern: nsid, target_kind: "instance", auto_publish: false },
        },
      );
      expect(created.status(), await created.text()).toBe(201);
      const rule = await created.json();
      ruleId = rule.id;
      expect(rule.target.available).toBe(true);
      expect(rule.target.can_unpublish).toBe(true);
      expect(rule.target.did).toBe(account.did);

      await page.goto("/dashboard/settings/lexicon-publishing");
      await page.getByPlaceholder("Filter by NSID...").fill(nsid);
      const row = lexiconRow(page, nsid);
      await expect(row).toContainText("Not published");
      await row.getByRole("button", { name: "Publish", exact: true }).click();
      await expect(row).toContainText("Published", { timeout: 30000 });

      const record = await getSchemaRecord(account.did, nsid);
      expect(record.status, await record.clone().text()).toBe(200);
      const body = await record.json();
      expect(body.uri).toBe(`at://${account.did}/${SCHEMA}/${nsid}`);
      expect(body.value.$type).toBe(SCHEMA);
      expect(body.value.id).toBe(nsid);

      await row.getByRole("button", { name: `Unpublish ${nsid}` }).click();
      const dialog = page.getByRole("alertdialog");
      await expect(dialog).toContainText(`Unpublish ${nsid}?`);
      await dialog.getByRole("button", { name: "Unpublish" }).click();
      await expect(row).toContainText("Withdrawn", { timeout: 30000 });

      const gone = await getSchemaRecord(account.did, nsid);
      expect(gone.status).toBeGreaterThanOrEqual(400);
      expect(gone.status).toBeLessThan(500);
    } finally {
      // The afterAll's skip flow expects the wizard where the OAuth test
      // left it, so the identity goes back first.
      await restoreServiceIdentity(identity);
      if (ruleId) {
        await page.request.delete(`/admin/lexicon-publishing/rules/${ruleId}`);
      }
      await page.request.delete(`/admin/lexicons/${nsid}`);
    }
  });

  // Restore setup state for subsequent tests
  test.afterAll(async ({ browser }) => {
    const page = await browser.newPage();
    try {
      await loginAsTestAdmin(page);
      await page.goto("/setup");
      const skipCard = page.getByText(/skip for now/i);
      if (await skipCard.isVisible({ timeout: 3000 }).catch(() => false)) {
        await skipCard.click();
        await page.getByRole("button", { name: /continue/i }).click();
        await page.getByText(/help us build/i).waitFor({ timeout: 5000 });
        await page.getByRole("button", { name: /continue/i }).click();
        await expect(
          page.getByText("Your AppView is ready", { exact: true }),
        ).toBeVisible({ timeout: 5000 });
      }
    } finally {
      await page.close();
    }
  });
});
