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
 * A reasonable starter script body — defines the required `handle()`
 * function. Used to prefill the new-script form.
 */
export const DEFAULT_SCRIPT_BODY = `-- Trigger script: receives an \`event\` table describing what fired
-- the script and returns either a transformed event/record (table) or
-- \`nil\` to skip the operation.
--
-- Available APIs: db.*, http.*, xrpc.*, atproto.*, Record.*, env.<KEY>

function handle()
  log("script fired")
  return event
end
`

export const DEFAULT_JOB_SCRIPT_BODY = `-- Job runner: executes as a background job.
--
-- Available globals:
--   job.input      — the input table passed to jobs.create()
--   job.id         — the job's UUID
--   job.progress() — persist progress (visible in the dashboard)
--   job.should_stop() — check for pause/cancel (cooperative)
--   job.wait(seconds) — sleep (0–3600s)
--
-- Available APIs: db.*, http.*, xrpc.*, atproto.*, Record.*, env.<KEY>
-- Return value becomes the job's result.

function handle()
  local input = job.input

  job.progress({ status = "working" })

  if job.should_stop() then
    return { partial = true }
  end

  return { done = true }
end
`
