export interface NetworkLexiconSummary {
  nsid: string;
  authority_did: string;
  indexed: boolean;
  last_fetched_at: string | null;
  created_at: string;
  declared: boolean;
}
