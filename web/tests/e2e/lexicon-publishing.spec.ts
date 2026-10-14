import { test, expect, type APIRequestContext, type Page } from "@playwright/test";
import pg from "pg";
import {
  loginAsTestAdmin,
  loginAsUserWithPermissions,
  removeTestUser,
  restoreServiceIdentity,
  setServiceIdentityMode,
  snapshotServiceIdentity,
} from "./auth-helper";

/**
 * The publishing dashboard against the real API, with no account that can
 * actually receive a record: the instance is did:web, or attached to a DID
 * whose session is a stand-in no PDS would accept, and the only linked repo
 * is a grant nobody has authorized. A real publish is covered by
 * lexicon-publishing-oauth.spec.ts.
 */

const ALPHA = "test.e2e.pub.alpha";
const BETA = "test.e2e.pub.beta";
const GAMMA = "test.e2e.pub.gamma";
const OTHER = "test.e2e.pubother.thing";
const SEEDED = [ALPHA, BETA, GAMMA, OTHER];

const PREFIX = "test.e2e.pub.*";
const ATTACHED_DID = "did:plc:e2epublishinginstance";
const READER_DID = "did:plc:e2epublishingreader";
const STANDBY_DID = "did:plc:e2epublishingstandby";
const DID_WEB_REASON = "the instance identity is did:web and has no repo";
const PENDING_REASON = "this linked repo has not been authorized yet";
const NO_SESSION_REASON =
  "the attached account has no session; sign it in again from Service Identity";
const SCHEMA_SCOPE =
  "atproto repo:com.atproto.lexicon.schema?action=create&action=update&action=delete";
const DB_URL = "postgres://happyview:happyview@localhost:5434/happyview_test";

/**
 * A session row for the attached account carrying the schema scope, which is
 * all the instance target reads; its token reaches no PDS, and nothing here
 * publishes.
 */
async function seedAttachedSession(): Promise<void> {
  const client = new pg.Client(DB_URL);
  await client.connect();
  try {
    const now = new Date().toISOString();
    const sessionData = JSON.stringify({
      token_set: {
        access_token: "e2e-lexicon-publishing-unusable",
        scope: SCHEMA_SCOPE,
        expires_at: new Date(Date.now() + 3600_000).toISOString(),
      },
    });
    await client.query(
      `INSERT INTO happyview_oauth_sessions (did, session_data, created_at, updated_at)
       VALUES ($1, $2, $3, $3)
       ON CONFLICT (did) DO UPDATE SET session_data = $2, updated_at = $3`,
      [ATTACHED_DID, sessionData, now],
    );
  } finally {
    await client.end();
  }
}

async function removeAttachedSession(): Promise<void> {
  const client = new pg.Client(DB_URL);
  await client.connect();
  try {
    await client.query("DELETE FROM happyview_oauth_sessions WHERE did = $1", [
      ATTACHED_DID,
    ]);
  } finally {
    await client.end();
  }
}

/**
 * A second destination that stays able to publish, bound and given a granted
 * session by hand, so the page stays open while the instance is taken away.
 * Nothing here publishes, so its token reaches no PDS either.
 */
async function activateStandbyRepo(grantId: string): Promise<void> {
  const client = new pg.Client(DB_URL);
  await client.connect();
  try {
    const now = new Date().toISOString();
    await client.query(
      `UPDATE happyview_linked_repos
       SET did = $1, status = 'active', authorized_at = $2, last_refreshed_at = $2
       WHERE id = $3`,
      [STANDBY_DID, now, grantId],
    );
    const sessionData = JSON.stringify({
      dpop_key: {},
      token_set: {
        sub: STANDBY_DID,
        scope: SCHEMA_SCOPE,
        access_token: "e2e-lexicon-publishing-unusable",
        token_type: "DPoP",
      },
    });
    await client.query(
      `INSERT INTO happyview_linked_repo_sessions (did, session_data, updated_at)
       VALUES ($1, $2, $3)
       ON CONFLICT (did) DO UPDATE SET session_data = $2, updated_at = $3`,
      [STANDBY_DID, sessionData, now],
    );
  } finally {
    await client.end();
  }
}

async function removeStandbySession(): Promise<void> {
  const client = new pg.Client(DB_URL);
  await client.connect();
  try {
    await client.query(
      "DELETE FROM happyview_linked_repo_sessions WHERE did = $1",
      [STANDBY_DID],
    );
  } finally {
    await client.end();
  }
}

const PAGE = "/dashboard/settings/lexicon-publishing";

function sidebarLink(page: Page) {
  return page.getByRole("link", { name: "Lexicon Publishing" });
}

function lexicon(id: string) {
  return {
    lexicon: 1,
    id,
    defs: {
      main: {
        type: "record",
        key: "tid",
        record: { type: "object", properties: { text: { type: "string" } } },
      },
    },
  };
}

type Rule = { id: string; pattern: string };

async function rules(request: APIRequestContext): Promise<Rule[]> {
  const resp = await request.get("/admin/lexicon-publishing");
  expect(resp.ok()).toBeTruthy();
  return (await resp.json()).rules;
}

async function createRule(
  request: APIRequestContext,
  body: Record<string, unknown>,
) {
  return request.post("/admin/lexicon-publishing/rules", { data: body });
}

function cascade(page: Page) {
  return page.getByRole("list", {
    name: "Publishing defaults, most specific first",
  });
}

async function filterTo(page: Page, text: string) {
  await page.getByPlaceholder("Filter by NSID...").fill(text);
}

function lexiconRow(page: Page, nsid: string) {
  return page.locator("table tbody tr", {
    has: page.getByText(nsid, { exact: true }),
  });
}

async function openAddDefault(page: Page) {
  await page.getByRole("button", { name: "Add default" }).click();
  const sheet = page.getByRole("dialog");
  await expect(sheet.getByText("Add a default")).toBeVisible();
  return sheet;
}

test.describe("Lexicon publishing dashboard", () => {
  test.describe.configure({ mode: "serial" });

  let identity: Awaited<ReturnType<typeof snapshotServiceIdentity>>;
  let grantId: string;
  let standbyId: string;

  test.beforeAll(async ({ browser }) => {
    identity = await snapshotServiceIdentity();
    await setServiceIdentityMode("did_web", { did: "did:web:localhost" });

    const page = await browser.newPage();
    await loginAsTestAdmin(page);
    for (const id of SEEDED) {
      const resp = await page.request.post("/admin/lexicons", {
        data: { lexicon_json: lexicon(id), backfill: false },
      });
      expect(resp.ok(), `seed ${id}: ${await resp.text()}`).toBeTruthy();
    }
    const grant = await page.request.post("/admin/linked-repos", {
      data: {
        reason: "e2e lexicon publishing",
        scopes:
          "atproto repo:com.atproto.lexicon.schema?action=create&action=update&action=delete",
      },
    });
    expect(grant.ok(), `create grant: ${await grant.text()}`).toBeTruthy();
    grantId = (await grant.json()).id;
    const standby = await page.request.post("/admin/linked-repos", {
      data: { reason: "e2e lexicon publishing standby", scopes: SCHEMA_SCOPE },
    });
    expect(standby.ok(), `create standby: ${await standby.text()}`).toBeTruthy();
    standbyId = (await standby.json()).id;
    await page.close();
  });

  test.afterAll(async ({ browser }) => {
    // Later projects depend on the identity, so it goes back before any
    // other cleanup can fail and skip it. Undefined means the snapshot was
    // never taken, and restoring that would delete the identity outright.
    try {
      if (identity !== undefined) await restoreServiceIdentity(identity);
    } finally {
      await removeAttachedSession();
      await removeStandbySession();
      const page = await browser.newPage();
      await loginAsTestAdmin(page);
      for (const rule of await rules(page.request)) {
        if (rule.pattern === "*" || rule.pattern.startsWith("test.e2e.pub")) {
          await page.request.delete(
            `/admin/lexicon-publishing/rules/${rule.id}`,
          );
        }
      }
      for (const id of SEEDED) {
        await page.request.delete(`/admin/lexicons/${id}`);
      }
      if (grantId) await page.request.delete(`/admin/linked-repos/${grantId}`);
      if (standbyId) {
        await page.request.delete(`/admin/linked-repos/${standbyId}`);
      }
      await page.close();
      await removeTestUser(READER_DID);
    }
  });

  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page);
  });

  test("with nowhere to publish, publishing stays closed and the API refuses", async ({
    page,
  }) => {
    await page.goto("/dashboard/lexicons");
    await expect(
      page.getByRole("link", { name: "Lexicons", exact: true }),
    ).toBeVisible();
    await expect(sidebarLink(page)).toHaveCount(0);
    await expect(
      page.getByRole("columnheader", { name: "Published" }),
    ).toHaveCount(0);

    // The lexicon page says what to set up, and still checks DNS.
    await page.goto(`/dashboard/lexicons/${ALPHA}`);
    const panel = page.locator('section[aria-labelledby="publishing-panel-title"]');
    await expect(panel).toContainText("Nothing can receive lexicons yet");
    await expect(panel.getByRole("link", { name: "Service Identity" })).toBeVisible();
    await expect(panel.getByRole("link", { name: "Linked Repos" })).toBeVisible();
    await expect(panel).toContainText("_lexicon.pub.e2e.test");
    await expect(panel).toContainText("Not set yet");

    await page.goto(PAGE);
    await expect(page).toHaveURL(/\/dashboard\/?$/);
    await expect(
      page.getByText("Lexicon publishing needs an account to publish to"),
    ).toBeVisible();

    const instance = await createRule(page.request, {
      pattern: PREFIX,
      target_kind: "instance",
    });
    expect(instance.status()).toBe(400);
    expect((await instance.json()).error).toBe(DID_WEB_REASON);

    const pending = await createRule(page.request, {
      pattern: PREFIX,
      target_kind: "linked_repo",
      linked_repo_id: grantId,
    });
    expect(pending.status()).toBe(400);
    expect((await pending.json()).error).toBe(PENDING_REASON);
    expect(await rules(page.request)).toEqual([]);
  });

  test("an attached account without a session cannot receive lexicons", async ({
    page,
  }) => {
    await setServiceIdentityMode("attach_account", {
      attachedAccountDid: ATTACHED_DID,
    });
    await removeAttachedSession();

    const overview = await (
      await page.request.get("/admin/lexicon-publishing")
    ).json();
    const target = overview.targets.find(
      (t: { kind: string }) => t.kind === "instance",
    );
    expect(target.available).toBe(false);
    expect(target.unavailable_reason).toBe(NO_SESSION_REASON);

    await page.goto(PAGE);
    await expect(page).toHaveURL(/\/dashboard\/?$/);

    const refused = await createRule(page.request, {
      pattern: PREFIX,
      target_kind: "instance",
    });
    expect(refused.status()).toBe(400);
    expect((await refused.json()).error).toBe(NO_SESSION_REASON);
    expect(await rules(page.request)).toEqual([]);
  });

  test("once an account can receive lexicons, publishing opens from System", async ({
    page,
  }) => {
    await seedAttachedSession();
    expect(await rules(page.request)).toEqual([]);

    await page.goto("/dashboard/lexicons");
    await expect(
      page.getByRole("columnheader", { name: "Published" }),
    ).toBeVisible();
    await sidebarLink(page).click();
    await expect(page).toHaveURL(new RegExp(`${PAGE}/?$`));

    const list = cascade(page);
    await expect(list.getByRole("listitem")).toHaveCount(1);
    await expect(list.getByRole("listitem")).toContainText("Every lexicon");
    await expect(list.getByRole("listitem")).toContainText("Not published");

    await filterTo(page, "test.e2e.pub");
    for (const nsid of SEEDED) {
      await expect(lexiconRow(page, nsid)).toContainText("No default");
    }
  });

  test("the add dialog offers an unauthorized linked repo only as disabled", async ({
    page,
  }) => {
    await seedAttachedSession();
    await page.goto(PAGE);
    await expect(
      page.getByText("Nothing can receive lexicons yet", { exact: false }),
    ).toHaveCount(0);
    await expect(
      page.getByText("Nothing publishes until a default names an account."),
    ).toBeVisible();

    const sheet = await openAddDefault(page);
    const instance = sheet.locator("label", { hasText: "Instance account" });
    await expect(instance).toContainText(ATTACHED_DID);
    await expect(instance.getByRole("radio")).toBeEnabled();
    await expect(instance.getByRole("radio")).toBeChecked();

    const grant = sheet.locator(
      `label[for="publish-dest-linked_repo:${grantId}"]`,
    );
    await expect(grant).toContainText(`Unavailable: ${PENDING_REASON}`);
    await expect(grant.getByRole("radio")).toBeDisabled();

    await sheet.getByLabel("Pattern").fill(PREFIX);
    await expect(
      sheet.getByText(`Covers every lexicon under test.e2e.pub`),
    ).toBeVisible();
    await expect(sheet.getByText("Matches 3 lexicons here.")).toBeVisible();

    // Nothing here can actually write, so a reconcile must have nothing to do.
    const auto = sheet.getByRole("switch", { name: "Auto-publish" });
    await expect(auto).toBeChecked();
    await auto.click();
    await expect(
      sheet.getByText(
        "Matching lexicons publish only when someone presses Publish.",
      ),
    ).toBeVisible();

    await sheet.getByRole("button", { name: "Add default" }).click();
    await expect(page.getByText("Default added")).toBeVisible();
    await expect(sheet).toBeHidden();

    const items = cascade(page).getByRole("listitem");
    await expect(items).toHaveCount(2);
    await expect(items.nth(0)).toContainText(PREFIX);
    await expect(items.nth(0)).toContainText(
      "Covers every lexicon under test.e2e.pub",
    );
    await expect(items.nth(0)).toContainText("Instance account");
    await expect(items.nth(0)).toContainText(ATTACHED_DID);
    await expect(
      items.nth(0).getByRole("switch", { name: "Auto-publish" }),
    ).not.toBeChecked();
    await expect(items.nth(1)).toContainText("Everything else");

    await filterTo(page, "test.e2e.pub");
    for (const nsid of [ALPHA, BETA, GAMMA]) {
      await expect(lexiconRow(page, nsid)).toContainText("Not published");
    }
    await expect(lexiconRow(page, OTHER)).toContainText("No default");

    // Losing the session strands the default rather than removing it, and
    // the edit dialog says why the instance cannot be chosen. The standby
    // repo keeps the page open meanwhile.
    await activateStandbyRepo(standbyId);
    await removeAttachedSession();
    await page.reload();
    const stranded = cascade(page).getByRole("listitem").nth(0);
    await expect(stranded).toContainText("Destination unavailable");
    await expect(stranded).toContainText(
      "The attached account has no session; sign it in again from Service Identity",
    );
    await stranded
      .getByRole("button", { name: `Edit the default for ${PREFIX}` })
      .click();
    const edit = page.getByRole("dialog");
    await expect(edit.getByText("Edit default")).toBeVisible();
    const disabled = edit.locator("label", { hasText: "Instance account" });
    await expect(disabled).toContainText(`Unavailable: ${NO_SESSION_REASON}`);
    await expect(disabled.getByRole("radio")).toBeDisabled();
    await edit.getByRole("button", { name: "Cancel" }).click();
    await seedAttachedSession();
  });

  test("defaults are created, refused and ordered through the API", async ({
    page,
  }) => {
    const request = page.request;
    const prefix = (await rules(request)).find((r) => r.pattern === PREFIX);
    expect(prefix, "the dialog's default should exist").toBeTruthy();

    const catchAll = await createRule(request, {
      pattern: "*",
      target_kind: "instance",
      auto_publish: false,
    });
    expect(catchAll.status(), await catchAll.text()).toBe(201);
    const catchAllId = (await catchAll.json()).id;

    const exact = await createRule(request, {
      pattern: ` ${ALPHA} `,
      target_kind: "instance",
    });
    expect(exact.status(), await exact.text()).toBe(201);
    const exactRule = await exact.json();
    expect(exactRule.pattern).toBe(ALPHA);
    expect(exactRule.auto_publish).toBe(false);
    expect(exactRule.target.available).toBe(true);
    expect(exactRule.target.did).toBe(ATTACHED_DID);

    const duplicate = await createRule(request, {
      pattern: "TEST.E2E.PUB.*",
      target_kind: "instance",
    });
    expect(duplicate.status()).toBe(409);
    expect((await duplicate.json()).error).toBe(
      `a default for ${PREFIX} already exists`,
    );

    for (const bad of ["com.*.foo", "**", "com", ""]) {
      const resp = await createRule(request, {
        pattern: bad,
        target_kind: "instance",
      });
      expect(resp.status(), `pattern ${JSON.stringify(bad)}`).toBe(400);
    }

    const toPending = await request.patch(
      `/admin/lexicon-publishing/rules/${exactRule.id}`,
      { data: { target_kind: "linked_repo", linked_repo_id: grantId } },
    );
    expect(toPending.status()).toBe(400);
    expect((await toPending.json()).error).toBe(PENDING_REASON);

    const unchanged = await request.patch(
      `/admin/lexicon-publishing/rules/${exactRule.id}`,
      { data: { auto_publish: false } },
    );
    expect(unchanged.status()).toBe(200);
    expect((await unchanged.json()).target_kind).toBe("instance");

    // Most specific first: exact, then prefix, then the catch-all.
    expect((await rules(request)).map((r) => r.pattern)).toEqual([
      ALPHA,
      PREFIX,
      "*",
    ]);

    await page.goto(PAGE);
    const items = cascade(page).getByRole("listitem");
    // A catch-all leaves nothing for the closing "Everything else" row.
    await expect(items).toHaveCount(3);
    await expect(items.nth(0)).toContainText(`Covers only ${ALPHA}`);
    await expect(items.nth(1)).toContainText(PREFIX);
    await expect(items.nth(2)).toContainText("Covers every lexicon");
    await expect(cascade(page)).not.toContainText("Everything else");

    await filterTo(page, "test.e2e.pub");
    await expect(lexiconRow(page, ALPHA)).toContainText(`via ${ALPHA}`);
    await expect(lexiconRow(page, BETA)).toContainText(`via ${PREFIX}`);
    await expect(lexiconRow(page, OTHER)).toContainText("via *");

    const removed = await request.delete(
      `/admin/lexicon-publishing/rules/${catchAllId}`,
    );
    expect(removed.status()).toBe(204);
    const again = await request.delete(
      `/admin/lexicon-publishing/rules/${catchAllId}`,
    );
    expect(again.status()).toBe(404);
    expect((await rules(request)).map((r) => r.pattern)).toEqual([
      ALPHA,
      PREFIX,
    ]);
  });

  test("the add dialog reads a pattern and counts what it would cover", async ({
    page,
  }) => {
    const overview = await (
      await page.request.get("/admin/lexicon-publishing")
    ).json();
    const total: number = overview.lexicons.length;
    expect(total).toBeGreaterThanOrEqual(SEEDED.length);

    await page.goto(PAGE);
    const sheet = await openAddDefault(page);
    const pattern = sheet.getByLabel("Pattern");
    const help = sheet.locator("#publish-rule-pattern-help");

    // ALPHA's exact default and PREFIX's together claim the three under
    // test.e2e.pub, all more specific than *.
    await pattern.fill("*");
    await expect(help).toContainText("Covers every lexicon");
    await expect(help).toContainText(
      `Matches ${total} lexicons here; a more specific default keeps 3 of them.`,
    );

    await pattern.fill(PREFIX);
    await expect(help).toContainText("Covers every lexicon under test.e2e.pub");
    await expect(help).toContainText(
      "Matches 3 lexicons here; a more specific default keeps 1 of them.",
    );

    await pattern.fill("TEST.E2E.PUBOTHER.*");
    await expect(help).toContainText(
      "Covers every lexicon under test.e2e.pubother",
    );
    await expect(help).toContainText("Matches 1 lexicon here.");

    await pattern.fill(BETA);
    await expect(help).toContainText(`Covers only ${BETA}`);
    await expect(help).toContainText("Matches 1 lexicon here.");

    await pattern.fill("test.e2e.nothing.*");
    await expect(help).toContainText("No lexicon here matches it yet.");

    await pattern.fill("com.*.foo");
    await pattern.blur();
    await expect(help).toContainText(
      "* works alone, or as a trailing .* after a prefix",
    );
    await expect(sheet.getByRole("button", { name: "Add default" })).toBeDisabled();

    await sheet.getByRole("button", { name: "Cancel" }).click();
    await expect(sheet).toBeHidden();
    expect((await rules(page.request)).map((r) => r.pattern)).toEqual([
      ALPHA,
      PREFIX,
    ]);
  });

  test("the lexicon page shows the default that governs it", async ({
    page,
  }) => {
    await page.goto(`/dashboard/lexicons/${BETA}`);
    const panel = page.locator('section[aria-labelledby="publishing-panel-title"]');
    await expect(panel).toContainText("Not published");
    await expect(panel).toContainText("Instance account");
    await expect(panel.getByRole("link", { name: PREFIX })).toHaveAttribute(
      "href",
      new RegExp(PAGE),
    );
    await expect(panel.getByRole("button", { name: "Publish" })).toBeVisible();

    // HappyView knows where BETA goes, so it gives the whole record to add.
    await expect(panel).toContainText("_lexicon.pub.e2e.test");
    await expect(panel).toContainText("Not set yet");
    await expect(panel).toContainText(`did=${ATTACHED_DID}`);
    await expect(panel.getByRole("button", { name: "Check again" })).toBeVisible();

    await page.goto(`/dashboard/lexicons/${OTHER}`);
    await expect(panel).toContainText("No default");
    await expect(panel).toContainText("No default covers it.");
    await expect(panel.getByRole("link", { name: "Add one" })).toBeVisible();
    await expect(panel.getByRole("button", { name: /publish/i })).toHaveCount(0);
    // Nothing decides OTHER's account, so only the record's name is known.
    await expect(panel).toContainText("_lexicon.pubother.e2e.test");
    await expect(panel).toContainText("Not set yet");
    await expect(panel).not.toContainText("did=did:");
  });

  test("a reader sees publishing but none of its controls", async ({
    page,
  }) => {
    await page.context().clearCookies();
    await loginAsUserWithPermissions(page, READER_DID, ["lexicons:read"]);

    const write = await createRule(page.request, {
      pattern: "test.e2e.pub.gamma",
      target_kind: "instance",
    });
    expect(write.status()).toBe(403);

    await page.goto(`/dashboard/lexicons/${ALPHA}`);
    const panel = page.locator('section[aria-labelledby="publishing-panel-title"]');
    await expect(panel).toContainText("Not published");
    await expect(panel.getByRole("link", { name: ALPHA })).toBeVisible();
    await expect(panel.getByRole("button", { name: /publish/i })).toHaveCount(0);

    await page.goto(PAGE);
    const items = cascade(page).getByRole("listitem");
    await expect(items.nth(0)).toContainText("Auto-publish off");
    await expect(cascade(page).getByRole("switch")).toHaveCount(0);
    await expect(cascade(page).getByRole("button")).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Add default" })).toHaveCount(
      0,
    );
    await filterTo(page, "test.e2e.pub");
    await expect(lexiconRow(page, ALPHA)).toContainText("Not published");
    await expect(
      page.getByRole("button", { name: /publish/i }),
    ).toHaveCount(0);
  });

  test("a did:web instance leaves its defaults unavailable, with the reason", async ({
    page,
  }) => {
    await setServiceIdentityMode("did_web", { did: "did:web:localhost" });
    await page.goto(PAGE);

    const first = cascade(page).getByRole("listitem").nth(0);
    await expect(first).toContainText("Destination unavailable");
    await expect(first).toContainText(
      "The instance identity is did:web and has no repo",
    );

    await filterTo(page, "test.e2e.pub");
    await expect(lexiconRow(page, ALPHA)).toContainText(
      "Destination unavailable",
    );

    await first
      .getByRole("button", { name: `Edit the default for ${ALPHA}` })
      .click();
    const sheet = page.getByRole("dialog");
    await expect(sheet.getByText("Edit default")).toBeVisible();
    await expect(sheet).toContainText(
      `Its current destination is unavailable: ${DID_WEB_REASON}.`,
    );
    const instance = sheet.locator("label", { hasText: "Instance account" });
    await expect(instance).toContainText(`Unavailable: ${DID_WEB_REASON}`);
    await expect(instance.getByRole("radio")).toBeDisabled();
    await sheet.getByRole("button", { name: "Cancel" }).click();

    // The pencil's sibling deletes; the page goes back to the empty cascade.
    for (const pattern of [ALPHA, PREFIX]) {
      await cascade(page)
        .getByRole("button", { name: `Delete the default for ${pattern}` })
        .click();
      await page
        .getByRole("alertdialog")
        .getByRole("button", { name: "Delete" })
        .click();
      await expect(cascade(page)).not.toContainText(pattern);
    }
    await expect(cascade(page).getByRole("listitem")).toHaveCount(1);
    expect(await rules(page.request)).toEqual([]);
  });
});
