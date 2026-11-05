export interface EventLogEntry {
  id: string;
  event_type: string;
  severity: string;
  actor_did: string | null;
  subject: string | null;
  detail: Record<string, unknown>;
  created_at: string;
}

export interface EventsListResponse {
  events: EventLogEntry[];
  cursor: string | null;
}
