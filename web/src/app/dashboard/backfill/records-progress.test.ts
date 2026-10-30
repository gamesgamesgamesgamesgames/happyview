import { describe, expect, it } from "vitest";

import {
  recordsCountText,
  recordsFraction,
  runByExecutor,
  runningLabel,
} from "./records-progress";

describe("runningLabel", () => {
  it("says a store pass is running, not starting, while its stage stays pending", () => {
    expect(runningLabel({ stage: "pending", source: "store-pass" })).toBe(
      "running",
    );
  });

  it("keeps starting for HappyView's own backfills", () => {
    for (const source of ["auto", "replay", "crawl"]) {
      expect(runningLabel({ stage: "pending", source })).toBe("starting");
    }
  });

  it("names any other stage", () => {
    expect(
      runningLabel({ stage: "resolving_and_fetching", source: "crawl" }),
    ).toBe("resolving and fetching");
  });
});

describe("runByExecutor", () => {
  it("is any source HappyView does not run itself", () => {
    expect(runByExecutor("store-pass")).toBe(true);
    expect(runByExecutor("crawl")).toBe(false);
  });
});

describe("recordsCountText", () => {
  it("says about for an estimated total", () => {
    expect(recordsCountText({ done: 1200, total: 5500, estimated: true })).toBe(
      `${(1200).toLocaleString()} of about ${(5500).toLocaleString()} records`,
    );
  });

  it("leaves about out of a known total, and counts what it is told", () => {
    expect(recordsCountText({ done: 3, total: 10, noun: "repos" })).toBe(
      "3 of 10 repos",
    );
  });

  it("stands alone without a total", () => {
    expect(recordsCountText({ done: 7, total: null, noun: "hooked" })).toBe(
      "7 hooked",
    );
    expect(recordsCountText({ done: 7 })).toBe("7 records");
  });
});

describe("recordsFraction", () => {
  it("is the share done, held to 1 when an estimate is passed", () => {
    expect(recordsFraction({ done: 25, total: 100 })).toBe(0.25);
    expect(recordsFraction({ done: 120, total: 100 })).toBe(1);
  });

  it("is null without a total to measure against", () => {
    expect(recordsFraction({ done: 5, total: null })).toBeNull();
    expect(recordsFraction({ done: 5, total: 0 })).toBeNull();
  });
});
