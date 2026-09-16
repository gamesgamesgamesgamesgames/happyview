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

/**
 * A reasonable starter script body — defines the required
 * `handle(input, ctx)` function. Used to prefill the new-script form.
 */
export const DEFAULT_SCRIPT_BODY = `-- Trigger script: \`input\` is the trigger's payload; \`ctx\` describes
-- this invocation (caller, environment, trigger id, and more). Return
-- a transformed value, or \`nil\` to skip the operation.
--
-- require("internal.*") and installed libraries provide the rest.

local log = require("internal.logging")

function handle(input, ctx)
  log.info("script fired", { trigger = ctx.trigger })
  return input
end
`

export const DEFAULT_JOB_SCRIPT_BODY = `-- Job runner: executes as a background job. \`input\` is the job's
-- input table; \`ctx.job\` exposes the job's id, progress(data),
-- should_stop(), and wait(seconds). Return value becomes the job's result.
--
-- require("internal.*") and installed libraries provide the rest.

local log = require("internal.logging")

function handle(input, ctx)
  log.info("job started", { job_id = ctx.job.id })

  ctx.job.progress({ status = "working" })

  if ctx.job.should_stop() then
    return { partial = true }
  end

  return { done = true }
end
`
