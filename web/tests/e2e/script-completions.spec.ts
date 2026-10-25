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

async function typeOnLine(page: Page, line: number, text: string) {
  const editor = page.locator(".monaco-editor").first();
  await expect(editor).toBeVisible({ timeout: 10000 });
  await editor.locator(".view-line").nth(line - 1).click();
  await page.keyboard.press("End");
  await page.keyboard.type(text);
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

    await typeOnLine(page, 2, "db.");
    await expect(suggestions).toContainText("records", { timeout: 10000 });
    await expect(suggestions).toContainText("search");
    await page.keyboard.press("Escape");

    // `input` is the query's Params, `ctx` a QueryCtx.
    await typeOnLine(page, 4, " input.");
    await expect(suggestions).toContainText("actor", { timeout: 10000 });
    await page.keyboard.press("Escape");

    await typeOnLine(page, 4, " ctx.");
    await expect(suggestions).toContainText("caller_did", { timeout: 10000 });
  });
});
