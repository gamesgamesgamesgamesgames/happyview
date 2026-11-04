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

  it("shows a starting row's label alone, with no count and no bar", () => {
    const html = renderToStaticMarkup(
      createElement(RecordsProgress, {
        label: "Checking the Network store",
        active: true,
        starting: true,
        done: 0,
        total: null,
      }),
    );
    expect(html).toContain("Checking the Network store");
    expect(html).not.toContain('role="progressbar"');
    expect(html).not.toContain("records");
    expect(html).not.toContain("tabular-nums");
  });
});
