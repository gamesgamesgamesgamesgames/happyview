import { test, expect, type Page } from "@playwright/test";
import pg from "pg";
import { loginAsTestAdmin, DB_URL } from "./auth-helper";

const TEST_TYPE_NSID = "com.example.testspace";
const TEST_SKEY = "e2e-test-space";

async function enableSpacesFeature(): Promise<void> {
  const client = new pg.Client(DB_URL);
  await client.connect();
  try {
    const now = new Date().toISOString();
    await client.query(
      `INSERT INTO happyview_instance_settings (key, value, updated_at)
       VALUES ('feature.spaces_enabled', 'true', $1)
       ON CONFLICT (key) DO UPDATE SET value = 'true', updated_at = $1`,
      [now],
    );
  } finally {
    await client.end();
  }
}

const MEMBERSPACE = "com.atproto.memberspace.policy";
const MEMBER_POLICY = { $type: MEMBERSPACE };

test.describe("Spaces API", () => {
  // `page.request` runs outside the browser and sends no fetch metadata, and
  // the session cookie counts on XRPC only on a request from the instance's
  // own pages. This spec drives the API only, as the dashboard would.
  test.use({ extraHTTPHeaders: { "sec-fetch-site": "same-origin" } });

  let createdSpaceUri: string | null = null;

  test.beforeEach(async ({ page }) => {
    await enableSpacesFeature();
    await loginAsTestAdmin(page);
  });

  test.afterEach(async ({ page }) => {
    if (!createdSpaceUri) return;
    await page.request.post("/xrpc/com.atproto.space.deleteSpace", {
      data: { space: createdSpaceUri },
    });
    createdSpaceUri = null;
  });

  async function createSpace(
    page: Page,
    data: Record<string, unknown>,
  ): Promise<string> {
    const resp = await page.request.post(
      "/xrpc/com.atproto.space.createSpace",
      { data: { spaceType: TEST_TYPE_NSID, policy: MEMBER_POLICY, ...data } },
    );
    if (!resp.ok()) {
      throw new Error(
        `createSpace failed (${resp.status()}): ${await resp.text()}`,
      );
    }
    return (await resp.json()).uri;
  }

  async function getSpace(page: Page, uri: string) {
    return page.request.get("/xrpc/com.atproto.space.getSpace", {
      params: { space: uri },
    });
  }

  test("create space and verify it appears in listSpaces", async ({ page }) => {
    const uri = await createSpace(page, {
      skey: TEST_SKEY,
    });
    expect(uri).toMatch(/^at:\/\/.+\/space\//);
    createdSpaceUri = uri;

    const listResp = await page.request.get(
      "/xrpc/com.atproto.space.listSpaces",
    );
    expect(listResp.ok()).toBe(true);
    const listBody = await listResp.json();
    expect(listBody).toHaveProperty("spaces");

    const found = listBody.spaces.some(
      (s: { uri: string }) => s.uri === createdSpaceUri,
    );
    expect(found).toBe(true);
  });

  test("getSpace returns the created space and its policy", async ({
    page,
  }) => {
    const uri = await createSpace(page, { skey: TEST_SKEY + "-get" });
    createdSpaceUri = uri;

    const getResp = await getSpace(page, uri);
    expect(getResp.ok()).toBe(true);
    const getBody = await getResp.json();
    expect(getBody.uri).toBe(uri);
    expect(getBody.policy).toMatchObject({
      $type: MEMBERSPACE,
      publicRead: false,
    });
  });

  test("create duplicate space returns conflict", async ({ page }) => {
    createdSpaceUri = await createSpace(page, { skey: TEST_SKEY + "-dup" });

    const dupResp = await page.request.post(
      "/xrpc/com.atproto.space.createSpace",
      {
        data: {
          spaceType: TEST_TYPE_NSID,
          skey: TEST_SKEY + "-dup",
          policy: MEMBER_POLICY,
        },
      },
    );
    expect(dupResp.status()).toBe(409);
    expect((await dupResp.json()).error).toBe("SpaceAlreadyExists");
  });

  test("updateSpace replaces the policy", async ({ page }) => {
    const uri = await createSpace(page, {
      skey: TEST_SKEY + "-update",
      policy: { $type: MEMBERSPACE, publicRead: true },
    });
    createdSpaceUri = uri;

    const updateResp = await page.request.post(
      "/xrpc/com.atproto.space.updateSpace",
      {
        data: { space: uri, policy: { $type: MEMBERSPACE, publicWrite: true } },
      },
    );
    expect(updateResp.ok()).toBe(true);

    const { policy } = await (await getSpace(page, uri)).json();
    expect(policy).toMatchObject({ publicRead: false, publicWrite: true });
  });

  test("deleteSpace removes the space", async ({ page }) => {
    const uri = await createSpace(page, { skey: TEST_SKEY + "-delete" });

    const deleteResp = await page.request.post(
      "/xrpc/com.atproto.space.deleteSpace",
      { data: { space: uri } },
    );
    expect(deleteResp.ok()).toBe(true);

    const getResp = await getSpace(page, uri);
    expect(getResp.status()).toBe(400);
    expect((await getResp.json()).error).toBe("SpaceNotFound");
  });

  test("putMember returns 201", async ({ page }) => {
    const uri = await createSpace(page, { skey: TEST_SKEY + "-put-member" });
    createdSpaceUri = uri;

    const putResp = await page.request.post(
      "/xrpc/com.atproto.simplespace.putMember",
      {
        data: {
          space: uri,
          did: "did:plc:test-member",
          read: true,
          write: false,
        },
      },
    );
    expect(putResp.status()).toBe(201);
    const putBody = await putResp.json();
    expect(putBody.member.did).toBe("did:plc:test-member");
  });

  test("removeMember removes a previously added member", async ({ page }) => {
    const uri = await createSpace(page, { skey: TEST_SKEY + "-remove-member" });
    createdSpaceUri = uri;

    const putResp = await page.request.post(
      "/xrpc/com.atproto.simplespace.putMember",
      {
        data: {
          space: uri,
          did: "did:plc:test-member-rm",
          read: true,
          write: false,
        },
      },
    );
    expect(putResp.ok()).toBe(true);

    const removeResp = await page.request.post(
      "/xrpc/com.atproto.simplespace.removeMember",
      { data: { space: uri, did: "did:plc:test-member-rm" } },
    );
    expect(removeResp.ok()).toBe(true);
  });

  test("listMembers includes added member", async ({ page }) => {
    const uri = await createSpace(page, { skey: TEST_SKEY + "-list-members" });
    createdSpaceUri = uri;

    const putResp = await page.request.post(
      "/xrpc/com.atproto.simplespace.putMember",
      {
        data: {
          space: uri,
          did: "did:plc:test-member-list",
          read: true,
          write: true,
        },
      },
    );
    expect(putResp.ok()).toBe(true);

    const listResp = await page.request.get(
      "/xrpc/com.atproto.simplespace.listMembers",
      { params: { space: uri } },
    );
    expect(listResp.ok()).toBe(true);
    const listBody = await listResp.json();
    expect(listBody.members).toBeInstanceOf(Array);
    const member = listBody.members.find(
      (m: { did: string }) => m.did === "did:plc:test-member-list",
    );
    expect(member).toMatchObject({ read: true, write: true });
  });

  test("a managed space round-trips its managing app", async ({ page }) => {
    const policy = {
      $type: "com.atproto.managedspace.policy",
      managingApp: "did:web:app.example#forum",
      publishPolicy: false,
    };
    const uri = await createSpace(page, {
      skey: TEST_SKEY + "-managed",
      policy,
    });
    createdSpaceUri = uri;

    const getResp = await getSpace(page, uri);
    expect(getResp.ok()).toBe(true);
    expect((await getResp.json()).policy).toEqual(policy);
  });
});

async function disableSpacesFeature(): Promise<void> {
  const client = new pg.Client(DB_URL);
  await client.connect();
  try {
    await client.query(
      `DELETE FROM happyview_instance_settings WHERE key = 'feature.spaces_enabled'`,
    );
  } finally {
    await client.end();
  }
}

test.describe("Spaces Feature Flag", () => {
  // As in "Spaces API": the API request is the dashboard's own.
  test.use({ extraHTTPHeaders: { "sec-fetch-site": "same-origin" } });

  test.beforeEach(async ({ page }) => {
    await disableSpacesFeature();
    await loginAsTestAdmin(page);
  });

  test("spaces endpoints return 404 when feature is disabled", async ({
    page,
  }) => {
    const resp = await page.request.post(
      "/xrpc/com.atproto.space.createSpace",
      {
        data: {
          spaceType: "com.example.test",
          skey: "flag-test",
          policy: MEMBER_POLICY,
        },
      },
    );
    expect(resp.status()).toBe(404);
    const body = await resp.json();
    expect(body.error).toBe("FeatureDisabled");
  });
});
