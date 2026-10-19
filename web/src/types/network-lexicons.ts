export interface NetworkLexiconSummary {
  nsid: string;
  authority_did: string;
  target_collection: string | null;
  last_fetched_at: string | null;
  created_at: string;
  declared: boolean;
}
