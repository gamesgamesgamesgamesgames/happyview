// Relative, not `@/`: vitest runs without the path alias.
import { shareText } from "../../../backfill/records-progress";
import type {
  IngestRow,
  MetricsStep,
  MetricsWindow,
  Outcomes,
  ScriptStats,
  SeriesPoint,
} from "@/types/metrics";

export { shareText };

export const WINDOWS: readonly MetricsWindow[] = ["1h", "24h", "7d", "30d"];

/** The step a window's graphs are drawn at: the API's default for each. */
export function stepFor(window: MetricsWindow): MetricsStep {
  switch (window) {
    case "1h":
      return "1m";
    case "24h":
      return "30m";
    case "7d":
      return "1h";
    case "30d":
      return "1d";
  }
}

/** "the last hour", for "No script runs in the last hour." */
export function windowText(window: MetricsWindow): string {
  switch (window) {
    case "1h":
      return "the last hour";
    case "24h":
      return "the last 24 hours";
    case "7d":
      return "the last 7 days";
    case "30d":
      return "the last 30 days";
  }
}

/** One step's name, for "Runs per minute". */
export function stepNoun(step: MetricsStep): string {
  switch (step) {
    case "1m":
      return "minute";
    case "30m":
      return "half hour";
    case "1h":
      return "hour";
    case "1d":
      return "day";
  }
}

/**
 * A duration in milliseconds, with as many digits as its size needs:
 * "0.05 ms", "3.1 ms", "164 ms", "1.6 s". Null reads as a dash.
 */
export function formatMs(ms: number | null | undefined): string {
  if (ms == null || !Number.isFinite(ms)) return "—";
  if (ms === 0) return "0 ms";
  if (ms < 0.01) return "<0.01 ms";
  if (ms < 1) return `${trim(ms.toFixed(2))} ms`;
  if (ms < 10) return `${trim(ms.toFixed(1))} ms`;
  if (ms < 1000) return `${Math.round(ms)} ms`;
  const s = ms / 1000;
  return `${trim(s.toFixed(s < 10 ? 1 : 0))} s`;
}

function trim(fixed: string): string {
  return fixed.includes(".") ? fixed.replace(/\.?0+$/, "") : fixed;
}

/** "410/s", "2.4/s", "0.05/s". */
export function rateText(perSecond: number): string {
  if (perSecond >= 10) return `${Math.round(perSecond).toLocaleString()}/s`;
  if (perSecond >= 0.1) return `${trim(perSecond.toFixed(1))}/s`;
  if (perSecond > 0) return `${trim(perSecond.toFixed(2))}/s`;
  return "0/s";
}

/** Runs that timed out. */
export function timeoutCount(outcomes: Outcomes): number {
  return outcomes.timeout ?? 0;
}

/** Runs that ended in anything but `ok` or a timeout, which counts apart. */
export function errorCount(outcomes: Outcomes): number {
  let n = 0;
  for (const [outcome, count] of Object.entries(outcomes)) {
    if (outcome !== "ok" && outcome !== "timeout") n += count;
  }
  return n;
}

/** The failures in a set of outcomes, worst-first, for a tooltip. */
export function failureBreakdown(outcomes: Outcomes): string {
  return Object.entries(outcomes)
    .filter(([outcome, n]) => outcome !== "ok" && n > 0)
    .sort((a, b) => b[1] - a[1])
    .map(
      ([outcome, n]) => `${n.toLocaleString()} ${outcome.replace(/_/g, " ")}`,
    )
    .join(", ");
}

/**
 * The script editor's summary of the last hour:
 * "Last hour: 12,400 runs · p95 3.1 ms · 0 errors".
 */
export function summaryLine(stats: ScriptStats): string {
  const errors = errorCount(stats.outcomes);
  const timeouts = timeoutCount(stats.outcomes);
  const parts = [
    `${stats.runs.toLocaleString()} ${stats.runs === 1 ? "run" : "runs"}`,
    `p95 ${formatMs(stats.wall_ms.p95)}`,
    `${errors.toLocaleString()} ${errors === 1 ? "error" : "errors"}`,
  ];
  if (timeouts > 0) {
    parts.push(
      `${timeouts.toLocaleString()} ${timeouts === 1 ? "timeout" : "timeouts"}`,
    );
  }
  return `Last hour: ${parts.join(" · ")}`;
}

export type Side = "script" | "platform";

export interface Segment {
  key: string;
  label: string;
  side: Side;
  ms: number;
  /** Of every segment's time together, 0 to 1. */
  share: number;
}

export interface Breakdown {
  segments: Segment[];
  scriptMs: number;
  platformMs: number;
  /** Null when nothing was measured. */
  scriptShare: number | null;
}

function breakdownOf(
  parts: { key: string; label: string; side: Side; ms: number | null }[],
): Breakdown {
  const measured = parts.filter(
    (p): p is (typeof parts)[number] & { ms: number } =>
      p.ms != null && p.ms > 0,
  );
  const total = measured.reduce((sum, p) => sum + p.ms, 0);
  const segments = measured.map((p) => ({
    ...p,
    share: total > 0 ? p.ms / total : 0,
  }));
  const scriptMs = segments
    .filter((s) => s.side === "script")
    .reduce((sum, s) => sum + s.ms, 0);
  return {
    segments,
    scriptMs,
    platformMs: total - scriptMs,
    scriptShare: total > 0 ? scriptMs / total : null,
  };
}

/**
 * Where a script's mean run goes. The script's side is its own code and the
 * host calls it made, as the ingest view counts a hook's; everything else is
 * the platform building, linking, settling and dropping its instance. Each
 * mean is over the runs that reached that stage, so the parts are close to,
 * not exactly, the mean wall time.
 */
export function runBreakdown(stats: ScriptStats): Breakdown {
  return breakdownOf([
    {
      key: "prepare",
      label: "Prepare",
      side: "platform",
      ms: stats.prepare_ms_mean,
    },
    { key: "link", label: "Link", side: "platform", ms: stats.link_ms_mean },
    {
      key: "allocate",
      label: "Allocate",
      side: "platform",
      ms: stats.allocate_ms_mean,
    },
    {
      key: "guest",
      label: "Script code",
      side: "script",
      ms: stats.guest_ms.mean,
    },
    {
      key: "host",
      label: "Host calls",
      side: "script",
      ms: stats.host_ms_mean,
    },
    {
      key: "settle",
      label: "Settle",
      side: "platform",
      ms: stats.settle_ms_mean,
    },
    {
      key: "teardown",
      label: "Teardown",
      side: "platform",
      ms: stats.teardown_ms_mean,
    },
  ]);
}

/** The ingest stages in a record's order; any other follows by name. */
const STAGE_ORDER = [
  "resolve",
  "cid_check",
  "hook",
  "hook.link",
  "hook.allocate",
  "hook.teardown",
  "write",
  "delta",
];

const STAGE_LABELS: Record<string, string> = {
  resolve: "lookup",
  cid_check: "CID check",
  hook: "hook",
  "hook.link": "link",
  "hook.allocate": "allocate",
  "hook.teardown": "teardown",
  write: "write",
  delta: "storage",
};

export interface StageMean {
  name: string;
  label: string;
  ms: number;
  /** A part of the hook stage rather than more time beside it. */
  partOfHook: boolean;
}

/** An ingest row's timed stages, in a record's order. Counts are left out. */
export function stageMeans(stages: IngestRow["stages"]): StageMean[] {
  const rank = (name: string) => {
    const i = STAGE_ORDER.indexOf(name);
    return i === -1 ? STAGE_ORDER.length : i;
  };
  return Object.entries(stages)
    .filter(([, s]) => s.ms_mean != null)
    .sort(([a], [b]) => rank(a) - rank(b) || a.localeCompare(b))
    .map(([name, s]) => ({
      name,
      label: STAGE_LABELS[name] ?? name.replace(/_/g, " "),
      ms: s.ms_mean as number,
      partOfHook: name.startsWith("hook."),
    }));
}

/**
 * Why a stored window may hold less than it says: the table keeps rows for
 * `retention_days`, so a 30-day window on a 7-day instance is a week. Null
 * when the window fits.
 */
export function retentionNote(
  window: MetricsWindow,
  retentionDays: number | undefined,
): string | null {
  if (retentionDays == null || retentionDays === 0) return null;
  const days = window === "7d" ? 7 : window === "30d" ? 30 : 0;
  if (days <= retentionDays) return null;
  return `Metrics are kept for ${retentionDays} ${
    retentionDays === 1 ? "day" : "days"
  }, so this shows ${retentionDays === 1 ? "one day" : `${retentionDays} days`}.`;
}

export interface ChartPoint {
  /** The step's start, in epoch milliseconds. */
  t: number;
  runs: number;
  errors: number;
  timeouts: number;
  /** Null for a step with no runs, drawn as a gap. */
  p50: number | null;
  p95: number | null;
  max: number | null;
}

/** A series' points as rows a chart reads. */
export function chartPoints(points: SeriesPoint[]): ChartPoint[] {
  return points.map((p) => ({
    t: Date.parse(p.start),
    runs: p.runs,
    errors: errorCount(p.outcomes),
    timeouts: timeoutCount(p.outcomes),
    p50: p.runs > 0 ? p.wall_ms.p50 : null,
    p95: p.runs > 0 ? p.wall_ms.p95 : null,
    max: p.runs > 0 ? p.wall_ms.max : null,
  }));
}

/** How a step's start is printed on an axis: a time, or a date for days. */
export function tickFormat(step: MetricsStep): Intl.DateTimeFormatOptions {
  switch (step) {
    case "1m":
    case "30m":
      return { hour: "2-digit", minute: "2-digit" };
    case "1h":
      return { weekday: "short", hour: "2-digit", minute: "2-digit" };
    case "1d":
      return { month: "short", day: "numeric", timeZone: "UTC" };
  }
}

/** A script kind as the dashboard names it. */
export function kindLabel(kind: string): string {
  switch (kind) {
    case "xrpc_query":
      return "XRPC query";
    case "xrpc_procedure":
      return "XRPC procedure";
    case "record_event":
      return "Record hook";
    case "label":
      return "Label hook";
    case "job":
      return "Job";
    default:
      return kind.replace(/_/g, " ");
  }
}

/** The smallest of 1, 2, 2.5 and 5 times a power of ten at or above `v`. */
export function niceCeil(v: number): number {
  if (!(v > 0)) return 0;
  const power = 10 ** Math.floor(Math.log10(v));
  for (const step of [1, 2, 2.5, 5, 10]) {
    const candidate = step * power;
    if (candidate >= v * (1 - 1e-9)) return candidate;
  }
  return 10 * power;
}

/**
 * The top of the latency axis: three times the highest p95, or the highest
 * max if that is lower, rounded up. Scaling to max would let one slow run
 * flatten p50 and p95 against the floor; max then runs off the top, and its
 * value stays in the tooltip. Null leaves the axis to the chart.
 */
export function latencyAxisTop(points: ChartPoint[]): number | null {
  const p95 = Math.max(0, ...points.map((p) => p.p95 ?? 0));
  if (p95 === 0) return null;
  const max = Math.max(p95, ...points.map((p) => p.max ?? 0));
  return niceCeil(Math.min(max, p95 * 3));
}

export interface LatencyPoint extends ChartPoint {
  /** Max where it fits under the axis, else null: drawn as the dashed line. */
  maxShown: number | null;
  /** The axis top where max runs past it, else null: drawn along the edge. */
  maxOver: number | null;
  /** An off-scale step with no off-scale neighbour, which a line cannot show. */
  maxOverAlone: boolean;
}

/**
 * Splits max at the latency axis's top, so a max above it draws as a solid
 * line along the top edge rather than as fragments of a line that runs off.
 */
export function withMaxOverflow(
  points: ChartPoint[],
  top: number | null,
): LatencyPoint[] {
  const over = points.map((p) => top != null && p.max != null && p.max > top);
  return points.map((p, i) => ({
    ...p,
    maxShown: over[i] ? null : p.max,
    maxOver: over[i] ? top : null,
    maxOverAlone: over[i] && !over[i - 1] && !over[i + 1],
  }));
}
