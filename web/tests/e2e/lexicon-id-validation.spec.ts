import { test, expect } from "@playwright/test";
import { loginAsTestAdmin } from "./auth-helper";

async function replaceLexiconJson(
  page: import("@playwright/test").Page,
  text: string,
) {
  await page.locator(".monaco-editor").first().click();
  await page.keyboard.press("ControlOrMeta+KeyA");
  await page.evaluate((value) => {
    const data = new DataTransfer();
    data.setData("text/plain", value);
    document.activeElement?.dispatchEvent(
      new ClipboardEvent("paste", {
        clipboardData: data,
        bubbles: true,
        cancelable: true,
      }),
    );
  }, text);
}

function lexiconWithId(id: string) {
  return JSON.stringify({
    lexicon: 1,
    id,
    defs: {
      main: {
        type: "record",
        key: "tid",
        record: { type: "object", required: [], properties: {} },
      },
    },
  });
}

test.describe("Lexicon ID validation", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsTestAdmin(page);
    await page.goto("/dashboard/lexicons/new");
  });

  test("the boilerplate carries a valid placeholder NSID", async ({ page }) => {
    await expect(page.getByRole("button", { name: "Upload" })).toBeEnabled();
  });

  test("Upload is disabled when the lexicon ID is empty", async ({ page }) => {
    await replaceLexiconJson(page, lexiconWithId(""));

    await expect(page.getByRole("button", { name: "Upload" })).toBeDisabled();
    await expect(page.getByText(/valid NSID/i)).toBeVisible();
  });

  test("Upload is disabled when the lexicon ID is not an NSID", async ({
    page,
  }) => {
    await replaceLexiconJson(page, lexiconWithId("not-an-nsid"));

    await expect(page.getByRole("button", { name: "Upload" })).toBeDisabled();
  });

  test("Upload is re-enabled once a valid NSID is supplied", async ({
    page,
  }) => {
    await replaceLexiconJson(page, lexiconWithId(""));
    await expect(page.getByRole("button", { name: "Upload" })).toBeDisabled();

    await replaceLexiconJson(page, lexiconWithId("test.e2e.lexiconid.thing"));
    await expect(page.getByRole("button", { name: "Upload" })).toBeEnabled();
  });
});
