import type { TriggerKind } from "@/types/scripts";

import jobBody from "./lua-templates/job.lua";
import recordEventBody from "./lua-templates/record-event.lua";
import triggerBody from "./lua-templates/trigger.lua";

export const LEXICON_TEMPLATE = JSON.stringify(
  {
    $type: "com.atproto.lexicon.schema",
    lexicon: 1,
    id: "com.example.myRecord",
    defs: {
      main: {
        type: "record",
        key: "tid",
        record: {
          type: "object",
          required: [],
          properties: {},
        },
      },
    },
  },
  null,
  2,
);

/** Starter body for a trigger whose return value is the response. */
export const DEFAULT_SCRIPT_BODY: string = triggerBody;

/** Starter body for a `job.run:<type>` script. */
export const DEFAULT_JOB_SCRIPT_BODY: string = jobBody;

/**
 * Starter body for a `record.*` script. A record script's table return
 * replaces the record body, so echoing `input` there would index the event
 * envelope in place of every record; this body returns the record itself.
 */
export const DEFAULT_RECORD_SCRIPT_BODY: string = recordEventBody;

/** The body the new-script form prefills for a trigger kind. */
export function defaultBodyFor(kind: TriggerKind): string {
  if (kind === "job.run") return DEFAULT_JOB_SCRIPT_BODY;
  if (kind.startsWith("record.")) return DEFAULT_RECORD_SCRIPT_BODY;
  return DEFAULT_SCRIPT_BODY;
}

/** Whether `body` is still one of the prefills, untouched by the operator. */
export function isDefaultBody(body: string): boolean {
  return (
    body === DEFAULT_SCRIPT_BODY ||
    body === DEFAULT_JOB_SCRIPT_BODY ||
    body === DEFAULT_RECORD_SCRIPT_BODY
  );
}
