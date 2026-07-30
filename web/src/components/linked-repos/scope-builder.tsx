"use client";

import { useMemo, useState } from "react";
import { AlertTriangle, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Combobox,
  ComboboxContent,
  ComboboxEmpty,
  ComboboxInput,
  ComboboxItem,
  ComboboxList,
} from "@/components/ui/combobox";
import { Input } from "@/components/ui/input";

// Mirrors src/linked_repos/scope.rs byte-for-byte. Keep the two in sync —
// this is the client-side half of the same grammar the server enforces.

const REPO_ACTIONS = ["create", "update", "delete"] as const;
export type RepoAction = (typeof REPO_ACTIONS)[number];

function isRepoAction(value: string): value is RepoAction {
  return (REPO_ACTIONS as readonly string[]).includes(value);
}

// Same regex atrium_api::types::string::Nsid::new() compiles server-side.
const NSID_RE =
  /^[a-zA-Z]([a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?(\.[a-zA-Z0-9]([a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?)+(\.[a-zA-Z][a-zA-Z0-9]{0,62})$/;

function isValidNsid(nsid: string): boolean {
  return nsid.length <= 317 && NSID_RE.test(nsid);
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
  { value: "repo", hint: "com.example.note?action=create,update" },
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
  rest: string;
}

/** Split a stored scope string back into editable rows, dropping `atproto`. */
function parseInitialRows(value: string): ScopeRow[] {
  const rows: ScopeRow[] = [];
  for (const token of value.split(/\s+/).filter(Boolean)) {
    if (token === BASE_SCOPE) continue;
    const sep = token.indexOf(":");
    if (sep === -1) {
      rows.push({ prefix: token, rest: "" });
    } else {
      rows.push({ prefix: token.slice(0, sep), rest: token.slice(sep + 1) });
    }
  }
  rows.push({ prefix: "", rest: "" });
  return rows;
}

function rowToToken(row: ScopeRow): string {
  const prefix = row.prefix.trim();
  const rest = row.rest.trim();
  if (!prefix && !rest) return "";
  if (!prefix) return rest;
  return `${prefix}:${rest}`;
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
}

// Note: `value` only seeds the builder's initial state on mount — after that,
// this component is the source of truth and pushes every change back through
// onChange. Callers that need a hard reset (e.g. reopening a dialog) should
// remount via `key`, rather than relying on later `value` prop changes.
export function ScopeBuilder({ value, onChange }: ScopeBuilderProps) {
  // Lazy initializer: `value` seeds the rows once on mount and is not read
  // again — see the note above about remounting via `key` for a hard reset.
  const [rows, setRows] = useState<ScopeRow[]>(() => parseInitialRows(value));

  const scopeString = useMemo(() => buildScopeString(rows), [rows]);

  function update(next: ScopeRow[]) {
    // Always keep exactly one trailing blank row to type into.
    const trimmed = [...next];
    const last = trimmed[trimmed.length - 1];
    if (!last || last.prefix.trim() !== "" || last.rest.trim() !== "") {
      trimmed.push({ prefix: "", rest: "" });
    }
    setRows(trimmed);
    onChange(buildScopeString(trimmed));
  }

  function setRow(index: number, patch: Partial<ScopeRow>) {
    update(rows.map((r, i) => (i === index ? { ...r, ...patch } : r)));
  }

  function removeRow(index: number) {
    update(rows.filter((_, i) => i !== index));
  }

  const rowErrors = rows.map((row) => {
    const token = rowToToken(row);
    if (!token) return null;
    return validateScopeToken(token);
  });

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-2">
        <span className="text-sm font-medium">Scopes</span>
        <p className="text-muted-foreground text-xs">
          What this grant may do. Pick a prefix and fill in the rest — for
          example <code className="font-mono">repo</code> +{" "}
          <code className="font-mono">com.example.note?action=create</code>.
        </p>

        <div className="flex flex-col gap-1.5">
          {/* `atproto` is mandatory in every authorization request, so it is
              shown as a fixed row rather than something to add or remove. */}
          <div className="flex gap-1.5">
            <Input
              value={BASE_SCOPE}
              readOnly
              aria-label="Base scope, always included"
              className="font-mono text-sm bg-muted"
            />
          </div>

          {rows.map((row, index) => {
            const known = SCOPE_PREFIXES.find((p) => p.value === row.prefix);
            const isLast = index === rows.length - 1;
            return (
              <div key={index} className="flex flex-col gap-1">
                <div className="flex gap-1.5">
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
                      className="w-44 shrink-0 font-mono text-sm"
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

                  <Input
                    value={row.rest}
                    onChange={(e) => setRow(index, { rest: e.target.value })}
                    placeholder={known?.hint ?? "value"}
                    aria-label={`Scope ${index + 1} value`}
                    className="font-mono text-sm"
                  />

                  {!isLast && (
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon"
                      aria-label={`Remove scope ${index + 1}`}
                      className="shrink-0 size-9 text-muted-foreground hover:text-destructive"
                      onClick={() => removeRow(index)}
                    >
                      <X className="size-4" />
                    </Button>
                  )}
                </div>
                {rowErrors[index] && (
                  <p className="text-destructive text-xs">{rowErrors[index]}</p>
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

        <p className="text-muted-foreground text-xs">
          <code className="font-mono">put_record</code> upserts, so it needs both{" "}
          <code className="font-mono">create</code> and{" "}
          <code className="font-mono">update</code> on a collection unless the
          script always passes a <code className="font-mono">swap_cid</code>{" "}
          (which needs only <code className="font-mono">update</code>). Granting
          only <code className="font-mono">update</code> creates a grant that
          can&apos;t upsert on its own.
        </p>
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
