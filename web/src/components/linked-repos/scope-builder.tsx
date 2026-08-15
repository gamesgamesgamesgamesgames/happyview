"use client";

import { useEffect, useMemo, useState } from "react";
import { AlertTriangle, X } from "lucide-react";

import { isValidNsid } from "@happyview/nsid";

import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import {
  Combobox,
  ComboboxContent,
  ComboboxEmpty,
  ComboboxInput,
  ComboboxItem,
  ComboboxList,
} from "@/components/ui/combobox";
import { Input } from "@/components/ui/input";
import {
  InputGroup,
  InputGroupAddon,
  InputGroupInput,
} from "@/components/ui/input-group";

/**
 * One shared column geometry for every row: prefix, value, remove.
 *
 * The third column is declared even on the trailing row that has no remove
 * button, so the value inputs all end at the same x. Secondary content —
 * checkboxes, errors, notes — sits at `col-start-2` and therefore lines up
 * under the value it describes instead of hanging off the left edge.
 */
const ROW_GRID =
  "grid grid-cols-[11rem_minmax(0,1fr)_2.25rem] items-start gap-x-2 gap-y-1";

// Mirrors src/linked_repos/scope.rs byte-for-byte. Keep the two in sync —
// this is the client-side half of the same grammar the server enforces.

const REPO_ACTIONS = ["create", "update", "delete"] as const;
export type RepoAction = (typeof REPO_ACTIONS)[number];

function isRepoAction(value: string): value is RepoAction {
  return (REPO_ACTIONS as readonly string[]).includes(value);
}

/**
 * Normalise a set of actions to declaration order, dropping duplicates.
 *
 * The emitted string is then a function of *what* was picked and never of the
 * order the boxes happened to be clicked in, so two operators granting the same
 * access produce byte-identical scopes.
 */
function orderActions(actions: readonly string[]): RepoAction[] {
  return REPO_ACTIONS.filter((a) => actions.includes(a));
}

function splitQuery(rest: string): [string, string | null] {
  const i = rest.indexOf("?");
  return i === -1 ? [rest, null] : [rest.slice(0, i), rest.slice(i + 1)];
}

function validateRepoRest(rest: string): string | null {
  const [target, query] = splitQuery(rest);
  if (!target) return "repo scope requires a collection or *";
  if (target !== "*" && !isValidNsid(target)) return `invalid NSID: ${target}`;
  if (query === null) return null;
  if (!query.startsWith("action=")) {
    return `unsupported repo scope parameter: ${query}`;
  }
  const actions = query.slice("action=".length);
  if (!actions) return "repo scope action list must not be empty";
  for (const action of actions.split(",")) {
    if (!isRepoAction(action)) return `unknown repo action: ${action}`;
  }
  return null;
}

function validateBlobRest(rest: string): string | null {
  const [target] = splitQuery(rest);
  const slash = target.indexOf("/");
  if (slash === -1) return "blob scope must be type/subtype";
  const type = target.slice(0, slash);
  const subtype = target.slice(slash + 1);
  if (!type || !subtype) return "blob scope must be type/subtype";
  return null;
}

/**
 * Scope prefixes the server accepts. `repo` and `blob` have structured values
 * it validates strictly; the rest are shape-checked only (non-empty).
 */
export const SCOPE_PREFIXES = [
  // No `?action=` in the repo hint: the checkboxes own the action list, and a
  // placeholder advertising the query invites typing a second copy of it.
  // Pasting one still works — `hoistRepoActions` lifts it into the boxes.
  { value: "repo", hint: "com.example.note" },
  { value: "blob", hint: "image/* or */*" },
  { value: "rpc", hint: "* or com.example.doThing" },
  { value: "identity", hint: "*" },
  { value: "account", hint: "email" },
  { value: "transition", hint: "generic or chat.bsky" },
  { value: "include", hint: "com.example.permissionSet" },
] as const;

const PREFIX_VALUES = SCOPE_PREFIXES.map((p) => p.value) as readonly string[];

export function validateScopeToken(scope: string): string | null {
  if (!scope) return "scope must not be empty";
  if (scope === "atproto") return null;

  const sep = scope.indexOf(":");
  if (sep === -1) return `unknown scope: ${scope}`;
  const prefix = scope.slice(0, sep);
  const rest = scope.slice(sep + 1);

  switch (prefix) {
    case "repo":
      return validateRepoRest(rest);
    case "blob":
      return validateBlobRest(rest);
    case "rpc":
    case "identity":
    case "account":
    case "transition":
    case "include":
      return rest ? null : `${prefix} scope requires a value`;
    default:
      return `unknown scope prefix: ${prefix}`;
  }
}

/**
 * The scope every AT Protocol authorization request must carry. Always emitted,
 * never a choice — an authorization server rejects the whole request without it.
 */
export const BASE_SCOPE = "atproto";

/**
 * Does this scope string grant nothing beyond {@link BASE_SCOPE}?
 *
 * The builder always emits `atproto`, so "the string is non-empty" no longer
 * means the operator picked anything. Callers use this to keep a grant that can
 * do nothing at all from being created by simply opening the dialog.
 */
export function grantsNoAccess(value: string): boolean {
  return value
    .split(/\s+/)
    .filter(Boolean)
    .every((token) => token === BASE_SCOPE);
}

/** Validate a whitespace-separated scope string. Returns the first error, or null if valid. */
export function validateScopeString(value: string): string | null {
  const tokens = value.split(/\s+/).filter(Boolean);
  if (tokens.length === 0) return "at least one scope is required";
  for (const token of tokens) {
    const err = validateScopeToken(token);
    if (err) return err;
  }
  return null;
}

interface ScopeRow {
  prefix: string;
  /** For `repo` rows this is the target alone; the query lives in `actions`. */
  rest: string;
  /** Only meaningful for `repo` rows. */
  actions: RepoAction[];
}

const EMPTY_ROW: ScopeRow = { prefix: "", rest: "", actions: [] };

/**
 * Pull an `?action=` query out of a `repo` value into the checkbox state.
 *
 * The value field is the target alone, but operators paste whole scope strings
 * into it, so a pasted query is hoisted into the boxes rather than left to
 * produce a second, invisible source of truth. Returns `null` when there is
 * nothing to hoist or the query is malformed — malformed text stays in the
 * field so the row error can explain what is wrong with it.
 */
function hoistRepoActions(
  value: string,
): { rest: string; actions: RepoAction[] } | null {
  const [target, query] = splitQuery(value);
  if (query === null || !query.startsWith("action=")) return null;
  const actions = query.slice("action=".length).split(",").filter(Boolean);
  if (actions.length === 0 || !actions.every(isRepoAction)) return null;
  return { rest: target, actions: orderActions(actions) };
}

/** Split a stored scope string back into editable rows, dropping `atproto`. */
function parseInitialRows(value: string): ScopeRow[] {
  const rows: ScopeRow[] = [];
  for (const token of value.split(/\s+/).filter(Boolean)) {
    if (token === BASE_SCOPE) continue;
    const sep = token.indexOf(":");
    if (sep === -1) {
      rows.push({ ...EMPTY_ROW, prefix: token });
      continue;
    }
    const prefix = token.slice(0, sep);
    const rest = token.slice(sep + 1);
    const hoisted = prefix === "repo" ? hoistRepoActions(rest) : null;
    rows.push(
      hoisted
        ? { prefix, rest: hoisted.rest, actions: hoisted.actions }
        : { ...EMPTY_ROW, prefix, rest },
    );
  }
  rows.push({ ...EMPTY_ROW });
  return rows;
}

function rowToToken(row: ScopeRow): string {
  const prefix = row.prefix.trim();
  const rest = row.rest.trim();
  if (!prefix && !rest) return "";
  if (!prefix) return rest;
  if (prefix === "repo") {
    const actions = orderActions(row.actions);
    // With no actions this is the bare form, which the server reads as *every*
    // action. The builder never means that — `repoActionError` refuses the row
    // — but it still has to render something for the preview and the error to
    // describe.
    return actions.length
      ? `repo:${rest}?action=${actions.join(",")}`
      : `repo:${rest}`;
  }
  return `${prefix}:${rest}`;
}

/**
 * Builder-only rule: a `repo` row has to say which actions it grants.
 *
 * `repo:<nsid>` with no query is valid to the server and grants create, update
 * *and* delete. Inheriting that default here would make "checked nothing" mean
 * "granted everything" — the opposite of what someone assembling a
 * least-privilege grant intends, and unfixable afterwards because scopes are
 * immutable. The grammar keeps accepting the bare form for scopes written by
 * hand or through the API; the builder simply won't produce one.
 */
function repoActionError(row: ScopeRow): string | null {
  if (row.prefix.trim() !== "repo") return null;
  // Stay quiet until there is a target, matching how row errors ignore blanks.
  if (!row.rest.trim()) return null;
  if (orderActions(row.actions).length > 0) return null;
  return "select at least one action";
}

/** `update` without `create` can't upsert — see the note this renders. */
function cannotUpsert(row: ScopeRow): boolean {
  return (
    row.prefix.trim() === "repo" &&
    row.actions.includes("update") &&
    !row.actions.includes("create")
  );
}

function buildScopeString(rows: ScopeRow[]): string {
  // `atproto` is not a choice, so it is not a row: emit it first and show it in
  // the preview, so what the operator reads is exactly what the grant will
  // store and request rather than something silently corrected server-side.
  const parts: string[] = [BASE_SCOPE];
  for (const row of rows) {
    const token = rowToToken(row);
    if (!token || token === BASE_SCOPE) continue;
    parts.push(token);
  }
  return parts.join(" ");
}

interface ScopeBuilderProps {
  value: string;
  onChange: (scopes: string) => void;
  /**
   * Reports builder-level problems the scope string can't express — currently
   * a `repo` row with no actions picked, which reads as a valid (and maximally
   * permissive) scope once serialised. Pass a stable function; a `useState`
   * setter is ideal.
   */
  onValidityChange?: (error: string | null) => void;
}

// Note: `value` only seeds the builder's initial state on mount — after that,
// this component is the source of truth and pushes every change back through
// onChange. Callers that need a hard reset (e.g. reopening a dialog) should
// remount via `key`, rather than relying on later `value` prop changes.
export function ScopeBuilder({
  value,
  onChange,
  onValidityChange,
}: ScopeBuilderProps) {
  // Lazy initializer: `value` seeds the rows once on mount and is not read
  // again — see the note above about remounting via `key` for a hard reset.
  const [rows, setRows] = useState<ScopeRow[]>(() => parseInitialRows(value));

  const scopeString = useMemo(() => buildScopeString(rows), [rows]);

  function update(next: ScopeRow[]) {
    // Always keep exactly one trailing blank row to type into.
    const trimmed = [...next];
    const last = trimmed[trimmed.length - 1];
    if (!last || last.prefix.trim() !== "" || last.rest.trim() !== "") {
      trimmed.push({ ...EMPTY_ROW });
    }
    setRows(trimmed);
    onChange(buildScopeString(trimmed));
  }

  function setRow(index: number, patch: Partial<ScopeRow>) {
    update(
      rows.map((r, i) => {
        if (i !== index) return r;
        const next = { ...r, ...patch };
        // Actions belong to `repo` alone, so leaving it drops them rather than
        // letting a stale set reappear if the operator switches back.
        if (patch.prefix !== undefined && patch.prefix.trim() !== "repo") {
          next.actions = [];
        }
        return next;
      }),
    );
  }

  /** Value-field edits, hoisting a pasted `?action=` into the checkboxes. */
  function setRest(index: number, raw: string) {
    if (rows[index]?.prefix.trim() === "repo") {
      const hoisted = hoistRepoActions(raw);
      if (hoisted) {
        setRow(index, { rest: hoisted.rest, actions: hoisted.actions });
        return;
      }
    }
    setRow(index, { rest: raw });
  }

  function toggleAction(index: number, action: RepoAction, checked: boolean) {
    const current = rows[index]?.actions ?? [];
    setRow(index, {
      actions: orderActions(
        checked ? [...current, action] : current.filter((a) => a !== action),
      ),
    });
  }

  function removeRow(index: number) {
    update(rows.filter((_, i) => i !== index));
  }

  const rowErrors = rows.map((row) => {
    const token = rowToToken(row);
    if (!token) return null;
    return validateScopeToken(token) ?? repoActionError(row);
  });

  const firstError = rowErrors.find((e) => e != null) ?? null;
  useEffect(() => {
    onValidityChange?.(firstError);
  }, [firstError, onValidityChange]);

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-2">
        <span className="text-sm font-medium">Scopes</span>
        <p className="text-muted-foreground text-xs">
          What this grant may do. Pick a prefix and fill in the rest — for
          example <code className="font-mono">repo</code> +{" "}
          <code className="font-mono">com.example.note</code>, then the actions
          it may perform on that collection.
        </p>

        <div className="flex flex-col gap-2">
          {/* `atproto` is mandatory in every authorization request, so it is
              shown as a fixed row rather than something to add or remove. It
              has no prefix, so it spans the prefix and value columns and ends
              flush with every other input. */}
          <div className={ROW_GRID}>
            <Input
              value={BASE_SCOPE}
              readOnly
              aria-label="Base scope, always included"
              className="col-span-2 bg-muted font-mono text-sm"
            />
          </div>

          {rows.map((row, index) => {
            const known = SCOPE_PREFIXES.find((p) => p.value === row.prefix);
            const isLast = index === rows.length - 1;
            return (
              <div key={index} className={ROW_GRID}>
                <Combobox
                  items={PREFIX_VALUES}
                  inputValue={row.prefix}
                  onInputValueChange={(v, details) => {
                    if (details.reason === "input-change") {
                      setRow(index, { prefix: v });
                    }
                  }}
                  onValueChange={(v) => {
                    if (typeof v === "string") setRow(index, { prefix: v });
                  }}
                >
                  <ComboboxInput
                    className="w-full font-mono text-sm"
                    placeholder="prefix"
                    aria-label={`Scope ${index + 1} prefix`}
                  />
                  <ComboboxContent className="w-auto min-w-(--anchor-width)">
                    <ComboboxEmpty>No matching prefix</ComboboxEmpty>
                    <ComboboxList>
                      {(item: string) => (
                        <ComboboxItem key={item} value={item}>
                          <span className="font-mono">{item}</span>
                        </ComboboxItem>
                      )}
                    </ComboboxList>
                  </ComboboxContent>
                </Combobox>

                {/* `repo` is the one prefix whose value has structure, so its
                    collection and its actions share a single bordered field.
                    Sitting inside the same box (and the same focus ring) is
                    what ties the checkboxes to the collection they apply to —
                    below-and-indented alone reads as a sibling row. */}
                {row.prefix.trim() === "repo" ? (
                  <InputGroup>
                    <InputGroupInput
                      value={row.rest}
                      onChange={(e) => setRest(index, e.target.value)}
                      placeholder={known?.hint ?? "value"}
                      aria-label={`Scope ${index + 1} value`}
                      className="font-mono text-sm"
                    />
                    <InputGroupAddon
                      align="block-end"
                      // The addon focuses the input when its padding is
                      // clicked. Inside the action group that gesture belongs
                      // to the checkboxes, so it stops here.
                      onClick={(e) => e.stopPropagation()}
                    >
                      <fieldset className="flex flex-wrap items-center gap-x-5 gap-y-1.5">
                        <legend className="sr-only">
                          Scope {index + 1} actions
                        </legend>
                        {REPO_ACTIONS.map((action) => {
                          const id = `scope-${index}-${action}`;
                          return (
                            <div
                              key={action}
                              className="flex items-center gap-2"
                            >
                              <Checkbox
                                id={id}
                                checked={row.actions.includes(action)}
                                onCheckedChange={(checked) =>
                                  toggleAction(index, action, checked === true)
                                }
                              />
                              {/* The addon renders its contents muted, which
                                  suits hint text but not a control label —
                                  these are interactive and carry the same
                                  weight as the value above them. */}
                              <Label
                                htmlFor={id}
                                className="text-foreground cursor-pointer font-mono text-sm font-normal"
                              >
                                {action}
                              </Label>
                            </div>
                          );
                        })}
                      </fieldset>
                    </InputGroupAddon>
                  </InputGroup>
                ) : (
                  <Input
                    value={row.rest}
                    onChange={(e) => setRest(index, e.target.value)}
                    placeholder={known?.hint ?? "value"}
                    aria-label={`Scope ${index + 1} value`}
                    className="font-mono text-sm"
                  />
                )}

                {/* The cell is declared by the grid either way, so the trailing
                    row's input still ends where every other one does. */}
                {!isLast && (
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon"
                    aria-label={`Remove scope ${index + 1}`}
                    className="size-9 text-muted-foreground hover:text-destructive"
                    onClick={() => removeRow(index)}
                  >
                    <X className="size-4" />
                  </Button>
                )}

                {rowErrors[index] && (
                  <p className="col-start-2 col-span-2 text-destructive text-xs">
                    {rowErrors[index]}
                  </p>
                )}

                {cannotUpsert(row) && (
                  <p className="col-start-2 col-span-2 text-muted-foreground text-xs">
                    <code className="font-mono">update</code> without{" "}
                    <code className="font-mono">create</code> can&apos;t{" "}
                    <code className="font-mono">put_record</code>, which upserts
                    — unless the script always passes a{" "}
                    <code className="font-mono">swap_cid</code>.
                  </p>
                )}
              </div>
            );
          })}
        </div>

        {rows.some((r) => rowToToken(r) === "transition:generic") && (
          <div className="flex items-start gap-3 rounded-lg border border-amber-500/50 bg-amber-500/10 p-3">
            <AlertTriangle className="size-4 text-amber-500 shrink-0 mt-0.5" />
            <p className="text-xs text-amber-500">
              <code>transition:generic</code> grants broad write access to any
              collection in this repo. Prefer specific{" "}
              <code>repo:&lt;collection&gt;</code> scopes — a linked repo&apos;s
              grant is long-lived and its scopes can&apos;t be narrowed later
              without deleting and re-linking.
            </p>
          </div>
        )}

      </div>

      <div className="flex flex-col gap-2">
        <span className="text-sm font-medium">Preview</span>
        <p className="rounded-md border bg-muted/50 px-3 py-2 font-mono text-xs break-all min-h-9">
          {scopeString}
        </p>
        {grantsNoAccess(scopeString) && (
          <p className="text-muted-foreground text-xs">
            Nothing selected yet. <code className="font-mono">{BASE_SCOPE}</code>{" "}
            on its own authorizes no reads or writes.
          </p>
        )}
      </div>
    </div>
  );
}
