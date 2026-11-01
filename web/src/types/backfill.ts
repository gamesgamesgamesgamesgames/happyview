export type BackfillSource = "auto" | "replay" | "crawl";

export interface ReplayProgress {
  after_seq: number;
  through_seq: number | null;
  tip_seq: number | null;
  bytes: number;
  blocks_failed: number;
}

export interface BackfillJob {
  id: string;
  collection: string | null;
  did: string | null;
  scope: "network" | "dids";
  // One of HappyView's own sources, or the name of the executor that ran the
  // job, such as a network store's `store-pass`.
  source: BackfillSource | (string & {});
  // Set for jobs that read the Jetstream archive; null for crawl jobs.
  replay: ReplayProgress | null;
  status: string;
  stage: string;
  total_repos: number | null;
  resolved_repos: number | null;
  processed_repos: number | null;
  total_records: number | null;
  error: string | null;
  started_at: string | null;
  completed_at: string | null;
  created_at: string;
  error_counts?: Record<string, number>;
  // False while a network job is still listing repos from the relay; until
  // then `total_repos` counts the repos discovered so far.
  discovery_complete?: boolean;
  // Set for jobs on the bounded queue: how many recent completions the
  // `fetched` repo list keeps. Null for jobs created before it.
  recent_completions_limit?: number | null;
  // What the job's executor said about it, for a collection backfilled by
  // the record source rather than a crawl or a replay.
  note?: string | null;
  // What the job's records have cost in hooks so far, while this instance
  // holds it; null before its first record and an hour after its last.
  hooks?: HookStats | null;
  // The same, kept with the job once it completed, failed or was cancelled.
  hook_stats?: HookStats | null;
}

// A job's hook stats: `GET /admin/backfill/status` and the `hook_stats`
// event. Times are milliseconds; shares are 0 to 1.
export interface HookStats {
  done: number;
  failed: number;
  timeouts: number;
  dead_lettered: number;
  skipped: number;
  outcomes: Partial<
    Record<"proceed" | "replace" | "skip" | "cid_mismatch", number>
  >;
  rate_per_s: number;
  avg_rate_per_s: number;
  eta_s: number | null;
  record_ms_p50: number | null;
  script_ms_p50: number | null;
  script_share: number | null;
  stages: Record<string, { n: number; ms_mean: number | null }>;
}

// What `GET /admin/backfill/collections/{nsid}` reports: the executor that
// backfills the collection, if any, and what it says about it now.
export interface BackfillCollection {
  collection: string;
  executor: string | null;
  summary: {
    message: string;
    records: number | null;
    state: "running" | "complete" | "failed" | null;
  } | null;
}

export interface BackfillRepoEntry {
  did: string;
  pds_endpoint: string | null;
  status: string;
  records_fetched: number;
}

export interface BackfillReposResponse {
  repos: BackfillRepoEntry[];
  cursor: string | null;
}

export interface PdsSummaryEntry {
  pds_endpoint: string;
  total_repos: number;
  completed_repos: number;
  total_records: number;
}

export interface PdsSummaryResponse {
  pds_endpoints: PdsSummaryEntry[];
}

// `hook_stats` events carry a whole `HookStats` beside the job id.
export interface BackfillEvent extends Partial<HookStats> {
  type: string;
  job_id: string;
  did?: string;
  pds_endpoint?: string;
  records_fetched?: number;
  total_repos?: number | null;
  resolved_repos?: number | null;
  processed_repos?: number | null;
  total_records?: number | null;
  stage?: string;
  status?: string;
  error?: string | null;
  error_counts?: Record<string, number>;
  discovery_complete?: boolean;
  // `replay_progress` events
  through_seq?: number | null;
  tip_seq?: number | null;
  bytes?: number;
  blocks_failed?: number;
  // `job_note` events
  note?: string;
}

export interface BlueskyProfile {
  did: string;
  handle: string;
  displayName?: string;
  avatar?: string;
}

export interface BackfillErrorEntry {
  did: string;
  collection: string | null;
  phase: string;
  kind: string;
  message: string;
  attempts: number;
  last_at: string;
}

// One kind's exact total, carrying its own retryability. `retryable` is
// served here rather than duplicated as a client-side kind set — see
// `BackfillErrorCount` in `src/admin/types.rs` for why.
export interface BackfillErrorCount {
  kind: string;
  count: number;
  retryable: boolean;
}

export interface BackfillErrorsResponse {
  errors: BackfillErrorEntry[];
  cursor: string | null;
  counts: BackfillErrorCount[];
  capped: boolean;
  cap: number;
}
