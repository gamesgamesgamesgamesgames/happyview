import type { BackfillJob, BackfillSource } from "@/types/backfill";

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
      label: "Ran the create hook over stored records",
      done,
      total: done,
      estimated: false,
    };
  }
  return {
    label: "Running the create hook over stored records",
    done,
    total: job.total_repos,
    estimated: true,
  };
}
