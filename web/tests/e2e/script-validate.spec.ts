import { test, expect } from "@playwright/test";
import { loginAsTestAdmin } from "./auth-helper";

/**
 * Saving a script is checked by the interpreter its language names, and this
 * stack's `lua` interpreter is the echo fixture (see the volume comment in
 * `docker-compose.e2e.yml`). The fixture judges no real body, so what these
 * assert is the *path*: that a save asks an interpreter, and that what the
 * interpreter answers is what the operator is shown. Whether a given Lua
 * body is good is pinned by Rust tests against the real plugin, not here.
 */
const ID = "xrpc.query:test.e2e.scriptvalidate";

test.describe("Script validation reaches the interpreter", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page);
  });

  test.afterEach(async ({ page }) => {
    await page.request.delete(`/admin/scripts/${encodeURIComponent(ID)}`);
  });

  test("a refusal from the interpreter is the message the operator sees", async ({
    page,
  }) => {
    // Both are directives the fixture recognises, so each answer comes from
    // the interpreter rather than from anything on the host.
    const missingHandle = await page.request.post("/admin/scripts", {
      data: { id: ID, body: "no-handle" },
    });
    expect(missingHandle.status()).toBe(400);
    expect((await missingHandle.json()).error).toBe(
      "script must define a handle() function",
    );

    const willNotParse = await page.request.post("/admin/scripts", {
      data: { id: ID, body: "invalid, says the body" },
    });
    expect(willNotParse.status()).toBe(400);
    expect((await willNotParse.json()).error).toContain(
      "script compilation failed",
    );

    // Neither was stored.
    const read = await page.request.get(
      `/admin/scripts/${encodeURIComponent(ID)}`,
    );
    expect(read.status()).toBe(404);
  });

  test("a body the interpreter accepts is stored", async ({ page }) => {
    const saved = await page.request.post("/admin/scripts", {
      data: { id: ID, body: "function handle(input, ctx)\n  return input\nend\n" },
    });
    expect(saved.status()).toBe(201);
    expect((await saved.json()).runnable).toBe(true);
  });
});
