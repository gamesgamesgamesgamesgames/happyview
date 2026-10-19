export interface LexiconSummary {
  id: string;
  revision: number;
  lexicon_type: string;
  backfill: boolean;
  action: string | null;
  target_collection: string | null;
  source: string;
  authority_did: string | null;
  last_fetched_at: string | null;
  /** `null` for a project lexicon, which has no stored row. */
  created_at: string | null;
  updated_at: string | null;
  /** `"project"` when a file in the instance's project defines this lexicon. */
  layer: "project" | "manual" | "network";
  /** Project-relative file path, for a project lexicon only. */
  path: string | null;
  /** A stored lexicon sits under this project file and answers again if it is removed. */
  shadows_stored: boolean;
  /** Listed in happyview.toml, which would restore it after a delete. */
  declared: boolean;
  token_cost: number | null;
}

export interface LexiconDetail extends LexiconSummary {
  lexicon_json: Record<string, unknown>;
}
