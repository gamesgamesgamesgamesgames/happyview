import { test, expect } from "@playwright/test";
import { loginAsTestAdmin } from "./auth-helper";

/**
 * Saving a script is checked by the interpreter its language names, and this
 * stack's `lua` interpreter is the published Lua plugin (see the volume
 * comment in `docker-compose.e2e.yml`). So these assert both halves: that a
 * save asks an interpreter and shows the operator what it answered, and that
 * the answers are real Lua judgements. The bodies below are therefore Lua,
 * not fixture directives — a body that parses but defines no `handle`, and
 * one that does not parse.
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
    // Parses, defines no `handle`. A missing handle is a sentence about the
    // script's shape, so it is the interpreter's own words rather than a
    // compilation failure at a line.
    const missingHandle = await page.request.post("/admin/scripts", {
      data: { id: ID, body: "local x = 1\n" },
    });
    expect(missingHandle.status()).toBe(400);
    expect((await missingHandle.json()).error).toBe(
      "script must define a handle() function",
    );

    const willNotParse = await page.request.post("/admin/scripts", {
      data: { id: ID, body: "function handle(\n" },
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
