// Plain-language translations for AT Protocol OAuth scope strings, shown on
// the invitee-facing /link/start page. Mirrors the scope grammar
// `src/linked_repos/scope.rs` enforces server-side — the admin-facing grant
// builder (`components/linked-repos/scope-builder.tsx`) speaks the same
// grammar. Keep all three in sync when the grammar changes.

export interface ScopeDescription {
  /** The raw scope string, shown small and secondary for a technical reader. */
  raw: string;
  /**
   * What this scope lets the app *do*, as a heading a reader can scan. Scopes
   * sharing a heading are the same permission over different targets, so the
   * page groups on it rather than repeating the verb phrase per row.
   */
  heading: string;
  /**
   * What the heading's verb applies to — a collection NSID, a MIME pattern —
   * or `null` when the heading already says everything (`Upload files`).
   */
  target: string | null;
  /**
   * Plumbing every grant carries (`atproto`, `identity:*`, `rpc:*`) rather
   * than something the reader should weigh as a permission — callers should
   * render these quietly, not alongside the real capabilities.
   */
  quiet: boolean;
}

/** A heading and everything it applies to, in first-seen order. */
export interface ScopeGroup {
  heading: string;
  targets: string[];
}

function splitOnce(value: string, sep: string): [string, string | null] {
  const i = value.indexOf(sep);
  return i === -1 ? [value, null] : [value.slice(0, i), value.slice(i + 1)];
}

function capitalize(word: string): string {
  return word.length === 0 ? word : word[0].toUpperCase() + word.slice(1);
}

function joinActions(actions: string[]): string {
  const verbs = actions.map((action, i) => (i === 0 ? capitalize(action) : action));
  // "Create only" rather than a bare "Create": on a consent screen the useful
  // fact is as much what the app *can't* do afterwards as what it can.
  if (verbs.length <= 1) return verbs[0] ? `${verbs[0]} only` : "";
  if (verbs.length === 2) return `${verbs[0]} and ${verbs[1]}`;
  return `${verbs.slice(0, -1).join(", ")}, and ${verbs[verbs.length - 1]}`;
}

type Described = Pick<ScopeDescription, "heading" | "target">;

function describeRepoScope(rest: string): Described | null {
  const [collection, query] = splitOnce(rest, "?");
  const params = query === null ? null : new URLSearchParams(query);
  // Actions are repeated `action=` parameters read with `URLSearchParams`
  // semantics, not a comma-joined list — `?action=create,update` is a single
  // unknown action the authorization server rejects outright. No `?action=` at
  // all means every action, mirroring scope::allows_repo.
  const actions = params?.has("action")
    ? params.getAll("action").filter(Boolean)
    : ["create", "update", "delete"];
  // An `?action=` naming nothing is ungrammatical; the caller shows the raw
  // scope rather than claiming a permission it can't name.
  if (actions.length === 0) return null;
  return {
    heading: joinActions(actions),
    target: collection === "*" ? "every collection" : collection,
  };
}

function describeBlobScope(rest: string): Described {
  const [target] = splitOnce(rest, "?");
  const [type, subtype] = target.split("/");
  if (!type || !subtype) return { heading: "Upload files", target: rest };
  if (type === "*" && subtype === "*") return { heading: "Upload files", target: null };
  return { heading: "Upload files of type", target: subtype === "*" ? `${type}/*` : target };
}

function describeOtherScope(prefix: string, rest: string): Described {
  switch (prefix) {
    case "rpc":
      return { heading: "Call API methods", target: rest };
    case "identity":
      return { heading: "Access identity info", target: rest };
    case "account":
      return { heading: "Access account info", target: rest };
    default:
      return { heading: prefix, target: rest || null };
  }
}

/**
 * Translate one OAuth scope string into plain language for a non-technical
 * reader. `quiet: true` marks scopes that are plumbing every grant needs
 * (`atproto`, `identity:*`, `rpc:*`) rather than a permission worth weighing.
 */
export function describeScope(scope: string): ScopeDescription {
  const quietly = (heading: string): ScopeDescription => ({
    raw: scope,
    heading,
    target: null,
    quiet: true,
  });

  if (scope === "atproto") return quietly("Basic account access");

  const [prefix, rest] = splitOnce(scope, ":");
  if (rest === null) {
    // Doesn't match the `prefix:rest` grammar at all — show it verbatim
    // rather than guessing at a translation.
    return { raw: scope, heading: scope, target: null, quiet: false };
  }

  if ((prefix === "identity" || prefix === "rpc") && rest === "*") {
    return quietly("Basic account access");
  }

  const described =
    prefix === "repo"
      ? describeRepoScope(rest)
      : prefix === "blob"
        ? describeBlobScope(rest)
        : describeOtherScope(prefix, rest);

  // A repo scope we can't name falls back to the raw string as its own heading.
  return { raw: scope, ...(described ?? { heading: scope, target: null }), quiet: false };
}

/**
 * Collapse descriptions that grant the same thing into one heading each,
 * keeping first-seen order so the list is stable across renders.
 */
export function groupScopes(descriptions: ScopeDescription[]): ScopeGroup[] {
  const groups = new Map<string, ScopeGroup>();
  for (const { heading, target } of descriptions) {
    const group = groups.get(heading) ?? { heading, targets: [] };
    if (target !== null && !group.targets.includes(target)) group.targets.push(target);
    groups.set(heading, group);
  }
  return [...groups.values()];
}
