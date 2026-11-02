import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { RecordsProgress } from "./records-progress-bar";

function hasBar(active: boolean, total: number | null): boolean {
  const html = renderToStaticMarkup(
    createElement(RecordsProgress, { label: "Label", active, done: 5, total }),
  );
  return html.includes('role="progressbar"');
}

describe("RecordsProgress", () => {
  it("draws a bar with a total to measure against, running or not", () => {
    expect(hasBar(true, 10)).toBe(true);
    expect(hasBar(false, 10)).toBe(true);
  });

  it("draws an unknown total only while it runs", () => {
    expect(hasBar(true, null)).toBe(true);
    expect(hasBar(false, null)).toBe(false);
  });
});
