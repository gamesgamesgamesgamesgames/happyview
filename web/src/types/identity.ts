export interface ResolvedIdentity {
  did: string
  /** Null unless the handle is confirmed in both directions. */
  handle: string | null
}
