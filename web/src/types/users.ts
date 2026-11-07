export interface UserSummary {
  id: string;
  did: string;
  is_super: boolean;
  permissions: string[];
  created_at: string;
  last_used_at: string | null;
}
