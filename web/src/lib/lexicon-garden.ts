// lexicon.garden indexes published lexicons across the network, so it can
// suggest NSIDs by prefix. HappyView still resolves the chosen NSID itself via
// DNS and the authority's PDS; this is only used for typeahead.
const LEXICON_GARDEN_URL = "https://lexicon.garden";

export interface LexiconSuggestion {
  nsid: string;
  did: string;
  uri: string;
  lexiconType?: string;
}

export const SUGGEST_MIN_QUERY_LENGTH = 2;
const SUGGEST_MAX_QUERY_LENGTH = 253;

export async function suggestLexicons(
  query: string,
  signal?: AbortSignal,
): Promise<LexiconSuggestion[]> {
  const params = new URLSearchParams({
    q: query.slice(0, SUGGEST_MAX_QUERY_LENGTH),
    limit: "20",
  });
  const response = await fetch(
    `${LEXICON_GARDEN_URL}/xrpc/garden.lexicon.suggest?${params}`,
    { signal },
  );
  if (!response.ok) {
    throw new Error(`lexicon.garden suggest failed: ${response.status}`);
  }
  const body = (await response.json()) as { suggestions: LexiconSuggestion[] };
  return body.suggestions;
}
