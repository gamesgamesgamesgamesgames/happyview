export interface AdminSpace {
  id: string;
  uri: string;
  did: string;
  authority_did: string;
  creator_did: string;
  type: string;
  skey: string;
  display_name: string | null;
  description: string | null;
  read_policy: { $type: string };
  write_policy: { $type: string };
  app_access: { $type: string; clients?: string[] };
  config: Record<string, unknown>;
  revision: string | null;
  created_at: string;
  updated_at: string;
}

export interface AdminListSpacesResponse {
  spaces: AdminSpace[];
  cursor?: string;
}

export interface AdminSpaceMember {
  did: string;
  read: boolean;
  write: boolean;
}

export interface AdminSpaceCollection {
  collection: string;
  count: number;
}

export interface AdminSpaceDetail {
  space: AdminSpace;
  members: AdminSpaceMember[];
  collections: AdminSpaceCollection[];
}

export interface AdminSpaceRecord {
  uri: string;
  did: string;
  collection: string;
  rkey: string;
  cid: string;
  indexed_at: string;
  record: Record<string, unknown>;
}

export interface AdminListSpaceRecordsResponse {
  records: AdminSpaceRecord[];
  cursor?: string;
}

export type GrantScope = "space" | "account";

export interface AccessGrant {
  id: string;
  user_id: string;
  user_did: string;
  scope: GrantScope;
  target: string;
  reason: string;
  created_at: string;
  expires_at: string;
  revoked_at: string | null;
  revoked_by: string | null;
}

export interface InspectorStatus {
  enabled: boolean;
  default_grant_minutes: number;
  max_grant_minutes: number;
}

export interface AdminAccountSpace {
  space: AdminSpace;
  record_count: number;
}

export interface AdminAccountRecord extends AdminSpaceRecord {
  space_id: string;
  space_uri: string;
}

/** A stored read or write rule, as `/admin/spaces/retired-policies` sends it. */
export type SpaceRule =
  | { $type: "com.atproto.simplespace.defs#publicPolicy" }
  | { $type: "com.atproto.simplespace.defs#memberListPolicy" }
  | {
      $type: "com.atproto.simplespace.defs#managingAppPolicy";
      managingApp: string;
    };

/** A space whose rules predate policies and match none. */
export interface RetiredSpace {
  id: string;
  uri: string;
  spaceType: string;
  skey: string;
  creatorDid: string;
  readPolicy: SpaceRule;
  writePolicy: SpaceRule;
  appAccess: { $type: string; allowed?: string[] };
}

/** `spaces` is present only while the space inspector is on. */
export interface RetiredSpacesResponse {
  count: number;
  spaces?: RetiredSpace[];
}
