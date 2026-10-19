export interface LabelerSummary {
  did: string;
  status: string;
  cursor: number | null;
  created_at: string;
  updated_at: string;
  declared: boolean;
}
