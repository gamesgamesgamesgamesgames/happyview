// Plain-language translations for AT Protocol OAuth scope strings, shown on
// the invitee-facing /link/start page. Mirrors the scope grammar
// `src/linked_repos/scope.rs` enforces server-side — the admin-facing grant
// builder (`components/linked-repos/scope-builder.tsx`) speaks the same
// grammar. Keep all three in sync when the grammar changes.

export interface ScopeDescription {
  /** The raw scope string, shown small and secondary for a technical reader. */
  raw: string;
  /** Plain-language translation of what this scope grants. */
  text: string;
  /**
   * Plumbing every grant carries (`atproto`, `identity:*`, `rpc:*`) rather
   * than something the reader should weigh as a permission — callers should
   * render these quietly, not alongside the real capabilities.
   */
  quiet: boolean;
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
  if (verbs.length <= 1) return verbs[0] ?? "";
  if (verbs.length === 2) return `${verbs[0]} and ${verbs[1]}`;
  return `${verbs.slice(0, -1).join(", ")}, and ${verbs[verbs.length - 1]}`;
}

function describeRepoScope(rest: string): string {
  const [target, query] = splitOnce(rest, "?");
  // No `?action=` at all means every action is granted — mirrors
  // scope::allows_repo's "no query means all actions" rule.
  let actions = ["create", "update", "delete"];
  if (query?.startsWith("action=")) {
    actions = query.slice("action=".length).split(",").filter(Boolean);
  }
  const verbPhrase = joinActions(actions);
  const noun = target === "*" ? "records in every collection" : `${target} records`;
  return `${verbPhrase} ${noun}`;
}

function describeBlobScope(rest: string): string {
  const [target] = splitOnce(rest, "?");
  const [type, subtype] = target.split("/");
  if (!type || !subtype) return `Upload files (${rest})`;
  if (type === "*" && subtype === "*") return "Upload files";
  if (subtype === "*") return `Upload ${type} files`;
  return `Upload ${type}/${subtype} files`;
}

function describeOtherScope(prefix: string, rest: string): string {
  switch (prefix) {
    case "rpc":
      return `Call the ${rest} API method`;
    case "identity":
      return `Access identity info (${rest})`;
    case "account":
      return `Access account info (${rest})`;
    default:
      return rest ? `${prefix}: ${rest}` : prefix;
  }
}

/**
 * Translate one OAuth scope string into plain language for a non-technical
 * reader. `quiet: true` marks scopes that are plumbing every grant needs
 * (`atproto`, `identity:*`, `rpc:*`) rather than a permission worth weighing.
 */
export function describeScope(scope: string): ScopeDescription {
  if (scope === "atproto") {
    return { raw: scope, text: "Basic account access", quiet: true };
  }

  const [prefix, rest] = splitOnce(scope, ":");
  if (rest === null) {
    // Doesn't match the `prefix:rest` grammar at all — show it verbatim
    // rather than guessing at a translation.
    return { raw: scope, text: scope, quiet: false };
  }

  if ((prefix === "identity" || prefix === "rpc") && rest === "*") {
    return { raw: scope, text: "Basic account access", quiet: true };
  }

  switch (prefix) {
    case "repo":
      return { raw: scope, text: describeRepoScope(rest), quiet: false };
    case "blob":
      return { raw: scope, text: describeBlobScope(rest), quiet: false };
    default:
      return { raw: scope, text: describeOtherScope(prefix, rest), quiet: false };
  }
}
