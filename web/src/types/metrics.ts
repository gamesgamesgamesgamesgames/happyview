/** The windows the metrics API reads over. */
export type MetricsWindow = "1h" | "24h" | "7d" | "30d";

/** The steps a script's series is cut into. */
export type MetricsStep = "1m" | "30m" | "1h" | "1d";

/** The script kinds a run is recorded under. */
export type ScriptMetricsKind =
  "xrpc_query" | "xrpc_procedure" | "record_event" | "label" | "job";

/**
 * How a run ended: `ok`, the interpreter's own failures, or the host's.
 * Kept open because the server may add one.
 */
export type RunOutcome =
  | "ok"
  | "syntax"
  | "runtime"
  | "missing_handle"
  | "timeout"
  | "memory"
  | "trap"
  | "no_interpreter"
  | "error";

/** Run counts by outcome; an outcome with no runs is absent. */
export type Outcomes = Record<string, number>;

/** Durations in milliseconds; null when nothing was measured. */
export interface Percentiles {
  p50: number | null;
  p95: number | null;
  p99: number | null;
  max: number | null;
  mean: number | null;
}

export interface HostImportStats {
  name: string;
  calls: number;
  ms_mean: number;
}

/** What a window says about one script, in the list and in its detail. */
export interface ScriptStats {
  runs: number;
  outcomes: Outcomes;
  wall_ms: Percentiles;
  guest_ms: Percentiles;
  prepare_ms_mean: number | null;
  instantiate_ms_mean: number | null;
  link_ms_mean: number | null;
  allocate_ms_mean: number | null;
  teardown_ms_mean: number | null;
  host_ms_mean: number | null;
  settle_ms_mean: number | null;
  /** Sorted by total time spent. */
  host_imports: HostImportStats[];
  memory_peak_bytes: { max: number | null; mean: number | null };
  /** Compiled script modules only. */
  fuel_mean: number | null;
  module_cache: { hits: number; misses: number };
}

export interface ScriptMetricsRow extends ScriptStats {
  trigger: string;
  kind: ScriptMetricsKind | string;
  layer: "project" | "dashboard";
}

interface WindowedResponse {
  window: MetricsWindow;
  enabled: boolean;
  /** Stored windows (7d, 30d) only: the retention in force, 0 for forever. */
  retention_days?: number;
}

export interface ScriptMetricsListResponse extends WindowedResponse {
  overflow: number;
  scripts: ScriptMetricsRow[];
}

export interface PointPercentiles {
  p50: number | null;
  p95: number | null;
  max: number | null;
}

/** One step of a script's series. A step with no runs has null latencies. */
export interface SeriesPoint {
  start: string;
  runs: number;
  outcomes: Outcomes;
  wall_ms: PointPercentiles;
  guest_ms: PointPercentiles;
  host_ms_mean: number | null;
  instantiate_ms_mean: number | null;
  link_ms_mean: number | null;
  allocate_ms_mean: number | null;
  teardown_ms_mean: number | null;
}

export interface ScriptSeriesResponse extends WindowedResponse, ScriptStats {
  trigger: string;
  kind: ScriptMetricsKind | string;
  layers: ("project" | "dashboard")[];
  step: MetricsStep;
  points: SeriesPoint[];
}

/** A stage's records and mean time; a count stage has a null mean. */
export interface StageStats {
  n: number;
  ms_mean: number | null;
}

export interface IngestRow {
  collection: string;
  action: string;
  records: number;
  records_per_s: number;
  record_ms: Percentiles;
  stages: Record<string, StageStats>;
  outcomes: Record<string, number>;
  failed: number;
  timeouts: number;
  dead_lettered: number;
  script_ms_mean: number | null;
  script_share: number | null;
  platform_share: number | null;
}

export interface IngestMetricsResponse extends WindowedResponse {
  overflow: number;
  scripts_table_reads: number;
  collections: IngestRow[];
}

export interface LiveMetricsResponse {
  enabled: boolean;
  active_runs: Partial<Record<ScriptMetricsKind, number>>;
  jobs: { pending: number; running: number };
  backfill_jobs: {
    job_id: string;
    idle_s: number;
    hooks: { done: number; rate_per_s: number } & Record<string, unknown>;
  }[];
  module_cache: { hits: number; misses: number; compiles: number };
}
