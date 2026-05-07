export interface LexiconSummary {
  id: string
  revision: number
  lexicon_type: string
  backfill: boolean
  action: string | null
  target_collection: string | null
  source: string
  authority_did: string | null
  last_fetched_at: string | null
  created_at: string
  updated_at: string
  token_cost: number | null
}

export interface LexiconDetail extends LexiconSummary {
  lexicon_json: Record<string, unknown>
}
