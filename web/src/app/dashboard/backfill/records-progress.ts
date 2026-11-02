import type {
  BackfillCollection,
  BackfillEvent,
  BackfillJob,
  BackfillSource,
  HookStats,
} from "@/types/backfill";

/**
 * The hook stats a `hook_stats` event carries, or null for one missing its
 * counts. The event puts them beside the job id rather than under a key.
 */
export function hookStatsOf(e: BackfillEvent): HookStats | null {
  if (e.done == null || e.outcomes == null || e.stages == null) return null;
  return {
    done: e.done,
    failed: e.failed ?? 0,
    timeouts: e.timeouts ?? 0,
    dead_lettered: e.dead_lettered ?? 0,
    skipped: e.skipped ?? 0,
    outcomes: e.outcomes,
    rate_per_s: e.rate_per_s ?? 0,
    avg_rate_per_s: e.avg_rate_per_s ?? 0,
    eta_s: e.eta_s ?? null,
    record_ms_p50: e.record_ms_p50 ?? null,
    script_ms_p50: e.script_ms_p50 ?? null,
    script_share: e.script_share ?? null,
    stages: e.stages,
  };
}

/** The sources HappyView backfills itself; any other names an executor. */
const OWN_SOURCES: readonly BackfillSource[] = ["auto", "replay", "crawl"];

/**
 * Whether a record source's executor ran the job rather than HappyView's own
 * crawl or replay, as a network store's store pass does. Such a job has no
 * crawl phases, and its stage stays `pending` while it runs.
 */
export function runByExecutor(source: string): boolean {
  return !(OWN_SOURCES as readonly string[]).includes(source);
}

/** What a running job's status badge says. */
export function runningLabel(job: {
  stage: BackfillJob["stage"];
  /** Typed as HappyView's own sources, but an executor's job carries its name. */
  source: string;
}): string {
  if (job.stage === "pending") {
    return runByExecutor(job.source) ? "running" : "starting";
  }
  return job.stage.replace(/_/g, " ");
}

export interface RecordsCount {
  done: number;
  /** Unknown when null or absent: the count then stands alone. */
  total?: number | null;
  /** What is counted, plural. */
  noun?: string;
  /** The total is an estimate, said as "about". */
  estimated?: boolean;
}

/**
 * A count's pieces in reading order: `done`, then `total` (null when the
 * count stands alone), said with "about" when `estimated`, then the noun.
 */
export interface RecordsCountParts {
  done: number;
  total: number | null;
  estimated: boolean;
  noun: string;
}

/**
 * "1,200 of about 5,500 records", or "1,200 records" with no total or once
 * the count has reached a known one.
 */
export function recordsCountParts({
  done,
  total,
  noun = "records",
  estimated = false,
}: RecordsCount): RecordsCountParts {
  if (total == null || (total === done && !estimated)) {
    return { done, total: null, estimated: false, noun };
  }
  return { done, total, estimated, noun };
}

/** {@link recordsCountParts} as one string. */
export function recordsCountText(count: RecordsCount): string {
  const { done, total, estimated, noun } = recordsCountParts(count);
  if (total == null) return `${done.toLocaleString()} ${noun}`;
  return `${done.toLocaleString()} of ${estimated ? "about " : ""}${total.toLocaleString()} ${noun}`;
}

/** How full the bar is, 0 to 1, or null with no total to measure against. */
export function recordsFraction({ done, total }: RecordsCount): number | null {
  if (total == null || total <= 0) return null;
  return Math.min(1, Math.max(0, done / total));
}

/** "about 2 h 55 m", "about 4 m", "under a minute". */
export function etaText(seconds: number): string {
  if (seconds < 60) return "under a minute";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `about ${minutes} m`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return rest === 0 ? `about ${hours} h` : `about ${hours} h ${rest} m`;
}

/** "2%", "0.4%", "<0.1%" of a 0-to-1 share. */
export function shareText(share: number): string {
  const percent = share * 100;
  if (percent >= 10) return `${Math.round(percent)}%`;
  if (percent >= 0.1) return `${percent.toFixed(1).replace(/\.0$/, "")}%`;
  return percent > 0 ? "<0.1%" : "0%";
}

export interface HookDetail {
  /** Rate, time left and anything that went wrong, in reading order. */
  progress: string[];
  /** How much of a record's time its record script spent, when known. */
  scriptShare: string | null;
  /**
   * How many records went to Dead Letters, as a count to say beside a link
   * there, or null for none.
   */
  deadLettered: string | null;
}

/**
 * What a job's hook stats say beside its count. A finished job has no rate
 * to keep up, so it reads its average and no time left.
 */
export function hookDetail(stats: HookStats, finished: boolean): HookDetail {
  const progress: string[] = [];
  const rate = finished ? stats.avg_rate_per_s : stats.rate_per_s;
  if (rate > 0) {
    const perSecond =
      rate >= 10 ? Math.round(rate).toLocaleString() : rate.toFixed(1);
    progress.push(finished ? `averaged ${perSecond}/s` : `${perSecond}/s`);
  }
  if (!finished && stats.eta_s != null) {
    progress.push(`${etaText(stats.eta_s)} left`);
  }
  // Timeouts are among the failures, and a dead-lettered record failed too.
  if (stats.failed > 0) {
    const timedOut =
      stats.timeouts > 0
        ? ` (${stats.timeouts.toLocaleString()} timed out)`
        : "";
    progress.push(`${stats.failed.toLocaleString()} failed${timedOut}`);
  }
  return {
    progress,
    scriptShare:
      stats.script_share == null
        ? null
        : `${shareText(stats.script_share)} of time in scripts`,
    deadLettered:
      stats.dead_lettered > 0 ? stats.dead_lettered.toLocaleString() : null,
  };
}

/**
 * What an executor-run job's progress row says. The total is the estimate
 * the store took when the pass began, and the pass leaves out records the
 * live feed delivers itself, so a finished pass routinely hooks a little
 * fewer than it: once completed, the row counts what was done and fills the
 * bar rather than reading as a pass that stopped short.
 */
export function executorProgress(
  job: Pick<
    BackfillJob,
    "status" | "stage" | "processed_repos" | "total_repos" | "total_records"
  >,
): { label: string } & RecordsCount {
  const done = job.processed_repos ?? 0;
  if (waitingForCrawl(job)) {
    // Records the crawl stores reach this instance as they are stored, and
    // their record scripts run then, so the count grows with no total.
    return {
      label: "Waiting for the Network store's crawl",
      done,
      total: null,
      noun: "records arrived so far",
    };
  }
  if (job.status === "completed") {
    // A resumed pass can report fewer than it wrote over all its runs, so a
    // finished one counts whichever is more.
    const ran = Math.max(done, job.total_records ?? 0);
    return {
      label: "Ran record scripts",
      done: ran,
      total: ran,
      estimated: false,
    };
  }
  return {
    // Only a pass that is still going is "running"; a failed, cancelled or
    // paused one keeps its count under a plain label.
    label:
      job.status === "running" || job.status === "pausing"
        ? "Running record scripts"
        : "Record scripts",
    done,
    total: job.total_repos,
    estimated: true,
  };
}

/**
 * Whether an executor-run job ended with nothing counted against it: a store
 * pass that found no record script or nothing in the store, one that failed
 * before it read anything, or the row the store keeps for a collection it has
 * seen. Its counts are 0, or unset on that row, and no hook ran or failed
 * under it, so
 * it has no progress to show; its note or error says why. Records the feed
 * delivered and hooked are not this job's to count. Judged by the counts, not
 * the note's wording.
 */
export function executorRanNothing(
  job: Pick<BackfillJob, "status" | "processed_repos" | "total_records">,
  hooks: Pick<
    HookStats,
    "done" | "failed" | "timeouts" | "dead_lettered"
  > | null,
): boolean {
  // Hooks that failed reached no decision, so `done` leaves them out; the
  // row is where their counts show.
  const hooked =
    (hooks?.done ?? 0) +
    (hooks?.failed ?? 0) +
    (hooks?.timeouts ?? 0) +
    (hooks?.dead_lettered ?? 0);
  return (
    ["completed", "failed", "cancelled"].includes(job.status) &&
    (job.processed_repos ?? 0) === 0 &&
    (job.total_records ?? 0) === 0 &&
    hooked === 0
  );
}

/** The stage an executor's job holds while its source is still being filled. */
export const WAITING_FOR_CRAWL = "waiting_for_crawl";

/** Whether a job is running and waiting for the Network store to crawl. */
export function waitingForCrawl(
  job: Pick<BackfillJob, "status" | "stage">,
): boolean {
  return (
    job.stage === WAITING_FOR_CRAWL &&
    (job.status === "running" || job.status === "pausing")
  );
}

type CollectionSummary = NonNullable<BackfillCollection["summary"]>;

const SUMMARY_STATE_LABELS: Record<
  NonNullable<CollectionSummary["state"]>,
  string
> = {
  queued: "crawl queued",
  running: "crawling",
  complete: "crawl complete",
  failed: "crawl failed",
};

/**
 * What a collection summary says beside its message, in reading order: the
 * records held, how the crawl stands, and when it is next tried.
 */
export function summaryParts(
  summary: Pick<CollectionSummary, "records" | "state" | "retry_at">,
  formatTime: (iso: string) => string = (iso) => new Date(iso).toLocaleString(),
  now: number = Date.now(),
): string[] {
  const parts: string[] = [];
  if (summary.records != null) {
    parts.push(`${summary.records.toLocaleString()} records`);
  }
  if (summary.state) parts.push(SUMMARY_STATE_LABELS[summary.state]);
  if (summary.retry_at) {
    // A time already gone by is due, not upcoming: the store tries it on its
    // next pass.
    parts.push(
      Date.parse(summary.retry_at) <= now
        ? "next crawl attempt due"
        : `next crawl attempt at ${formatTime(summary.retry_at)}`,
    );
  }
  return parts;
}
