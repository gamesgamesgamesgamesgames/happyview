import type { ScriptLanguage, TriggerKind } from "@/types/scripts";
import {
  DEFAULT_JOB_SCRIPT_BODY,
  DEFAULT_RECORD_SCRIPT_BODY,
  DEFAULT_SCRIPT_BODY,
} from "@/lib/lua-templates";

import jsJobBody from "./script-templates/javascript/job.js.txt";
import jsRecordEventBody from "./script-templates/javascript/record-event.js.txt";
import jsTriggerBody from "./script-templates/javascript/trigger.js.txt";
import tsJobBody from "./script-templates/typescript/job.ts.txt";
import tsRecordEventBody from "./script-templates/typescript/record-event.ts.txt";
import tsTriggerBody from "./script-templates/typescript/trigger.ts.txt";

/** A language's starters: one per way a trigger kind reads its return value. */
interface Starters {
  job: string;
  recordEvent: string;
  trigger: string;
}

/**
 * Starter bodies by `script_type`. `tests/script_starters.rs` saves and runs
 * each JavaScript and TypeScript one against the real interpreter, and
 * `tests/admin_scripts_validate.rs` saves each Lua one.
 */
const STARTERS: Record<string, Starters> = {
  lua: {
    job: DEFAULT_JOB_SCRIPT_BODY,
    recordEvent: DEFAULT_RECORD_SCRIPT_BODY,
    trigger: DEFAULT_SCRIPT_BODY,
  },
  javascript: {
    job: jsJobBody,
    recordEvent: jsRecordEventBody,
    trigger: jsTriggerBody,
  },
  typescript: {
    job: tsJobBody,
    recordEvent: tsRecordEventBody,
    trigger: tsTriggerBody,
  },
};

const LABELS: Record<string, string> = {
  lua: "Lua",
  javascript: "JavaScript",
  typescript: "TypeScript",
};

/**
 * What to call a language. An interpreter for a language this dashboard has
 * no name for is still installable, so its id stands in.
 */
export function languageLabel(language: ScriptLanguage): string {
  return LABELS[language] ?? language;
}

/**
 * The body the new-script form prefills for a trigger kind in a language.
 * Empty for a language with no starter here: its interpreter's contract is
 * not one this dashboard knows.
 */
export function defaultBodyFor(
  kind: TriggerKind,
  language: ScriptLanguage,
): string {
  const starters = STARTERS[language];
  if (!starters) return "";
  if (kind === "job.run") return starters.job;
  if (kind.startsWith("record.")) return starters.recordEvent;
  return starters.trigger;
}

/**
 * Whether `body` is still a prefill, in any language, untouched by the
 * operator — and so safe to replace when the kind or the language changes.
 */
export function isDefaultBody(body: string): boolean {
  return (
    body === "" ||
    Object.values(STARTERS).some(
      (s) => body === s.job || body === s.recordEvent || body === s.trigger,
    )
  );
}

/**
 * The language a new script starts in: Lua when it is installed, since that
 * is what every script was before there was a choice, and otherwise the first
 * installed one.
 */
export function defaultLanguage(installed: ScriptLanguage[]): ScriptLanguage {
  if (installed.length === 0 || installed.includes("lua")) return "lua";
  return installed[0];
}
