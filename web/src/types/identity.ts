export interface ResolvedIdentity {
  did: string
  /** Null unless the handle is confirmed in both directions. */
  handle: string | null
  /** Present only when requested with `profile`. */
  display_name?: string
  /** Avatar image URL; present only when requested with `profile`. */
  avatar?: string
}
