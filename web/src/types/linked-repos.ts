export interface LinkedRepo {
  id: string
  did: string | null
  handle: string | null
  reason: string | null
  scopes: string
  status: "pending" | "active" | "needs_reauth"
  last_error: string | null
  last_refreshed_at: string | null
  authorized_at: string | null
  created_by: string
  created_at: string
}

export interface LinkedReposListResponse {
  linked_repos: LinkedRepo[]
}

export interface CreateLinkedRepoBody {
  handle?: string
  reason?: string
  scopes: string
}
