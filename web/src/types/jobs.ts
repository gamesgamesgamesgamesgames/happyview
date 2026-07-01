export interface Job {
  id: string;
  job_type: string;
  status: string;
  input: Record<string, unknown>;
  progress: Record<string, unknown>;
  result: Record<string, unknown> | null;
  error: string | null;
  created_by: string;
  started_at: string | null;
  completed_at: string | null;
  created_at: string;
}

export interface JobsListResponse {
  jobs: Job[];
  cursor: string | null;
}
