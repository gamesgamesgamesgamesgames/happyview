import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { test, expect, type Page } from "@playwright/test";
import { loginAsTestAdmin } from "./auth-helper";

/**
 * The script editor completes from the declarations the interpreter renders.
 * The endpoint is answered with files the Lua interpreter rendered for real
 * (the same fixture the reader's unit tests use), so this runs whether or not
 * the stack's pinned interpreter renders declarations yet; the save still goes
 * through the stack's real Lua interpreter.
 */
const ID = "xrpc.query:com.example.feed.getPosts";
const FIXTURE = join(__dirname, "../../src/lib/luals/fixture");

function files(dir: string): { path: string; contents: string }[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) return files(path);
    return [
      { path: relative(FIXTURE, path), contents: readFileSync(path, "utf8") },
    ];
  });
}

/** Type at the end of the script, on a line of its own. Monaco reuses its
 * line elements out of order, so clicking a line by index is not a way to
 * reach it. */
async function typeAtEnd(page: Page, text: string) {
  const editor = page.locator(".monaco-editor").first();
  await expect(editor).toBeVisible({ timeout: 10000 });
  await editor.locator(".view-lines").click();
  await page.keyboard.press("ControlOrMeta+End");
  await page.keyboard.press("Enter");
  await page.keyboard.type(text);
}

/** Type `text` at the end of the script and expect the suggestions to
 * include `expected`. Typed at machine speed, the `.` can land while Monaco is
 * still answering the quick suggestions the name before it opened, and that
 * session filters its own list by the `.` rather than asking the providers
 * again. Asking explicitly (Ctrl+Space) is a fresh request, so a provider that
 * offers nothing still fails. */
async function completes(page: Page, text: string, expected: string) {
  await typeAtEnd(page, text);
  const suggestions = page.locator(".suggest-widget");
  await expect(async () => {
    if (!((await suggestions.textContent()) ?? "").includes(expected)) {
      await page.keyboard.press("Escape");
      await page.keyboard.press("Control+Space");
    }
    await expect(suggestions).toContainText(expected, { timeout: 2000 });
  }).toPass({ timeout: 15000 });
}

test.describe("Script editor completions", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page);
    await page.route(
      (url) => url.pathname.endsWith("/admin/editor/declarations"),
      (route) =>
        route.fulfill({
          json: { language_id: "lua", files: files(FIXTURE), notes: [] },
        }),
    );
    const saved = await page.request.post("/admin/scripts", {
      data: {
        id: ID,
        body: 'local db = require("happyview.db")\n\nfunction handle(input, ctx)\n  return {}\nend\n',
      },
    });
    expect(saved.status()).toBe(201);
  });

  test.afterEach(async ({ page }) => {
    await page.request.delete(`/admin/scripts/${encodeURIComponent(ID)}`);
  });

  test("a required library, input and ctx complete their members", async ({
    page,
  }) => {
    await page.goto(`/dashboard/settings/scripts/${encodeURIComponent(ID)}`);
    const suggestions = page.locator(".suggest-widget");

    await completes(page, "db.", "records");
    await expect(suggestions).toContainText("search");
    await page.keyboard.press("Escape");

    // `input` is the query's Params, `ctx` a QueryCtx.
    await completes(page, "input.", "actor");
    await page.keyboard.press("Escape");

    await completes(page, "ctx.", "caller_did");
  });
});
