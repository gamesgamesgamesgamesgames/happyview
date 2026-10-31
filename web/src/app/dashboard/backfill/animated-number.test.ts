import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { stepToward } from "./animated-number";
import { RecordsProgress } from "./records-progress-bar";
import { type RecordsCount, recordsCountText } from "./records-progress";

describe("stepToward", () => {
  it("moves part of the way toward a new value, from either side", () => {
    expect(stepToward(0, 1000)).toBeCloseTo(60);
    expect(stepToward(1000, 0)).toBeCloseTo(940);
  });

  it("lands on the value once within half a unit, so a count settles", () => {
    expect(stepToward(999.6, 1000)).toBe(1000);
    expect(stepToward(1000, 1000)).toBe(1000);
  });

  it("reaches the value from a jump of millions", () => {
    let shown = 1_203_500;
    let frames = 0;
    while (shown !== 5_546_055 && frames < 1000) {
      shown = stepToward(shown, 5_546_055);
      frames++;
    }
    expect(shown).toBe(5_546_055);
  });
});

/** The row's text, as first rendered: each number at its value. */
function rowText(count: RecordsCount): string {
  const html = renderToStaticMarkup(
    createElement(RecordsProgress, { label: "Label", active: true, ...count }),
  );
  return html
    .replace(/<[^>]+>/g, "|")
    .split("|")
    .filter(Boolean)
    .join("");
}

describe("RecordsProgress", () => {
  it.each<[string, RecordsCount]>([
    [
      "an estimated total",
      { done: 1_203_500, total: 5_549_379, estimated: true },
    ],
    ["a known total", { done: 250, total: 1000, noun: "records hooked" }],
    ["a count that reached its total", { done: 5_546_055, total: 5_546_055 }],
    ["no total", { done: 42 }],
  ])("counts up the words recordsCountText says, for %s", (_, count) => {
    expect(rowText(count)).toBe(`Label${recordsCountText(count)}`);
  });
});
