const SEARCH_URL =
  "https://typeahead.waow.tech/xrpc/tech.waow.typeahead.searchActors"

export interface TypeaheadActor {
  did: string
  handle: string
  displayName?: string
  avatar?: string
}

/**
 * Suggest accounts for a partial handle or name.
 *
 * Suggestions are a convenience: every failure, including rate limiting,
 * resolves to an empty list so typing a full handle still works.
 */
export async function searchActors(
  query: string,
  signal: AbortSignal,
): Promise<TypeaheadActor[]> {
  const params = new URLSearchParams({ q: query, limit: "8" })
  try {
    const resp = await fetch(`${SEARCH_URL}?${params}`, {
      signal,
      headers: { "X-Client": window.location.host },
    })
    if (!resp.ok) return []
    const data = await resp.json()
    return Array.isArray(data.actors) ? (data.actors as TypeaheadActor[]) : []
  } catch {
    return []
  }
}
