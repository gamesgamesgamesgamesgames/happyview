// Relative imports, not `@/`: the unit tests run without the path alias.
import { getLexiconRecordCount } from "./api";
import { parseTriggerId } from "../types/scripts";

/**
 * The search param a page is opened with right after a record script is
 * added: the collection the script runs on. The save leaves the page with a
 * full page load, so a toast wouldn't survive it; the param does.
 */
export const ADDED_PARAM = "added";

/**
 * The collection a record script runs on, or null for any other trigger:
 * queries, procedures, jobs and labels have no records waiting for them.
 */
export function addedCollection(triggerId: string): string | null {
  const parsed = parseTriggerId(triggerId);
  return parsed && parsed.kind.startsWith("record.") ? parsed.suffix : null;
}

/**
 * Where to go after adding a script: `href`, carrying the script's
 * collection in {@link ADDED_PARAM} when it is a record script.
 */
export function withAddedScript(href: string, triggerId: string): string {
  const collection = addedCollection(triggerId);
  if (!collection) return href;
  const sep = href.includes("?") ? "&" : "?";
  return `${href}${sep}${ADDED_PARAM}=${encodeURIComponent(collection)}`;
}

/**
 * Whether records a new script hasn't run over may already be here: some
 * are, or the count couldn't be read. Only a count of 0 says there are none.
 */
export function mayHaveRecords(count: number | null): boolean {
  return count !== 0;
}

/**
 * Whether to suggest a backfill for a record script just added to
 * `collection`. Asks for the collection's record count, which is a
 * COUNT(*), so this is for right after a script is added, not every view.
 * A count that can't be fetched is unknown, and suggests one too.
 */
export async function suggestBackfill(collection: string): Promise<boolean> {
  try {
    const { count } = await getLexiconRecordCount(collection);
    return mayHaveRecords(count);
  } catch {
    return true;
  }
}

/** The Backfills page with its New backfill form open on `collection`. */
export function backfillHref(collection: string): string {
  return `/dashboard/backfill?collection=${encodeURIComponent(collection)}`;
}
