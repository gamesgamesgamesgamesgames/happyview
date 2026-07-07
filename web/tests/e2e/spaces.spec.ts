import { test, expect } from "@playwright/test"
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

  test("create space and verify it appears in listSpaces", async ({ page }) => {
    const createResp = await page.request.post(
      "/xrpc/com.atproto.simplespace.createSpace",
      {
        data: {
          type: TEST_TYPE_NSID,
          skey: TEST_SKEY,
          displayName: "E2E Test Space",
          mintPolicy: "member-list",
        },
      },
    )

    if (!createResp.ok()) {
      const errBody = await createResp.text()
      throw new Error(`createSpace failed (${createResp.status()}): ${errBody}`)
    }
    const createBody = await createResp.json()
    expect(createBody).toHaveProperty("uri")
    expect(createBody.uri).toMatch(/^at:\/\/.+\/space\//)
    createdSpaceUri = createBody.uri

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
    const createResp = await page.request.post(
      "/xrpc/com.atproto.simplespace.createSpace",
      {
        data: {
          type: TEST_TYPE_NSID,
          skey: TEST_SKEY + "-get",
          displayName: "GetSpace Test",
        },
      },
    )
    expect(createResp.ok()).toBe(true)
    const { uri } = await createResp.json()
    createdSpaceUri = uri

    const getResp = await page.request.get("/xrpc/com.atproto.space.getSpace", {
      params: { space: uri },
    })
    expect(getResp.ok()).toBe(true)
    const getBody = await getResp.json()
    expect(getBody.space.display_name).toBe("GetSpace Test")
    expect(getBody.space.mint_policy).toBe("member-list")
  })

  test("create duplicate space returns conflict", async ({ page }) => {
    const createResp = await page.request.post(
      "/xrpc/com.atproto.simplespace.createSpace",
      {
        data: {
          type: TEST_TYPE_NSID,
          skey: TEST_SKEY + "-dup",
        },
      },
    )
    expect(createResp.ok()).toBe(true)
    createdSpaceUri = (await createResp.json()).uri

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
    const createResp = await page.request.post(
      "/xrpc/com.atproto.simplespace.createSpace",
      {
        data: {
          type: TEST_TYPE_NSID,
          skey: TEST_SKEY + "-update",
          displayName: "Before Update",
        },
      },
    )
    if (!createResp.ok()) {
      throw new Error(
        `createSpace failed (${createResp.status()}): ${await createResp.text()}`,
      )
    }
    const { uri } = await createResp.json()
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
    const createResp = await page.request.post(
      "/xrpc/com.atproto.simplespace.createSpace",
      {
        data: {
          type: TEST_TYPE_NSID,
          skey: TEST_SKEY + "-delete",
        },
      },
    )
    if (!createResp.ok()) {
      throw new Error(
        `createSpace failed (${createResp.status()}): ${await createResp.text()}`,
      )
    }
    const { uri } = await createResp.json()

    const deleteResp = await page.request.post(
      "/xrpc/com.atproto.simplespace.deleteSpace",
      { data: { space: uri } },
    )
    expect(deleteResp.ok()).toBe(true)

    const getResp = await page.request.get(
      "/xrpc/com.atproto.space.getSpace",
      { params: { space: uri } },
    )
    expect(getResp.status()).toBe(404)
  })

  test("addMember returns 201", async ({ page }) => {
    const createResp = await page.request.post(
      "/xrpc/com.atproto.simplespace.createSpace",
      {
        data: {
          type: TEST_TYPE_NSID,
          skey: TEST_SKEY + "-add-member",
        },
      },
    )
    if (!createResp.ok()) {
      throw new Error(
        `createSpace failed (${createResp.status()}): ${await createResp.text()}`,
      )
    }
    const { uri } = await createResp.json()
    createdSpaceUri = uri

    const addResp = await page.request.post(
      "/xrpc/com.atproto.simplespace.addMember",
      { data: { space: uri, did: "did:plc:test-member", access: "read" } },
    )
    expect(addResp.status()).toBe(201)
    const addBody = await addResp.json()
    expect(addBody.member.did).toBe("did:plc:test-member")
  })

  test("removeMember removes a previously added member", async ({ page }) => {
    const createResp = await page.request.post(
      "/xrpc/com.atproto.simplespace.createSpace",
      {
        data: {
          type: TEST_TYPE_NSID,
          skey: TEST_SKEY + "-remove-member",
        },
      },
    )
    if (!createResp.ok()) {
      throw new Error(
        `createSpace failed (${createResp.status()}): ${await createResp.text()}`,
      )
    }
    const { uri } = await createResp.json()
    createdSpaceUri = uri

    await page.request.post("/xrpc/com.atproto.simplespace.addMember", {
      data: { space: uri, did: "did:plc:test-member-rm", access: "read" },
    })

    const removeResp = await page.request.post(
      "/xrpc/com.atproto.simplespace.removeMember",
      { data: { space: uri, did: "did:plc:test-member-rm" } },
    )
    expect(removeResp.ok()).toBe(true)
  })

  test("listMembers includes added member", async ({ page }) => {
    const createResp = await page.request.post(
      "/xrpc/com.atproto.simplespace.createSpace",
      {
        data: {
          type: TEST_TYPE_NSID,
          skey: TEST_SKEY + "-list-members",
        },
      },
    )
    if (!createResp.ok()) {
      throw new Error(
        `createSpace failed (${createResp.status()}): ${await createResp.text()}`,
      )
    }
    const { uri } = await createResp.json()
    createdSpaceUri = uri

    await page.request.post("/xrpc/com.atproto.simplespace.addMember", {
      data: { space: uri, did: "did:plc:test-member-list", access: "write" },
    })

    const listResp = await page.request.get(
      "/xrpc/com.atproto.simplespace.listMembers",
      { params: { space: uri } },
    )
    expect(listResp.ok()).toBe(true)
    const listBody = await listResp.json()
    expect(listBody.members).toBeInstanceOf(Array)
    const found = listBody.members.some(
      (m: { did: string }) => m.did === "did:plc:test-member-list",
    )
    expect(found).toBe(true)
  })

  test("getConfig returns space configuration", async ({ page }) => {
    const createResp = await page.request.post(
      "/xrpc/com.atproto.simplespace.createSpace",
      {
        data: {
          type: TEST_TYPE_NSID,
          skey: TEST_SKEY + "-get-config",
          mintPolicy: "member-list",
        },
      },
    )
    if (!createResp.ok()) {
      throw new Error(
        `createSpace failed (${createResp.status()}): ${await createResp.text()}`,
      )
    }
    const { uri } = await createResp.json()
    createdSpaceUri = uri

    const configResp = await page.request.get(
      "/xrpc/com.atproto.simplespace.getConfig",
      { params: { space: uri } },
    )
    expect(configResp.ok()).toBe(true)
    const configBody = await configResp.json()
    expect(configBody.mintPolicy).toBe("member-list")
  })

  test("updateConfig changes mint policy", async ({ page }) => {
    const createResp = await page.request.post(
      "/xrpc/com.atproto.simplespace.createSpace",
      {
        data: {
          type: TEST_TYPE_NSID,
          skey: TEST_SKEY + "-update-config",
          mintPolicy: "member-list",
        },
      },
    )
    if (!createResp.ok()) {
      throw new Error(
        `createSpace failed (${createResp.status()}): ${await createResp.text()}`,
      )
    }
    const { uri } = await createResp.json()
    createdSpaceUri = uri

    const updateResp = await page.request.post(
      "/xrpc/com.atproto.simplespace.updateConfig",
      { data: { space: uri, mintPolicy: "public" } },
    )
    expect(updateResp.ok()).toBe(true)

    const configResp = await page.request.get(
      "/xrpc/com.atproto.simplespace.getConfig",
      { params: { space: uri } },
    )
    expect(configResp.ok()).toBe(true)
    const configBody = await configResp.json()
    expect(configBody.mintPolicy).toBe("public")
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
