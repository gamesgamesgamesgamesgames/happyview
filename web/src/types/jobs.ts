export interface Job {
  id: string
  job_type: string
  status: string
  input: unknown
  progress: unknown
  result: unknown | null
  error: string | null
  created_by: string
  inherit_auth: boolean
  started_at: string | null
  completed_at: string | null
  created_at: string
}

export interface JobsListResponse {
  jobs: Job[]
  cursor: string | null
}
