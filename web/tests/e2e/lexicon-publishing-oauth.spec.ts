import fs from "fs";
import path from "path";
import { test, expect, type Page } from "@playwright/test";
import {
  awaitConsentOrCallback,
  loginAsTestAdmin,
  restoreServiceIdentity,
  setServiceIdentityMode,
  snapshotServiceIdentity,
  submitPdsLogin,
} from "./auth-helper";

const PDS_URL = "http://localhost:3100";
const PDS_PASSWORD = "Test-password-e2e-123";
const SCHEMA = "com.atproto.lexicon.schema";
const SCOPES = `atproto repo:${SCHEMA}?action=create&action=update&action=delete`;

const NOTE = "test.e2e.lexpub.note";
// Covered by the same default but never published by hand, so a reconcile
// that runs is visible next to the withdrawn lexicon it must leave alone.
const DRAFT = "test.e2e.lexpub.draft";
// Covered by nothing.
const ASIDE = "test.e2e.lexpubaside.thing";
const PATTERN = "test.e2e.lexpub.*";
const PAGE = "/dashboard/settings/lexicon-publishing";

// Full-page captures for design review, written only when a directory is
// named, so an ordinary run leaves nothing behind.
const SCREENS = process.env.LEXPUB_SCREENS_DIR;

function lexicon(id: string, description?: string) {
  return {
    lexicon: 1,
    id,
    ...(description ? { description } : {}),
    defs: {
      main: {
        type: "record",
        key: "tid",
        record: { type: "object", properties: { text: { type: "string" } } },
      },
    },
  };
}

async function createPdsAccount(): Promise<{ did: string; handle: string }> {
  const suffix = Date.now().toString(36);
  const handle = `lexpub-${suffix}.test`;
  const resp = await fetch(`${PDS_URL}/xrpc/com.atproto.server.createAccount`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      email: `lexpub-${suffix}@example.com`,
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

async function getSchemaRecord(did: string, nsid: string) {
  const url = new URL(`${PDS_URL}/xrpc/com.atproto.repo.getRecord`);
  url.searchParams.set("repo", did);
  url.searchParams.set("collection", SCHEMA);
  url.searchParams.set("rkey", nsid);
  return fetch(url);
}

async function overviewEntry(page: Page, nsid: string) {
  const resp = await page.request.get("/admin/lexicon-publishing");
  expect(resp.ok()).toBeTruthy();
  const { lexicons } = await resp.json();
  return lexicons.find((l: { nsid: string }) => l.nsid === nsid);
}

function lexiconRow(page: Page, nsid: string) {
  return page.locator("table tbody tr", {
    has: page.getByText(nsid, { exact: true }),
  });
}

async function openPublishing(page: Page) {
  await page.goto(PAGE);
  await page.getByPlaceholder("Filter by NSID...").fill("test.e2e.lexpub");
  await expect(lexiconRow(page, NOTE)).toBeVisible();
}

const VIEWPORTS = [
  { name: "desktop", width: 1440, height: 900 },
  { name: "mobile", width: 390, height: 844 },
] as const;

/**
 * Captures the page in both themes at both sizes, then restores desktop light.
 * `reopen` re-opens a popover after each resize, since one opened at another
 * width stays anchored where its trigger used to be. `viewports` narrows the
 * sizes for something that exists only at some of them.
 */
async function capture(
  page: Page,
  name: string,
  reopen?: () => Promise<void>,
  viewports: readonly (typeof VIEWPORTS)[number][] = VIEWPORTS,
) {
  if (!SCREENS) return;
  fs.mkdirSync(SCREENS, { recursive: true });
  for (const viewport of viewports) {
    await page.setViewportSize(viewport);
    if (reopen) {
      await page.keyboard.press("Escape");
      await reopen();
    }
    for (const scheme of ["light", "dark"] as const) {
      await page.emulateMedia({ colorScheme: scheme });
      await page.waitForTimeout(300);
      await page.screenshot({
        path: path.join(SCREENS, `${name}-${viewport.name}-${scheme}.png`),
        fullPage: true,
      });
    }
  }
  await page.setViewportSize(VIEWPORTS[0]);
  await page.emulateMedia({ colorScheme: "light" });
}

/**
 * A lexicon published into a real linked repo on a real PDS, through the
 * dashboard: publish, republish after an edit, and a withdrawal that
 * auto-publish has to respect.
 */
test.describe("Lexicon publishing — real linked repo", () => {
  test.describe.configure({ mode: "serial", timeout: 180_000 });

  let account: { did: string; handle: string };
  let grantId: string;
  let inviteToken: string;
  let ruleId: string;
  let identity: Awaited<ReturnType<typeof snapshotServiceIdentity>>;

  test.beforeAll(async ({ browser }) => {
    // The attach-account project leaves the instance unset up, which sends
    // every dashboard page to the setup wizard. did:web completes setup
    // without offering the instance as a destination.
    identity = await snapshotServiceIdentity();
    await setServiceIdentityMode("did_web", { did: "did:web:localhost" });
    account = await createPdsAccount();

    const page = await browser.newPage();
    await loginAsTestAdmin(page);
    for (const id of [NOTE, DRAFT, ASIDE]) {
      const resp = await page.request.post("/admin/lexicons", {
        data: { lexicon_json: lexicon(id), backfill: false },
      });
      expect(resp.ok(), `seed ${id}: ${await resp.text()}`).toBeTruthy();
    }

    const created = await page.request.post("/admin/linked-repos", {
      data: { reason: "e2e lexicon publishing", scopes: SCOPES },
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
    // Later projects depend on the identity, so it goes back before any
    // other cleanup can fail and skip it. Undefined means the snapshot was
    // never taken, and restoring that would delete the identity outright.
    try {
      if (identity !== undefined) await restoreServiceIdentity(identity);
    } finally {
      const page = await browser.newPage();
      await loginAsTestAdmin(page);
      if (ruleId) {
        await page.request.delete(`/admin/lexicon-publishing/rules/${ruleId}`);
      }
      for (const id of [NOTE, DRAFT, ASIDE]) {
        await page.request.delete(`/admin/lexicons/${id}`);
      }
      if (grantId) await page.request.delete(`/admin/linked-repos/${grantId}`);
      await page.close();
    }
  });

  test("the account links through a real authorization", async ({ page }) => {
    await page.goto(`/auth/linked-repo/start?token=${inviteToken}`);
    await expect(page).toHaveURL(/\/link\/start\/?\?token=/);
    await expect(page.getByText(SCHEMA).first()).toBeVisible({
      timeout: 15000,
    });

    // A DID rather than the handle: `.test` names do not resolve inside the
    // Docker network.
    await page.getByPlaceholder("you.bsky.social").fill(account.did);
    await page.getByRole("button", { name: /continue/i }).click();

    await page.waitForURL(/pds\.localhost/, { timeout: 60000 });
    await submitPdsLogin(page, account.handle, PDS_PASSWORD);

    const outcome = await awaitConsentOrCallback(page);
    expect(
      outcome === "consent" || outcome === "callback",
      `expected the PDS consent screen or a redirect back, got ${outcome} at ${page.url()}`,
    ).toBeTruthy();
    if (outcome === "consent") {
      await page.getByRole("button", { name: /^authorize$/i }).click();
    }
    await page.waitForURL(/\/link\/result/, { timeout: 60000 });
    expect(new URL(page.url()).searchParams.get("status")).toBe("success");
  });

  test("a default sends the lexicon to the linked repo", async ({ page }) => {
    await loginAsTestAdmin(page);
    const resp = await page.request.post("/admin/lexicon-publishing/rules", {
      data: {
        pattern: PATTERN,
        target_kind: "linked_repo",
        linked_repo_id: grantId,
        auto_publish: false,
      },
    });
    expect(resp.status(), await resp.text()).toBe(201);
    const rule = await resp.json();
    ruleId = rule.id;
    expect(rule.target.available).toBe(true);
    expect(rule.target.can_unpublish).toBe(true);
    expect(rule.target.did).toBe(account.did);

    await openPublishing(page);
    await expect(lexiconRow(page, NOTE)).toContainText("Not published");
    await expect(lexiconRow(page, NOTE)).toContainText(`via ${PATTERN}`);
    await expect(lexiconRow(page, ASIDE)).toContainText("No default");
  });

  test("Publish writes the schema record to the PDS", async ({ page }) => {
    await loginAsTestAdmin(page);
    await openPublishing(page);
    const row = lexiconRow(page, NOTE);
    await row.getByRole("button", { name: "Publish", exact: true }).click();
    await expect(row).toContainText("Published", { timeout: 30000 });
    await expect(row.getByRole("button", { name: `Unpublish ${NOTE}` })).toBeVisible();

    const record = await getSchemaRecord(account.did, NOTE);
    expect(record.status, await record.clone().text()).toBe(200);
    const body = await record.json();
    expect(body.uri).toBe(`at://${account.did}/${SCHEMA}/${NOTE}`);
    expect(body.value.$type).toBe(SCHEMA);
    expect(body.value.id).toBe(NOTE);
    expect(body.value.lexicon).toBe(1);
    expect(body.value.defs).toEqual(lexicon(NOTE).defs);

    // The CID HappyView computed is the one the PDS stored.
    const entry = await overviewEntry(page, NOTE);
    expect(entry.status).toBe("current");
    expect(entry.publication.did).toBe(account.did);
    expect(entry.publication.cid).toBe(body.cid);

    await capture(page, "publishing-page-published-and-pending");
  });

  test("the DNS column names the record to add", async ({ page }) => {
    await loginAsTestAdmin(page);
    await openPublishing(page);
    const dns = lexiconRow(page, NOTE).getByRole("button", { name: "Not set" });
    await expect(dns).toBeVisible({ timeout: 20000 });
    await dns.click();

    const popover = page.getByRole("dialog");
    await expect(popover).toContainText("Add this DNS record");
    await expect(popover).toContainText("_lexicon.lexpub.e2e.test");
    await expect(popover).toContainText("TXT");
    await expect(popover).toContainText(`did=${account.did}`);
    await expect(popover).toContainText(
      "this one covers all 2 lexicons under lexpub.e2e.test",
    );
    // The DNS column is hidden on narrow screens, where the lexicon's own
    // Publishing panel carries the record instead.
    await capture(
      page,
      "publishing-dns-popover",
      async () => {
        await dns.click();
        await expect(popover).toBeVisible();
      },
      VIEWPORTS.filter((v) => v.name === "desktop"),
    );
  });

  test("an edit makes it outdated, and Republish updates the record", async ({
    page,
  }) => {
    await loginAsTestAdmin(page);
    const description = "A note, now described.";
    const edit = await page.request.post("/admin/lexicons", {
      data: { lexicon_json: lexicon(NOTE, description), backfill: false },
    });
    expect(edit.ok(), await edit.text()).toBeTruthy();

    await openPublishing(page);
    const row = lexiconRow(page, NOTE);
    await expect(row).toContainText("Outdated");
    await capture(page, "publishing-page-outdated");

    if (SCREENS) {
      await page.goto(`/dashboard/lexicons/${NOTE}`);
      await expect(page.locator(".monaco-editor .view-lines")).toContainText(
        NOTE,
      );
      await capture(page, "lexicon-detail-publishing-panel-outdated");
    }

    await openPublishing(page);
    await row.getByRole("button", { name: "Republish" }).click();
    await expect(row).toContainText("Published", { timeout: 30000 });

    const body = await (await getSchemaRecord(account.did, NOTE)).json();
    expect(body.value.description).toBe(description);
    expect(body.value.$type).toBe(SCHEMA);
    expect((await overviewEntry(page, NOTE)).publication.cid).toBe(body.cid);
  });

  test("Unpublish deletes the record and auto-publish leaves it withdrawn", async ({
    page,
  }) => {
    await loginAsTestAdmin(page);
    await openPublishing(page);
    const row = lexiconRow(page, NOTE);
    await row.getByRole("button", { name: `Unpublish ${NOTE}` }).click();
    const dialog = page.getByRole("alertdialog");
    await expect(dialog).toContainText(`Unpublish ${NOTE}?`);
    await expect(dialog).toContainText(account.did);
    await dialog.getByRole("button", { name: "Unpublish" }).click();
    await expect(row).toContainText("Withdrawn", { timeout: 30000 });

    const gone = await getSchemaRecord(account.did, NOTE);
    expect(gone.status).toBeGreaterThanOrEqual(400);
    expect(gone.status).toBeLessThan(500);

    const auto = page
      .getByRole("list", { name: "Publishing defaults, most specific first" })
      .getByRole("switch", { name: "Auto-publish" });
    await auto.click();
    await expect(auto).toBeChecked();

    // The reconcile the switch starts publishes DRAFT, which shows it ran,
    // and must skip NOTE.
    await expect
      .poll(async () => (await overviewEntry(page, DRAFT)).status, {
        timeout: 30000,
      })
      .toBe("current");
    expect((await getSchemaRecord(account.did, DRAFT)).status).toBe(200);
    await page.waitForTimeout(3000);
    const still = await getSchemaRecord(account.did, NOTE);
    expect(still.status).toBeGreaterThanOrEqual(400);
    const entry = await overviewEntry(page, NOTE);
    expect(entry.status).toBe("withdrawn");
    expect(entry.publication.withdrawn_at).toEqual(expect.any(String));

    await openPublishing(page);
    await expect(lexiconRow(page, NOTE)).toContainText("Withdrawn");
    await expect(lexiconRow(page, DRAFT)).toContainText("Published");
    await capture(page, "publishing-page-withdrawn-and-auto");
  });

  test("screens for design review", async ({ page }) => {
    test.skip(!SCREENS, "set LEXPUB_SCREENS_DIR to capture screens");
    await loginAsTestAdmin(page);

    await page.goto(PAGE);
    await page.getByRole("button", { name: "Add default" }).click();
    const sheet = page.getByRole("dialog");
    await sheet.getByLabel("Pattern").fill("test.e2e.*");
    await expect(sheet.getByText("Covers every lexicon under test.e2e")).toBeVisible();
    await capture(page, "add-default-dialog");
    await sheet.getByRole("button", { name: "Cancel" }).click();

    await page.goto("/dashboard/lexicons");
    await page.getByPlaceholder("Filter by ID...").fill("test.e2e.lexpub");
    await expect(page.getByText(NOTE)).toBeVisible();
    await capture(page, "lexicons-list-published-column");

    await page.goto(`/dashboard/lexicons/${DRAFT}`);
    await expect(
      page.locator('section[aria-labelledby="publishing-panel-title"]'),
    ).toContainText("Published");
    await expect(page.locator(".monaco-editor .view-lines")).toContainText(
      DRAFT,
    );
    await capture(page, "lexicon-detail-publishing-panel-published");

    await page.goto("/dashboard/settings/linked-repos");
    await page.getByRole("button", { name: "Link a repo" }).click();
    const link = page.getByRole("dialog");
    await link.getByRole("button", { name: "Lexicon publishing" }).click();
    await expect(link.getByText(SCOPES)).toBeVisible();
    await capture(page, "link-repo-dialog-publishing-preset");
  });
});
