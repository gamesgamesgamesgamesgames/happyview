export type PublishTargetKind = "instance" | "linked_repo"

export interface PublishTarget {
  kind: PublishTargetKind
  linked_repo_id: string | null
  did: string | null
  handle: string | null
  available: boolean
  unavailable_reason: string | null
  can_unpublish: boolean
}

export interface PublishRule {
  id: string
  /** `*`, `com.example.*`, or an exact NSID, as stored after normalisation. */
  pattern: string
  target_kind: PublishTargetKind
  /** `null` once the linked repo it named has been removed. */
  linked_repo_id: string | null
  auto_publish: boolean
  created_by: string
  created_at: string
  updated_at: string
  target: PublishTarget
}

export type PublishStatus =
  | "no_rule"
  | "target_unavailable"
  | "unpublished"
  | "withdrawn"
  | "moved"
  | "outdated"
  | "current"

export interface Publication {
  nsid: string
  /** `null` when only failed attempts exist. */
  did: string | null
  uri: string | null
  cid: string | null
  published_at: string | null
  last_error: string | null
  last_attempt_at: string | null
  /** Set by an explicit unpublish; auto-publish skips the lexicon until it is published by hand. */
  withdrawn_at: string | null
}

export interface PublishableLexicon {
  nsid: string
  layer: "manual" | "project"
  rule_id: string | null
  rule_pattern: string | null
  auto_publish: boolean
  target: PublishTarget | null
  status: PublishStatus
  publication: Publication | null
}

export interface LexiconPublishingOverview {
  targets: PublishTarget[]
  /** Most specific first: the order in which they win a match. */
  rules: PublishRule[]
  lexicons: PublishableLexicon[]
}

export interface LexiconDnsEntry {
  authority: string
  record_name: string
  /** `null` when no `_lexicon` record exists. */
  did: string | null
  error: string | null
}

export interface CreatePublishRuleBody {
  pattern: string
  target_kind: PublishTargetKind
  linked_repo_id?: string
  auto_publish?: boolean
}

export interface UpdatePublishRuleBody {
  target_kind?: PublishTargetKind
  linked_repo_id?: string | null
  auto_publish?: boolean
}

export interface PublishOutcome {
  nsid: string
  ok: boolean
  uri: string | null
  cid: string | null
  error: string | null
}
