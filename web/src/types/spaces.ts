export interface AdminSpace {
  id: string
  uri: string
  did: string
  authority_did: string
  creator_did: string
  type: string
  skey: string
  display_name: string | null
  description: string | null
  read_policy: { $type: string }
  write_policy: { $type: string }
  app_access: { $type: string; clients?: string[] }
  config: Record<string, unknown>
  revision: string | null
  created_at: string
  updated_at: string
}

export interface AdminListSpacesResponse {
  spaces: AdminSpace[]
  cursor?: string
}

export interface AdminSpaceMember {
  did: string
  read: boolean
  write: boolean
}

export interface AdminSpaceCollection {
  collection: string
  count: number
}

export interface AdminSpaceDetail {
  space: AdminSpace
  members: AdminSpaceMember[]
  /** Every DID with records in the space, members or not. */
  authors: string[]
  collections: AdminSpaceCollection[]
}

export interface AdminSpaceRecord {
  uri: string
  did: string
  collection: string
  rkey: string
  cid: string
  indexed_at: string
  record: Record<string, unknown>
}

export interface AdminListSpaceRecordsResponse {
  records: AdminSpaceRecord[]
  cursor?: string
}

export type GrantScope = "space" | "account"

export interface AccessGrant {
  id: string
  user_id: string
  user_did: string
  scope: GrantScope
  target: string
  reason: string
  created_at: string
  expires_at: string
  revoked_at: string | null
  revoked_by: string | null
}

export interface InspectorStatus {
  enabled: boolean
  default_grant_minutes: number
  max_grant_minutes: number
}

export interface AdminAccountSpace {
  space: AdminSpace
  record_count: number
}

export interface AdminAccountRecord extends AdminSpaceRecord {
  space_id: string
  space_uri: string
}
