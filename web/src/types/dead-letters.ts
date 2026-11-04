export interface DeadLetterSummary {
  id: string;
  lexicon_id: string;
  uri: string;
  did: string;
  collection: string;
  rkey: string;
  action: string;
  error: string;
  attempts: number;
  created_at: string;
  resolved_at: string | null;
}

export interface DeadLetterDetail extends DeadLetterSummary {
  record: Record<string, unknown> | null;
}

export interface DeadLettersListResponse {
  dead_letters: DeadLetterSummary[];
  cursor: string | null;
}

export interface DeadLetterCountResponse {
  count: number;
}

export interface BulkActionResponse {
  ok: boolean;
}

/** Retry and reindex answer for each dead letter they tried. */
export interface BulkItemsResponse extends BulkActionResponse {
  succeeded: number;
  failed: number;
  results: { id: string; ok: boolean; error?: string }[];
}
