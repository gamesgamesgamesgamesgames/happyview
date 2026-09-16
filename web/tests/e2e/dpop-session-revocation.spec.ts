import { execSync } from "child_process";
import { createHash, randomBytes } from "crypto";
import { test, expect } from "@playwright/test";
import {
  countDpopRows,
  dpopKeyIdForProvision,
  insertDpopSession,
  loginAsTestAdmin,
  makeDpopProof,
} from "./auth-helper";

const HAPPYVIEW_CONTAINER = "happyview-e2e-happyview-1";
const PDS_ISSUER = "https://pds.localhost";
const USER_DID = "did:plc:e2e-revocation-subject";
const CLIENT_NAME = "e2e-revocation";

/**
 * Logging out of a DPoP session must tell the account's PDS, not just drop our
 * own row. Until it did, a signed-out session stayed listed as active on the
 * user's account until its refresh token expired — two years, as a
 * confidential client.
 */
test.describe("DPoP session revocation against a real PDS", () => {
  test.describe.configure({ timeout: 120_000 });

  let clientKey: string;
  let clientSecret: string;
  let apiClientId: string;
  let dpopKey: Record<string, unknown>;
  let dpopKeyId: string;
  const accessToken = "e2e-revocation-access-token";

  test.beforeAll(async ({ browser }) => {
    const page = await browser.newPage();
    await loginAsTestAdmin(page);

    const existing = await page.request.get("/admin/api-clients");
    if (existing.ok()) {
      for (const c of await existing.json()) {
        if (c.name === CLIENT_NAME) {
          await page.request.delete(`/admin/api-clients/${c.id}`);
        }
      }
    }

    const created = await page.request.post("/admin/api-clients", {
      data: {
        name: CLIENT_NAME,
        client_id_url: "http://localhost",
        client_uri: "http://localhost",
        redirect_uris: ["http://127.0.0.1/callback"],
        scopes: "atproto",
        client_type: "public",
      },
    });
    expect(
      created.ok(),
      `create api client: ${await created.text()}`,
    ).toBeTruthy();
    const client = await created.json();
    clientKey = client.client_key;
    clientSecret = client.client_secret ?? "";
    apiClientId = client.id;

    // A public client must prove possession of a PKCE verifier to provision.
    const verifier = randomBytes(32).toString("hex");
    const challenge = createHash("sha256").update(verifier).digest("base64url");

    const provisioned = await page.request.post("/oauth/dpop-keys", {
      data: { pkce_challenge: challenge },
      headers: { "x-client-key": clientKey },
    });
    expect(provisioned.status()).toBe(201);
    const body = await provisioned.json();
    dpopKey = body.dpop_key;
    dpopKeyId = await dpopKeyIdForProvision(body.provision_id);

    await insertDpopSession({
      id: "e2e-revocation-session",
      apiClientId,
      dpopKeyId,
      userDid: USER_DID,
      accessToken,
      refreshToken: "e2e-revocation-refresh-token",
      issuer: PDS_ISSUER,
      pdsUrl: PDS_ISSUER,
    });

    await page.close();
  });

  test.afterAll(async ({ browser }) => {
    if (!apiClientId) return;
    const page = await browser.newPage();
    await loginAsTestAdmin(page);
    await page.request.delete(`/admin/api-clients/${apiClientId}`);
    await page.close();
  });

  test("logging out revokes at the PDS and is accepted as an authenticated client", async ({
    page,
  }) => {
    const before = await countDpopRows(USER_DID);
    expect(before.sessions).toBe(1);

    const since = new Date(Date.now() - 5_000).toISOString();

    const url = `https://happyview.127-0-0-1.sslip.io/oauth/sessions/${USER_DID}`;
    const proof = await makeDpopProof(dpopKey, "DELETE", url, accessToken);

    const deleted = await page.request.delete(`/oauth/sessions/${USER_DID}`, {
      headers: {
        "x-client-key": clientKey,
        authorization: `DPoP ${accessToken}`,
        dpop: proof,
      },
    });
    expect(
      deleted.status(),
      `logout must succeed regardless of what the PDS says: ${await deleted.text()}`,
    ).toBe(204);

    const logs = execSync(
      `docker logs --since ${since} ${HAPPYVIEW_CONTAINER} 2>&1`,
    ).toString();

    const attempted =
      new RegExp(
        `revoked session tokens at authorization server.*${apiClientId}`,
      ).test(logs) ||
      new RegExp(
        `failed to revoke session at authorization server.*${apiClientId}`,
      ).test(logs);
    expect(
      attempted,
      `expected a revocation attempt against ${PDS_ISSUER} for client ${apiClientId}; logs were:\n${logs}`,
    ).toBeTruthy();

    for (const rejection of [
      "invalid_client",
      "invalid_dpop_proof",
      "unauthorized_client",
      "DPoP proof required",
    ]) {
      expect(
        logs.includes(rejection),
        `PDS rejected our client credentials with '${rejection}'; logs were:\n${logs}`,
      ).toBe(false);
    }
  });

  test("logout clears the session and its DPoP key even when the PDS cannot revoke", async () => {
    const after = await countDpopRows(USER_DID);
    expect(after.sessions, "session row must be gone").toBe(0);
    expect(after.keys, "the DPoP key must go with it").toBe(0);
  });
});
