import { describe, expect, it } from "vitest";

import type { HookStats } from "@/types/backfill";

import {
  etaText,
  hookDetail,
  hookStatsOf,
  executorProgress,
  executorRanNothing,
  summaryParts,
  waitingForCrawl,
  recordsCountParts,
  recordsCountText,
  recordsFraction,
  runByExecutor,
  runningLabel,
  shareText,
} from "./records-progress";

describe("runningLabel", () => {
  it("says an executor's backfill is running, not starting, while its stage stays pending", () => {
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
    expect(recordsCountText({ done: 7, total: null, noun: "repos" })).toBe(
      "7 repos",
    );
    expect(recordsCountText({ done: 7 })).toBe("7 records");
  });
});

describe("recordsCountParts", () => {
  it("keeps the total, and says whether it is an estimate", () => {
    expect(recordsCountParts({ done: 3, total: 10, estimated: true })).toEqual({
      done: 3,
      total: 10,
      estimated: true,
      noun: "records",
    });
  });

  it("drops a known total the count has reached, but not an estimate", () => {
    expect(recordsCountParts({ done: 10, total: 10 }).total).toBeNull();
    expect(
      recordsCountParts({ done: 10, total: 10, estimated: true }).total,
    ).toBe(10);
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
      scriptShare: "2.1% of time in scripts",
      deadLettered: null,
    });
  });

  it("names what went wrong, and only when something did", () => {
    const detail = hookDetail(
      stats({ failed: 3, timeouts: 1, dead_lettered: 2 }),
      false,
    );
    expect(detail.progress.slice(2)).toEqual(["3 failed (1 timed out)"]);
    expect(detail.deadLettered).toBe("2");
  });

  it("leaves out timeouts and Dead Letters when there are none", () => {
    const detail = hookDetail(stats({ failed: 3 }), false);
    expect(detail.progress.slice(2)).toEqual(["3 failed"]);
    expect(detail.deadLettered).toBeNull();
  });

  it("keeps a tiny script share readable", () => {
    expect(hookDetail(stats({ script_share: 0.0004 }), false).scriptShare).toBe(
      "<0.1% of time in scripts",
    );
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
    ).toEqual({ progress: [], scriptShare: null, deadLettered: null });
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

describe("executorProgress", () => {
  it("counts against the store's estimate while the pass runs", () => {
    const row = executorProgress({
      stage: "pending",
      status: "running",
      processed_repos: 1200,
      total_repos: 5500,
      total_records: null,
    });
    expect(row.label).toBe("Running record scripts");
    expect(recordsCountText(row)).toBe(
      `${(1200).toLocaleString()} of about ${(5500).toLocaleString()} records`,
    );
  });

  it("reads a finished pass as finished, however far it fell short of the estimate", () => {
    const row = executorProgress({
      stage: "pending",
      status: "completed",
      processed_repos: 5546055,
      total_repos: 5549379,
      total_records: null,
    });
    expect(row.label).toBe("Ran record scripts");
    expect(recordsFraction(row)).toBe(1);
    expect(recordsCountText(row)).toBe(`${(5546055).toLocaleString()} records`);
  });

  it("keeps the estimate for a pass that stopped", () => {
    const row = executorProgress({
      stage: "pending",
      status: "cancelled",
      processed_repos: 10,
      total_repos: 100,
      total_records: null,
    });
    expect(recordsFraction(row)).toBe(0.1);
  });

  it("doesn't call a failed or paused pass running", () => {
    for (const status of ["failed", "cancelled", "paused"] as const) {
      const row = executorProgress({
        stage: "pending",
        status,
        processed_repos: 10,
        total_repos: 100,
        total_records: null,
      });
      expect(row.label).toBe("Record scripts");
    }
    const pausing = executorProgress({
      stage: "pending",
      status: "pausing",
      processed_repos: 10,
      total_repos: 100,
      total_records: null,
    });
    expect(pausing.label).toBe("Running record scripts");
  });
});

describe("a resumed pass's count", () => {
  it("counts what all its runs wrote once it completes", () => {
    const row = executorProgress({
      stage: "pending",
      status: "completed",
      processed_repos: 0,
      total_repos: 0,
      total_records: 120,
    });
    expect(recordsCountText(row)).toBe("120 records");
    expect(recordsFraction(row)).toBe(1);
  });
});

describe("executorRanNothing", () => {
  const ran = (
    job: {
      status: string;
      processed_repos: number | null;
      total_records: number | null;
    },
    done?: number,
  ) => executorRanNothing(job, done == null ? null : { done });

  it("is a completed pass that hooked nothing, whatever its note says", () => {
    expect(
      ran({ status: "completed", processed_repos: 0, total_records: 0 }),
    ).toBe(true);
    expect(
      ran({ status: "completed", processed_repos: 0, total_records: 0 }, 0),
    ).toBe(true);
  });

  it("is the row the store keeps for a collection with no history, whose counts are unset", () => {
    expect(
      ran({ status: "completed", processed_repos: null, total_records: null }),
    ).toBe(true);
  });

  it("is not a pass that ran records", () => {
    expect(
      ran({ status: "completed", processed_repos: 12, total_records: 12 }),
    ).toBe(false);
    expect(
      ran({ status: "completed", processed_repos: 0, total_records: 0 }, 3),
    ).toBe(false);
  });

  it("is a pass that failed or was cancelled before it counted anything", () => {
    for (const status of ["failed", "cancelled"]) {
      expect(ran({ status, processed_repos: 0, total_records: 0 })).toBe(true);
    }
    expect(
      ran({ status: "failed", processed_repos: 4, total_records: 4 }),
    ).toBe(false);
  });

  it("is not a pass that is still going or paused", () => {
    for (const status of ["running", "pausing", "paused"]) {
      expect(ran({ status, processed_repos: 0, total_records: 0 })).toBe(false);
    }
  });
});

describe("waiting for the Network store's crawl", () => {
  it("is a running job at the waiting stage", () => {
    expect(
      waitingForCrawl({ status: "running", stage: "waiting_for_crawl" }),
    ).toBe(true);
    expect(
      waitingForCrawl({ status: "pausing", stage: "waiting_for_crawl" }),
    ).toBe(true);
    expect(waitingForCrawl({ status: "running", stage: "pending" })).toBe(
      false,
    );
    expect(
      waitingForCrawl({ status: "paused", stage: "waiting_for_crawl" }),
    ).toBe(false);
  });

  it("says so in the badge", () => {
    expect(
      runningLabel({ stage: "waiting_for_crawl", source: "store-pass" }),
    ).toBe("waiting for crawl");
  });

  it("counts what has arrived, with no total, while it waits", () => {
    const row = executorProgress({
      stage: "waiting_for_crawl",
      status: "running",
      processed_repos: 40,
      total_repos: 0,
      total_records: null,
    });
    expect(row.label).toBe("Waiting for the Network store's crawl");
    expect(recordsFraction(row)).toBeNull();
    expect(recordsCountText(row)).toBe("40 records arrived so far");
  });
});

describe("summaryParts", () => {
  const bare = (iso: string) => iso;
  const before = Date.parse("2026-11-02T14:00:00+00:00");

  it("reads the records, the crawl and the next attempt in order", () => {
    expect(
      summaryParts(
        {
          records: 18571,
          state: "failed",
          retry_at: "2026-11-02T15:00:00+00:00",
        },
        bare,
        before,
      ),
    ).toEqual([
      `${(18571).toLocaleString()} records`,
      "crawl failed",
      "next crawl attempt at 2026-11-02T15:00:00+00:00",
    ]);
  });

  it("says a next attempt that has gone by is due", () => {
    expect(
      summaryParts(
        {
          records: null,
          state: "failed",
          retry_at: "2026-11-02T15:00:00+00:00",
        },
        bare,
        Date.parse("2026-11-02T16:00:00+00:00"),
      ),
    ).toEqual(["crawl failed", "next crawl attempt due"]);
  });

  it("reads the time in the reader's own format by default", () => {
    const iso = "2026-11-02T15:00:00+00:00";
    expect(
      summaryParts(
        { records: null, state: null, retry_at: iso },
        undefined,
        before,
      ),
    ).toEqual([`next crawl attempt at ${new Date(iso).toLocaleString()}`]);
  });

  it("names every crawl state", () => {
    expect(
      summaryParts({ records: null, state: "queued", retry_at: null }),
    ).toEqual(["crawl queued"]);
    expect(
      summaryParts({ records: null, state: "running", retry_at: null }),
    ).toEqual(["crawling"]);
    expect(
      summaryParts({ records: null, state: "complete", retry_at: null }),
    ).toEqual(["crawl complete"]);
  });

  it("leaves out what it doesn't know", () => {
    expect(
      summaryParts({ records: null, state: null, retry_at: null }),
    ).toEqual([]);
    expect(summaryParts({ records: 0, state: null, retry_at: null })).toEqual([
      "0 records",
    ]);
  });
});
