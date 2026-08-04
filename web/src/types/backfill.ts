export interface BackfillJob {
  id: string
  collection: string | null
  did: string | null
  status: string
  stage: string
  total_repos: number | null
  resolved_repos: number | null
  processed_repos: number | null
  total_records: number | null
  error: string | null
  started_at: string | null
  completed_at: string | null
  created_at: string
  error_counts?: Record<string, number>
}

export interface BackfillRepoEntry {
  did: string
  pds_endpoint: string | null
  status: string
  records_fetched: number
}

export interface BackfillReposResponse {
  repos: BackfillRepoEntry[]
  cursor: string | null
}

export interface PdsSummaryEntry {
  pds_endpoint: string
  total_repos: number
  completed_repos: number
  total_records: number
}

export interface PdsSummaryResponse {
  pds_endpoints: PdsSummaryEntry[]
}

export interface BackfillEvent {
  type: string
  job_id: string
  did?: string
  pds_endpoint?: string
  records_fetched?: number
  total_repos?: number | null
  resolved_repos?: number | null
  processed_repos?: number | null
  total_records?: number | null
  stage?: string
  status?: string
  error?: string | null
  error_counts?: Record<string, number>
}

export interface BlueskyProfile {
  did: string
  handle: string
  displayName?: string
  avatar?: string
}

export interface BackfillErrorEntry {
  did: string
  collection: string | null
  phase: string
  kind: string
  message: string
  attempts: number
  last_at: string
}

// One kind's exact total, carrying its own retryability. `retryable` is
// served here rather than duplicated as a client-side kind set — see
// `BackfillErrorCount` in `src/admin/types.rs` for why.
export interface BackfillErrorCount {
  kind: string
  count: number
  retryable: boolean
}

export interface BackfillErrorsResponse {
  errors: BackfillErrorEntry[]
  cursor: string | null
  counts: BackfillErrorCount[]
  capped: boolean
  cap: number
}
