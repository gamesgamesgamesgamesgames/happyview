/**
 * Scripting language a script is written for. The backend stamps this
 * on every row so a future runtime (e.g. `"typescript"`) can land
 * without a schema migration. Today only `"lua"` ships.
 */
export type ScriptLanguage = "lua"

/**
 * One row from the `scripts` table — what the admin API returns.
 *
 * The script's `id` IS its trigger string; the dispatcher resolves
 * scripts directly by id at firing time. Examples:
 *
 *   record.index:com.example.thing       — wildcard for any record event
 *   record.create:com.example.thing      — fires only on create
 *   xrpc.query:com.example.list          — XRPC query handler
 *   xrpc.procedure:com.example.create    — XRPC procedure handler
 *   labeler.apply:app.bsky.feed.post     — label on at://<did>/app.bsky.feed.post/<rkey>
 *   labeler.apply:_actor                 — label on a bare DID
 *
 * Cascade rule (record events ONLY): the dispatcher tries
 * `record.<action>:<nsid>` first, falls back to `record.index:<nsid>`
 * if no specific row exists. No cascade for XRPC or labeler triggers.
 */
export interface Script {
  /** Trigger string; identifies the row. */
  id: string
  script_type: ScriptLanguage
  body: string
  description?: string | null
  created_at: string
  updated_at: string
  /**
   * `false` when this script's own trigger id would be refused if submitted
   * today — the NSID rules tightened after it was created. It still fires and
   * can still be edited, but deleting it is irreversible: it cannot be
   * recreated with the same id.
   */
  recreatable: boolean
  /**
   * Removed v2 globals (`db`, `Record`, `now`, `params`, …) this script still
   * references as free names. Empty once migrated onto `require("happyview.*")`
   * / `require("internal.*")` — always empty for a JavaScript script, since
   * the codemod only understands Lua. Can instead be `["unparseable"]` when
   * the body does not parse as Lua at all, so migration status can't be read
   * off it.
   */
  needs_migration: string[]
}

/** One construct the codemod could not rewrite mechanically. */
export interface CodemodNote {
  /** Line number in the *original* source the construct sits on. */
  line: number
  message: string
}

/** Result of `POST /admin/scripts/{id}/codemod`. */
export interface CodemodResult {
  source: string
  notes: CodemodNote[]
  /**
   * `false` when the rewrite equals the stored body, so applying would store
   * nothing. Not the same as being on the v3 contract: a script applied with
   * markers left in place reports `false` too.
   */
  changed: boolean
}

/**
 * Body for `POST /admin/scripts/{id}/codemod`. Omit entirely, or send `{}`,
 * to preview without storing anything.
 */
export interface CodemodRequestBody {
  /**
   * Stores the rewritten body. Previewing needs `scripts:read`; this needs
   * `scripts:manage`, since it writes the row.
   */
  apply?: boolean
  /**
   * Required alongside `apply` when the rewrite changed the body and still
   * leaves `-- codemod:` markers behind; otherwise the request is refused
   * with 409. Sent only when the operator ticks it, never as `false`, so the
   * server's default refusal holds.
   */
  allow_markers?: boolean
  /**
   * Text to rewrite in place of the stored body, for an editor holding edits
   * the server has not seen. Preview only: the server refuses it alongside
   * `apply`. No stored script is needed, so it works before the first save;
   * the id in the path then supplies only the script's kind.
   */
  source?: string
}

/**
 * The 400 `POST /admin/scripts` and `PATCH /admin/scripts/{id}` answer for a
 * Lua body that still references removed v2 globals. `removed_globals` is
 * what identifies it; `error` is the same list as a sentence.
 */
export interface UnmigratedScriptError {
  error: string
  removed_globals: string[]
}

/** Body for `POST /admin/scripts` (create or replace by `id`). */
export interface UpsertScriptBody {
  id: string
  /** Defaults to `"lua"` server-side if omitted. */
  script_type?: ScriptLanguage
  body: string
  description?: string | null
}

/**
 * Body for `PATCH /admin/scripts/{id}`. All fields optional. Patching
 * `script_type` requires `body` alongside (server can't validate a
 * stale body against a new language).
 */
export interface PatchScriptBody {
  script_type?: ScriptLanguage
  body?: string
  description?: string | null
}

// ---------------------------------------------------------------------------
// Trigger grammar
// ---------------------------------------------------------------------------

/** Trigger families the dispatcher knows. */
export type TriggerKind =
  | "record.index"
  | "record.create"
  | "record.update"
  | "record.delete"
  | "xrpc.query"
  | "xrpc.procedure"
  | "labeler.apply"
  | "job.run"

/** Display labels for each trigger kind. */
export const TRIGGER_KIND_LABELS: Record<TriggerKind, string> = {
  "record.index": "Record (any action)",
  "record.create": "Record create",
  "record.update": "Record update",
  "record.delete": "Record delete",
  "xrpc.query": "XRPC query",
  "xrpc.procedure": "XRPC procedure",
  "labeler.apply": "Label arrival",
  "job.run": "Job runner",
}

/** Top-level grouping for the Scripts list page. */
export type TriggerFamily = "record" | "xrpc" | "labeler" | "job"

export const TRIGGER_FAMILY_LABELS: Record<TriggerFamily, string> = {
  record: "Record events",
  xrpc: "XRPC handlers",
  labeler: "Label arrivals",
  job: "Job runners",
}

/** Map a trigger kind to its top-level family. */
export function familyOf(kind: TriggerKind): TriggerFamily {
  if (kind.startsWith("record.")) return "record"
  if (kind.startsWith("xrpc.")) return "xrpc"
  if (kind.startsWith("job.")) return "job"
  return "labeler"
}

/**
 * Split a trigger id into its `(kind, suffix)` parts. Returns `null`
 * if the id doesn't match the trigger grammar — useful for surfacing
 * broken rows in the UI.
 */
export function parseTriggerId(
  id: string,
): { kind: TriggerKind; suffix: string } | null {
  const sep = id.indexOf(":")
  if (sep <= 0 || sep === id.length - 1) return null
  const prefix = id.slice(0, sep)
  const suffix = id.slice(sep + 1)
  const kind = ([
    "record.index",
    "record.create",
    "record.update",
    "record.delete",
    "xrpc.query",
    "xrpc.procedure",
    "labeler.apply",
    "job.run",
  ] as const).find((k) => k === prefix)
  if (!kind) return null
  return { kind, suffix }
}
