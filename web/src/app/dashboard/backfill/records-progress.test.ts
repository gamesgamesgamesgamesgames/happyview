import { describe, expect, it } from "vitest";

import type { HookStats } from "@/types/backfill";

import {
  etaText,
  hookDetail,
  hookStatsOf,
  recordsCountText,
  recordsFraction,
  runByExecutor,
  runningLabel,
  shareText,
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

const stats = (over: Partial<HookStats> = {}): HookStats => ({
  done: 1200,
  failed: 0,
  timeouts: 0,
  dead_lettered: 0,
  skipped: 0,
  outcomes: {},
  rate_per_s: 410.4,
  avg_rate_per_s: 395,
  eta_s: 10_500,
  record_ms_p50: 2.4,
  script_ms_p50: 0.05,
  script_share: 0.021,
  stages: {},
  ...over,
});

describe("etaText", () => {
  it("rounds to minutes, and says hours once there are any", () => {
    expect(etaText(30)).toBe("under a minute");
    expect(etaText(240)).toBe("about 4 m");
    expect(etaText(10_500)).toBe("about 2 h 55 m");
    expect(etaText(7200)).toBe("about 2 h");
  });
});

describe("shareText", () => {
  it("keeps one decimal below ten percent and none above", () => {
    expect(shareText(0.95)).toBe("95%");
    expect(shareText(0.021)).toBe("2.1%");
    expect(shareText(0.02)).toBe("2%");
    expect(shareText(0.0004)).toBe("<0.1%");
    expect(shareText(0)).toBe("0%");
  });
});

describe("hookDetail", () => {
  it("reads a running job's rate and time left, and the script's share", () => {
    expect(hookDetail(stats(), false)).toEqual({
      progress: ["410/s", "about 2 h 55 m left"],
      scriptShare: "script 2.1% of record time",
    });
  });

  it("names what went wrong, and only when something did", () => {
    const detail = hookDetail(
      stats({ failed: 3, timeouts: 1, dead_lettered: 2 }),
      false,
    );
    expect(detail.progress.slice(2)).toEqual([
      "3 failed",
      "1 timed out",
      "2 dead-lettered",
    ]);
  });

  it("reads a finished job's average, with no time left", () => {
    expect(hookDetail(stats(), true).progress).toEqual(["averaged 395/s"]);
  });

  it("leaves out a rate of zero, an unknown time left and an unknown share", () => {
    expect(
      hookDetail(
        stats({ rate_per_s: 0, eta_s: null, script_share: null }),
        false,
      ),
    ).toEqual({ progress: [], scriptShare: null });
  });

  it("keeps a decimal on a slow rate", () => {
    expect(hookDetail(stats({ rate_per_s: 2.46 }), false).progress[0]).toBe(
      "2.5/s",
    );
  });
});

describe("hookStatsOf", () => {
  it("lifts the stats out of a hook_stats event", () => {
    const { done, outcomes, stages, ...rest } = stats();
    expect(
      hookStatsOf({
        type: "hook_stats",
        job_id: "j",
        done,
        outcomes,
        stages,
        ...rest,
      }),
    ).toEqual(stats());
  });

  it("answers null for an event missing its counts", () => {
    expect(hookStatsOf({ type: "hook_stats", job_id: "j" })).toBeNull();
  });
});
