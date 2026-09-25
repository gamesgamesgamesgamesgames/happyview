import { test, expect, type Page } from "@playwright/test"
import pg from "pg"
import { loginAsTestAdmin } from "./auth-helper"

const TEST_TYPE_NSID = "com.example.testspace"
const TEST_SKEY = "e2e-test-space"
const DB_URL = "postgres://happyview:happyview@localhost:5434/happyview_test"

async function enableSpacesFeature(): Promise<void> {
  const client = new pg.Client(DB_URL)
  await client.connect()
  try {
    const now = new Date().toISOString()
    await client.query(
      `INSERT INTO happyview_instance_settings (key, value, updated_at)
       VALUES ('feature.spaces_enabled', 'true', $1)
       ON CONFLICT (key) DO UPDATE SET value = 'true', updated_at = $1`,
      [now],
    )
  } finally {
    await client.end()
  }
}

const MEMBER_LIST_POLICY = {
  $type: "com.atproto.simplespace.defs#memberListPolicy",
}
const PUBLIC_POLICY = { $type: "com.atproto.simplespace.defs#publicPolicy" }

test.describe("Spaces API", () => {
  let createdSpaceUri: string | null = null

  test.beforeEach(async ({ page }) => {
    await enableSpacesFeature()
    await loginAsTestAdmin(page)
  })

  test.afterEach(async ({ page }) => {
    if (!createdSpaceUri) return
    await page.request.post("/xrpc/com.atproto.simplespace.deleteSpace", {
      data: { space: createdSpaceUri },
    })
    createdSpaceUri = null
  })

  async function createSpace(
    page: Page,
    data: Record<string, unknown>,
  ): Promise<string> {
    const resp = await page.request.post(
      "/xrpc/com.atproto.simplespace.createSpace",
      { data: { type: TEST_TYPE_NSID, ...data } },
    )
    if (!resp.ok()) {
      throw new Error(`createSpace failed (${resp.status()}): ${await resp.text()}`)
    }
    return (await resp.json()).uri
  }

  async function getSpace(page: Page, uri: string) {
    return page.request.get("/xrpc/com.atproto.simplespace.getSpace", {
      params: { space: uri },
    })
  }

  test("create space and verify it appears in listSpaces", async ({ page }) => {
    const uri = await createSpace(page, {
      skey: TEST_SKEY,
      displayName: "E2E Test Space",
      readPolicy: MEMBER_LIST_POLICY,
      writePolicy: MEMBER_LIST_POLICY,
    })
    expect(uri).toMatch(/^at:\/\/.+\/space\//)
    createdSpaceUri = uri

    const listResp = await page.request.get(
      "/xrpc/com.atproto.space.listSpaces",
    )
    expect(listResp.ok()).toBe(true)
    const listBody = await listResp.json()
    expect(listBody).toHaveProperty("spaces")

    const found = listBody.spaces.some(
      (s: { uri: string }) => s.uri === createdSpaceUri,
    )
    expect(found).toBe(true)
  })

  test("getSpace returns the created space", async ({ page }) => {
    const uri = await createSpace(page, {
      skey: TEST_SKEY + "-get",
      displayName: "GetSpace Test",
      readPolicy: MEMBER_LIST_POLICY,
    })
    createdSpaceUri = uri

    const getResp = await getSpace(page, uri)
    expect(getResp.ok()).toBe(true)
    const getBody = await getResp.json()
    expect(getBody.space.display_name).toBe("GetSpace Test")
    expect(getBody.config.readPolicy).toEqual(MEMBER_LIST_POLICY)
  })

  test("create duplicate space returns conflict", async ({ page }) => {
    createdSpaceUri = await createSpace(page, { skey: TEST_SKEY + "-dup" })

    const dupResp = await page.request.post(
      "/xrpc/com.atproto.simplespace.createSpace",
      {
        data: {
          type: TEST_TYPE_NSID,
          skey: TEST_SKEY + "-dup",
        },
      },
    )
    expect(dupResp.status()).toBe(409)
  })

  test("updateSpace changes display name", async ({ page }) => {
    const uri = await createSpace(page, {
      skey: TEST_SKEY + "-update",
      displayName: "Before Update",
    })
    createdSpaceUri = uri

    const updateResp = await page.request.post(
      "/xrpc/com.atproto.simplespace.updateSpace",
      { data: { space: uri, displayName: "After Update" } },
    )
    expect(updateResp.ok()).toBe(true)
    const updateBody = await updateResp.json()
    expect(updateBody.space.display_name).toBe("After Update")
  })

  test("deleteSpace removes the space", async ({ page }) => {
    const uri = await createSpace(page, { skey: TEST_SKEY + "-delete" })

    const deleteResp = await page.request.post(
      "/xrpc/com.atproto.simplespace.deleteSpace",
      { data: { space: uri } },
    )
    expect(deleteResp.ok()).toBe(true)

    const getResp = await getSpace(page, uri)
    expect(getResp.status()).toBe(404)
  })

  test("putMember returns 201", async ({ page }) => {
    const uri = await createSpace(page, { skey: TEST_SKEY + "-put-member" })
    createdSpaceUri = uri

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
    )
    expect(putResp.status()).toBe(201)
    const putBody = await putResp.json()
    expect(putBody.member.did).toBe("did:plc:test-member")
  })

  test("removeMember removes a previously added member", async ({ page }) => {
    const uri = await createSpace(page, { skey: TEST_SKEY + "-remove-member" })
    createdSpaceUri = uri

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
    )
    expect(putResp.ok()).toBe(true)

    const removeResp = await page.request.post(
      "/xrpc/com.atproto.simplespace.removeMember",
      { data: { space: uri, did: "did:plc:test-member-rm" } },
    )
    expect(removeResp.ok()).toBe(true)
  })

  test("listMembers includes added member", async ({ page }) => {
    const uri = await createSpace(page, { skey: TEST_SKEY + "-list-members" })
    createdSpaceUri = uri

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
    )
    expect(putResp.ok()).toBe(true)

    const listResp = await page.request.get(
      "/xrpc/com.atproto.simplespace.listMembers",
      { params: { space: uri } },
    )
    expect(listResp.ok()).toBe(true)
    const listBody = await listResp.json()
    expect(listBody.members).toBeInstanceOf(Array)
    const member = listBody.members.find(
      (m: { did: string }) => m.did === "did:plc:test-member-list",
    )
    expect(member).toMatchObject({ read: true, write: true })
  })

  test("getSpace returns the read and write policies", async ({ page }) => {
    const uri = await createSpace(page, {
      skey: TEST_SKEY + "-policies",
      readPolicy: PUBLIC_POLICY,
      writePolicy: MEMBER_LIST_POLICY,
    })
    createdSpaceUri = uri

    const getResp = await getSpace(page, uri)
    expect(getResp.ok()).toBe(true)
    const { config } = await getResp.json()
    expect(config.readPolicy).toEqual(PUBLIC_POLICY)
    expect(config.writePolicy).toEqual(MEMBER_LIST_POLICY)
  })

  test("updateSpace changes the read policy", async ({ page }) => {
    const uri = await createSpace(page, {
      skey: TEST_SKEY + "-update-policy",
      readPolicy: MEMBER_LIST_POLICY,
      writePolicy: MEMBER_LIST_POLICY,
    })
    createdSpaceUri = uri

    const updateResp = await page.request.post(
      "/xrpc/com.atproto.simplespace.updateSpace",
      { data: { space: uri, readPolicy: PUBLIC_POLICY } },
    )
    expect(updateResp.ok()).toBe(true)

    const getResp = await getSpace(page, uri)
    expect(getResp.ok()).toBe(true)
    const { config } = await getResp.json()
    expect(config.readPolicy).toEqual(PUBLIC_POLICY)
    expect(config.writePolicy).toEqual(MEMBER_LIST_POLICY)
  })
})

async function disableSpacesFeature(): Promise<void> {
  const client = new pg.Client(DB_URL)
  await client.connect()
  try {
    await client.query(
      `DELETE FROM happyview_instance_settings WHERE key = 'feature.spaces_enabled'`,
    )
  } finally {
    await client.end()
  }
}

test.describe("Spaces Feature Flag", () => {
  test.beforeEach(async ({ page }) => {
    await disableSpacesFeature()
    await loginAsTestAdmin(page)
  })

  test("spaces endpoints return 404 when feature is disabled", async ({
    page,
  }) => {
    const resp = await page.request.post(
      "/xrpc/com.atproto.simplespace.createSpace",
      {
        data: {
          type: "com.example.test",
          skey: "flag-test",
        },
      },
    )
    expect(resp.status()).toBe(404)
    const body = await resp.json()
    expect(body.error).toBe("FeatureDisabled")
  })
})
