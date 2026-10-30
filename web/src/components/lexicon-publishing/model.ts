import { isValidNsid } from "@happyview/nsid";

import type {
  PublishableLexicon,
  PublishRule,
  PublishStatus,
  PublishTarget,
} from "@/types/lexicon-publishing";

// Mirrors src/lexicon_publishing/pattern.rs, so the dialog can describe a
// pattern before the server sees it. The server still decides.

export type ParsedPattern =
  | { kind: "all" }
  | { kind: "prefix"; base: string }
  | { kind: "exact"; nsid: string };

const SEGMENT = /^[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?$/;

export function parsePattern(
  raw: string,
): { ok: true; pattern: ParsedPattern } | { ok: false; error: string } {
  const value = raw.trim();
  if (!value) return { ok: false, error: "Enter a pattern" };
  if (value === "*") return { ok: true, pattern: { kind: "all" } };
  if (value.endsWith(".*")) {
    const base = value.slice(0, -2).toLowerCase();
    if (!base) return { ok: false, error: "Add a name before .*" };
    for (const segment of base.split(".")) {
      if (!segment) return { ok: false, error: "A segment is empty" };
      if (!SEGMENT.test(segment)) {
        return {
          ok: false,
          error: `"${segment}" takes only letters, digits and inner hyphens`,
        };
      }
    }
    return { ok: true, pattern: { kind: "prefix", base } };
  }
  if (value.includes("*")) {
    return {
      ok: false,
      error: "* works alone, or as a trailing .* after a prefix",
    };
  }
  if (!isValidNsid(value)) {
    return {
      ok: false,
      error: "Not an NSID. Use a full NSID, a prefix ending in .*, or *",
    };
  }
  return { ok: true, pattern: { kind: "exact", nsid: value } };
}

export function matches(pattern: ParsedPattern, nsid: string): boolean {
  switch (pattern.kind) {
    case "all":
      return true;
    case "prefix":
      return nsid.toLowerCase().startsWith(`${pattern.base}.`);
    case "exact":
      return pattern.nsid === nsid;
  }
}

/** Higher wins, as on the server: exact, then the longer prefix, then `*`. */
export function specificity(pattern: ParsedPattern): [number, number] {
  switch (pattern.kind) {
    case "all":
      return [0, 0];
    case "prefix":
      return [1, pattern.base.split(".").length];
    case "exact":
      return [2, pattern.nsid.split(".").length];
  }
}

export function moreSpecific(a: ParsedPattern, b: ParsedPattern): boolean {
  const [ak, al] = specificity(a);
  const [bk, bl] = specificity(b);
  return ak > bk || (ak === bk && al > bl);
}

export function describePattern(pattern: ParsedPattern): string {
  switch (pattern.kind) {
    case "all":
      return "Covers every lexicon";
    case "prefix":
      return `Covers every lexicon under ${pattern.base}`;
    case "exact":
      return `Covers only ${pattern.nsid}`;
  }
}

export function describeStoredPattern(stored: string): string {
  const parsed = parsePattern(stored);
  return parsed.ok ? describePattern(parsed.pattern) : stored;
}

/** The DNS authority of an NSID: its domain segments, reversed. */
export function nsidAuthority(nsid: string): string {
  return nsid.split(".").slice(0, -1).reverse().join(".").toLowerCase();
}

/** Statuses a press of Publish would change. */
export const PENDING_STATUSES: readonly PublishStatus[] = [
  "unpublished",
  "outdated",
  "moved",
];

export function isPending(lexicon: PublishableLexicon): boolean {
  return PENDING_STATUSES.includes(lexicon.status);
}

/** A failed attempt newer than the last success. */
export function lastFailure(lexicon: PublishableLexicon): string | null {
  const p = lexicon.publication;
  if (!p?.last_error) return null;
  if (!p.published_at || !p.last_attempt_at) return p.last_error;
  // Offsets and fractional digits vary, so the strings don't sort as times.
  return Date.parse(p.last_attempt_at) > Date.parse(p.published_at)
    ? p.last_error
    : null;
}

/**
 * A linked repo the server described without naming it, because the viewer
 * lacks `linked-repos:read`. A removed one still carries its reason.
 */
export function isRedacted(target: PublishTarget): boolean {
  return (
    target.kind === "linked_repo" &&
    !target.linked_repo_id &&
    !target.did &&
    !target.handle &&
    !target.unavailable_reason
  );
}

export function targetName(target: PublishTarget): string {
  if (target.kind === "instance") return "Instance account";
  if (isRedacted(target)) return "A linked repo";
  return target.handle ?? target.did ?? "Linked repo";
}

/** Why a destination can't publish, as a sentence. */
export function unavailableSentence(
  target: PublishTarget | null | undefined,
): string {
  if (target && isRedacted(target)) {
    return "Its linked repo can't be published to right now.";
  }
  return asSentence(target?.unavailable_reason);
}

/** Destinations a viewer may pick for a rule; the server refuses the rest. */
export function choosableTargets(
  targets: PublishTarget[],
  canSeeLinkedRepos: boolean,
): PublishTarget[] {
  return targets.filter((t) => t.kind === "instance" || canSeeLinkedRepos);
}

/** True when a target has no name of its own and is shown by its DID. */
export function nameIsDid(target: PublishTarget): boolean {
  return target.kind !== "instance" && !target.handle && Boolean(target.did);
}

export function targetKey(target: {
  kind: string;
  linked_repo_id: string | null;
}): string {
  return target.kind === "instance"
    ? "instance"
    : `linked_repo:${target.linked_repo_id ?? ""}`;
}

export function ruleTargetKey(rule: PublishRule): string {
  return targetKey({
    kind: rule.target_kind,
    linked_repo_id: rule.linked_repo_id,
  });
}

/** The account DNS should name for this lexicon, when there is one. */
export function expectedDid(lexicon: PublishableLexicon): string | null {
  if (lexicon.target?.available && lexicon.target.did)
    return lexicon.target.did;
  return lexicon.publication?.did ?? null;
}

/** Server reasons are clauses ("the linked repo ..."); shown alone they read as sentences. */
export function asSentence(reason: string | null | undefined): string {
  if (!reason) return "";
  return reason.charAt(0).toUpperCase() + reason.slice(1);
}
