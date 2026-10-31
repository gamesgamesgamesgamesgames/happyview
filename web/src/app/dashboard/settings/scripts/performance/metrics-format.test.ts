import { describe, expect, it } from "vitest";

import type { ScriptStats, SeriesPoint } from "@/types/metrics";

import {
  chartPoints,
  errorCount,
  failureBreakdown,
  formatMs,
  kindLabel,
  latencyAxisTop,
  niceCeil,
  rateText,
  retentionNote,
  runBreakdown,
  stageMeans,
  stepFor,
  summaryLine,
  timeoutCount,
  windowText,
  withMaxOverflow,
} from "./metrics-format";

function stats(overrides: Partial<ScriptStats> = {}): ScriptStats {
  const none = { p50: null, p95: null, p99: null, max: null, mean: null };
  return {
    runs: 0,
    outcomes: {},
    wall_ms: none,
    guest_ms: none,
    prepare_ms_mean: null,
    instantiate_ms_mean: null,
    link_ms_mean: null,
    allocate_ms_mean: null,
    teardown_ms_mean: null,
    host_ms_mean: null,
    settle_ms_mean: null,
    host_imports: [],
    memory_peak_bytes: { max: null, mean: null },
    fuel_mean: null,
    module_cache: { hits: 0, misses: 0 },
    ...overrides,
  };
}

describe("stepFor", () => {
  it("picks the API's default step for each window", () => {
    expect(stepFor("1h")).toBe("1m");
    expect(stepFor("24h")).toBe("30m");
    expect(stepFor("7d")).toBe("1h");
    expect(stepFor("30d")).toBe("1d");
  });
});

describe("windowText", () => {
  it("reads as the end of a sentence", () => {
    expect(`No script runs in ${windowText("1h")}.`).toBe(
      "No script runs in the last hour.",
    );
    expect(windowText("30d")).toBe("the last 30 days");
  });
});

describe("formatMs", () => {
  it("gives small durations their digits and large ones fewer", () => {
    expect(formatMs(0.05)).toBe("0.05 ms");
    expect(formatMs(0.001)).toBe("<0.01 ms");
    expect(formatMs(3.14)).toBe("3.1 ms");
    expect(formatMs(2.0)).toBe("2 ms");
    expect(formatMs(164.064)).toBe("164 ms");
    expect(formatMs(1638)).toBe("1.6 s");
    expect(formatMs(42_000)).toBe("42 s");
  });

  it("reads zero as zero and a missing value as a dash", () => {
    expect(formatMs(0)).toBe("0 ms");
    expect(formatMs(null)).toBe("—");
    expect(formatMs(undefined)).toBe("—");
  });
});

describe("rateText", () => {
  it("rounds a busy rate and keeps a slow one's digits", () => {
    expect(rateText(410.2)).toBe("410/s");
    expect(rateText(2.44)).toBe("2.4/s");
    expect(rateText(0.048)).toBe("0.05/s");
    expect(rateText(0)).toBe("0/s");
  });
});

describe("errorCount and timeoutCount", () => {
  const outcomes = { ok: 10, runtime: 2, syntax: 1, timeout: 3 };

  it("counts every failure but a timeout as an error", () => {
    expect(errorCount(outcomes)).toBe(3);
    expect(timeoutCount(outcomes)).toBe(3);
  });

  it("is zero with nothing recorded", () => {
    expect(errorCount({})).toBe(0);
    expect(timeoutCount({})).toBe(0);
  });

  it("lists failures worst first", () => {
    expect(failureBreakdown(outcomes)).toBe("3 timeout, 2 runtime, 1 syntax");
  });
});

describe("summaryLine", () => {
  it("says runs, p95 and errors", () => {
    expect(
      summaryLine(
        stats({
          runs: 12400,
          outcomes: { ok: 12400 },
          wall_ms: { p50: 1, p95: 3.1, p99: 4, max: 9, mean: 1.2 },
        }),
      ),
    ).toBe(
      `Last hour: ${(12400).toLocaleString()} runs · p95 3.1 ms · 0 errors`,
    );
  });

  it("adds timeouts only when there are some, and counts one in the singular", () => {
    expect(
      summaryLine(
        stats({
          runs: 1,
          outcomes: { runtime: 1 },
          wall_ms: { p50: 1, p95: 2, p99: 2, max: 2, mean: 2 },
        }),
      ),
    ).toBe("Last hour: 1 run · p95 2 ms · 1 error");
    expect(
      summaryLine(stats({ runs: 4, outcomes: { ok: 2, timeout: 2 } })),
    ).toBe("Last hour: 4 runs · p95 — · 0 errors · 2 timeouts");
  });
});

describe("runBreakdown", () => {
  it("counts the script's code and host calls as the script, the rest as the platform", () => {
    const b = runBreakdown(
      stats({
        guest_ms: { p50: 0, p95: 0, p99: 0, max: 0, mean: 0.05 },
        host_ms_mean: 0.05,
        prepare_ms_mean: 1.5,
        link_ms_mean: 0.4,
        allocate_ms_mean: 0.4,
        settle_ms_mean: null,
        teardown_ms_mean: 0.1,
      }),
    );
    expect(b.scriptMs).toBeCloseTo(0.1);
    expect(b.platformMs).toBeCloseTo(2.4);
    expect(b.scriptShare).toBeCloseTo(0.04);
    expect(b.segments.map((s) => s.key)).toEqual([
      "prepare",
      "link",
      "allocate",
      "guest",
      "host",
      "teardown",
    ]);
    expect(b.segments.reduce((sum, s) => sum + s.share, 0)).toBeCloseTo(1);
  });

  it("has no share when nothing was measured", () => {
    const b = runBreakdown(stats());
    expect(b.segments).toEqual([]);
    expect(b.scriptShare).toBeNull();
  });
});

describe("stageMeans", () => {
  it("orders stages as a record meets them and leaves counts out", () => {
    const means = stageMeans({
      delta: { n: 2, ms_mean: 0.001 },
      "hook.link": { n: 2, ms_mean: 0.39 },
      resolve: { n: 2, ms_mean: 0.21 },
      write_skipped: { n: 2, ms_mean: null },
      hook: { n: 2, ms_mean: 1.8 },
      custom_stage: { n: 1, ms_mean: 0.5 },
    });
    expect(means.map((m) => m.name)).toEqual([
      "resolve",
      "hook",
      "hook.link",
      "delta",
      "custom_stage",
    ]);
    expect(means.find((m) => m.name === "hook.link")?.partOfHook).toBe(true);
    expect(means.find((m) => m.name === "custom_stage")?.label).toBe(
      "custom stage",
    );
  });
});

describe("retentionNote", () => {
  it("says a long window holds only what retention keeps", () => {
    expect(retentionNote("30d", 7)).toBe(
      "Metrics are kept for 7 days, so this shows 7 days.",
    );
  });

  it("is silent when the window fits, retention is forever, or the window is in memory", () => {
    expect(retentionNote("7d", 30)).toBeNull();
    expect(retentionNote("30d", 0)).toBeNull();
    expect(retentionNote("1h", undefined)).toBeNull();
  });
});

describe("chartPoints", () => {
  const point = (over: Partial<SeriesPoint>): SeriesPoint => ({
    start: "2026-10-30T21:29:00+00:00",
    runs: 0,
    outcomes: {},
    wall_ms: { p50: null, p95: null, max: null },
    guest_ms: { p50: null, p95: null, max: null },
    host_ms_mean: null,
    instantiate_ms_mean: null,
    link_ms_mean: null,
    allocate_ms_mean: null,
    teardown_ms_mean: null,
    ...over,
  });

  it("keeps an idle step's latency null, so it draws as a gap", () => {
    const [row] = chartPoints([point({})]);
    expect(row.runs).toBe(0);
    expect(row.p50).toBeNull();
    expect(row.p95).toBeNull();
    expect(row.max).toBeNull();
  });

  it("splits a step's failures into errors and timeouts", () => {
    const [row] = chartPoints([
      point({
        runs: 4,
        outcomes: { ok: 1, runtime: 2, timeout: 1 },
        wall_ms: { p50: 1.5, p95: 2, max: 9 },
      }),
    ]);
    expect(row).toEqual({
      t: Date.parse("2026-10-30T21:29:00Z"),
      runs: 4,
      errors: 2,
      timeouts: 1,
      p50: 1.5,
      p95: 2,
      max: 9,
    });
  });
});

describe("kindLabel", () => {
  it("names record events as hooks", () => {
    expect(kindLabel("record_event")).toBe("Record hook");
    expect(kindLabel("something_new")).toBe("something new");
  });
});

describe("niceCeil", () => {
  it("rounds up to 1, 2, 2.5 or 5 times a power of ten", () => {
    expect(niceCeil(6.2)).toBe(10);
    expect(niceCeil(1.6)).toBe(2);
    expect(niceCeil(2.2)).toBe(2.5);
    expect(niceCeil(0.37)).toBeCloseTo(0.5);
    expect(niceCeil(122)).toBe(200);
    expect(niceCeil(5)).toBe(5);
    expect(niceCeil(0)).toBe(0);
  });
});

describe("latencyAxisTop", () => {
  const row = (p95: number | null, max: number | null) => ({
    t: 0,
    runs: 1,
    errors: 0,
    timeouts: 0,
    p50: p95,
    p95,
    max,
  });

  it("scales to three times p95 when one slow run would flatten the rest", () => {
    expect(latencyAxisTop([row(3.1, 41), row(2.8, 12)])).toBe(10);
  });

  it("stops at max when max is close to p95", () => {
    expect(latencyAxisTop([row(61, 70)])).toBe(100);
  });

  it("leaves the axis alone with no runs", () => {
    expect(latencyAxisTop([row(null, null)])).toBeNull();
  });
});

describe("withMaxOverflow", () => {
  const row = (max: number | null) => ({
    t: 0,
    runs: max == null ? 0 : 1,
    errors: 0,
    timeouts: 0,
    p50: null,
    p95: null,
    max,
  });

  it("draws max under the top as the line and max over it on the edge", () => {
    const [under, over, idle] = withMaxOverflow(
      [row(4), row(41), row(null)],
      10,
    );
    expect(under).toMatchObject({ max: 4, maxShown: 4, maxOver: null });
    expect(over).toMatchObject({ max: 41, maxShown: null, maxOver: 10 });
    expect(idle).toMatchObject({ maxShown: null, maxOver: null });
  });

  it("marks an off-scale step alone only when no neighbour is off-scale too", () => {
    const rows = withMaxOverflow(
      [row(41), row(4), row(41), row(41), row(4)],
      10,
    );
    expect(rows.map((r) => r.maxOverAlone)).toEqual([
      true,
      false,
      false,
      false,
      false,
    ]);
  });

  it("keeps every max on the line when the axis has no fixed top", () => {
    expect(withMaxOverflow([row(41)], null)[0].maxShown).toBe(41);
  });
});
