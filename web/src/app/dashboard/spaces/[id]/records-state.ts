import type { AccessGrant, AdminSpaceRecord } from "@/types/spaces";

/** The filter value that means "no filter". */
export const ALL = "__all__";

export interface RecordFilters {
  collection: string;
  repo: string;
}

export interface GrantRecordsState {
  records: AdminSpaceRecord[];
  nextCursor: string | undefined;
  cursorStack: string[];
  viewRecord: AdminSpaceRecord | null;
  /** The filters to fetch with, or null when there is no grant to fetch under. */
  filters: RecordFilters | null;
}

/**
 * The records view a grant starts from. Records, pages and the open record
 * belong to the previous grant's scope, so they always reset. An account grant
 * fixes the author, and a space grant starts from every author rather than a
 * filter left over from a previous grant.
 */
export function stateForGrant(
  grant: Pick<AccessGrant, "scope" | "target"> | null | undefined,
  collection: string,
): GrantRecordsState {
  return {
    records: [],
    nextCursor: undefined,
    cursorStack: [],
    viewRecord: null,
    filters: !grant
      ? null
      : grant.scope === "account"
        ? { collection, repo: grant.target }
        : { collection, repo: ALL },
  };
}
