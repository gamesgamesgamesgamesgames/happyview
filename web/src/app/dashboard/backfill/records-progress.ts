import type {
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
 * "1,200 of about 5,500 records", or "1,200 records" with no total or once
 * the count has reached a known one.
 */
export function recordsCountText({
  done,
  total,
  noun = "records",
  estimated = false,
}: RecordsCount): string {
  if (total == null || (total === done && !estimated)) {
    return `${done.toLocaleString()} ${noun}`;
  }
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
  /** How much of a record's time its hook's script spent, when known. */
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
  job: Pick<BackfillJob, "status" | "processed_repos" | "total_repos">,
): { label: string } & RecordsCount {
  const done = job.processed_repos ?? 0;
  if (job.status === "completed") {
    return {
      label: "Ran record scripts",
      done,
      total: done,
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
