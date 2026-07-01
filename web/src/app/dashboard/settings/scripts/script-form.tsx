"use client";

import { useEffect, useMemo, useRef } from "react";

import { MonacoEditor } from "@/components/monaco-editor";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectSeparator,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import type { LexiconSummary } from "@/types/lexicons";
import type { TriggerKind } from "@/types/scripts";
import {
  DEFAULT_JOB_SCRIPT_BODY,
  DEFAULT_SCRIPT_BODY,
  TRIGGER_KIND_LABELS,
  parseTriggerId,
} from "@/types/scripts";

/**
 * Sentinel suffix used when the operator picks "Actor" in the lexicon
 * dropdown. Combined with `kind = "labeler.apply"` it yields the
 * `labeler.apply:_actor` trigger.
 */
export const ACTOR_SUFFIX = "_actor";

/**
 * Sentinel value for the source selector when the operator picks "Job".
 * The actual suffix is typed into a free-form input (the job type name).
 */
export const JOB_SOURCE = "_job";

export interface ScriptFormState {
  /** Trigger kind selector value (e.g. `record.create`). */
  kind: TriggerKind;
  /**
   * Suffix portion of the trigger id — usually an NSID (= a lexicon id),
   * or the literal `_actor` when `kind === "labeler.apply"` for
   * actor-level labels. For jobs, this is the user-typed job type name.
   */
  suffix: string;
  /**
   * Which source-selector value was chosen. Usually identical to `suffix`
   * (i.e. a lexicon NSID or `_actor`). For jobs this is `_job` while
   * `suffix` holds the free-form job type name.
   */
  source: string;
  description: string;
  body: string;
}

/**
 * Build a `ScriptFormState` from a backend `id` string + body. Returns
 * defaults if the id is malformed (so the form still renders something
 * editable).
 */
export function stateFromScript(args: {
  id: string;
  description: string | null | undefined;
  body: string;
}): ScriptFormState {
  const parsed = parseTriggerId(args.id);
  const kind = parsed?.kind ?? "record.index";
  const suffix = parsed?.suffix ?? "";
  let source = suffix;
  if (kind === "job.run") source = JOB_SOURCE;
  else if (suffix === ACTOR_SUFFIX) source = ACTOR_SUFFIX;
  return {
    kind,
    suffix,
    source,
    description: args.description ?? "",
    body: args.body,
  };
}

/** Recompose the trigger id from `(kind, suffix)`. */
export function composeTriggerId(state: ScriptFormState): string {
  return `${state.kind}:${state.suffix}`;
}

// ---------------------------------------------------------------------------
// Trigger-kind options per lexicon type
// ---------------------------------------------------------------------------

interface ActionOption {
  kind: TriggerKind;
  label: string;
}

const ACTOR_ACTIONS: ActionOption[] = [
  { kind: "labeler.apply", label: TRIGGER_KIND_LABELS["labeler.apply"] },
];

const RECORD_ACTIONS: ActionOption[] = [
  { kind: "record.index", label: "Default handler (any action)" },
  { kind: "record.create", label: "On create" },
  { kind: "record.update", label: "On update" },
  { kind: "record.delete", label: "On delete" },
  { kind: "labeler.apply", label: "On label applied" },
];

const QUERY_ACTIONS: ActionOption[] = [
  { kind: "xrpc.query", label: "Query handler" },
];

const PROCEDURE_ACTIONS: ActionOption[] = [
  { kind: "xrpc.procedure", label: "Procedure handler" },
];

const JOB_ACTIONS: ActionOption[] = [{ kind: "job.run", label: "Job runner" }];

const JOB_TYPE_PATTERN = /^[a-z0-9][a-z0-9._-]*$/;

export function isValidJobType(value: string): boolean {
  return (
    value.length > 0 && value.length <= 128 && JOB_TYPE_PATTERN.test(value)
  );
}

function actionsFor(
  source: string,
  lexicons: LexiconSummary[],
): ActionOption[] {
  if (source === ACTOR_SUFFIX) return ACTOR_ACTIONS;
  if (source === JOB_SOURCE) return JOB_ACTIONS;
  const lex = lexicons.find((l) => l.id === source);
  if (!lex) return [];
  switch (lex.lexicon_type) {
    case "record":
      return RECORD_ACTIONS;
    case "query":
      return QUERY_ACTIONS;
    case "procedure":
      return PROCEDURE_ACTIONS;
    default:
      return [];
  }
}

// ---------------------------------------------------------------------------
// Form
// ---------------------------------------------------------------------------

/**
 * Shared form for create + edit. The trigger id is the row's PK and
 * can't change after creation — to "rename" you delete and recreate.
 *
 * - When `idLocked` is true (detail page): the trigger id is rendered
 *   as plain text; only description + body are editable.
 * - When `idLocked` is false (new page): a Lexicon picker (with
 *   `Actor` at the top, then a divider, then every stored lexicon)
 *   composes the suffix; an Action picker filtered to the selected
 *   lexicon's type composes the kind.
 */
export function ScriptForm({
  state,
  onChange,
  idLocked,
  lexicons,
  lexiconsLoading,
}: {
  state: ScriptFormState;
  onChange: (next: ScriptFormState) => void;
  idLocked?: boolean;
  /** Required when `idLocked` is false; ignored otherwise. */
  lexicons?: LexiconSummary[];
  /** True while the lexicon list is being fetched. */
  lexiconsLoading?: boolean;
}) {
  return (
    <div className="flex flex-col flex-1 min-h-0 gap-4">
      {idLocked ? (
        <LockedTrigger triggerId={composeTriggerId(state)} />
      ) : (
        <TriggerComposer
          state={state}
          onChange={onChange}
          lexicons={lexicons ?? []}
          lexiconsLoading={lexiconsLoading}
        />
      )}

      <div className="flex flex-col gap-1">
        <Label htmlFor="description" className="text-xs">
          Description (optional)
        </Label>
        <Textarea
          id="description"
          value={state.description}
          onChange={(e) => onChange({ ...state, description: e.target.value })}
          maxLength={300}
          rows={2}
          placeholder="What does this script do?"
          className="text-sm"
        />
      </div>

      <div className="flex flex-col flex-1 min-h-[300px] gap-1">
        <Label htmlFor="body" className="text-xs">
          Lua body
        </Label>
        <div className="border rounded-md flex-1 min-h-[300px] overflow-hidden">
          <MonacoEditor
            value={state.body}
            onChange={(v) => onChange({ ...state, body: v })}
            language="lua"
            className="h-full"
          />
        </div>
      </div>
    </div>
  );
}

function LockedTrigger({ triggerId }: { triggerId: string }) {
  return (
    <div className="flex flex-col gap-1">
      <Label className="text-xs">Trigger</Label>
      <p className="font-mono text-sm">{triggerId}</p>
    </div>
  );
}

function TriggerComposer({
  state,
  onChange,
  lexicons,
  lexiconsLoading,
}: {
  state: ScriptFormState;
  onChange: (next: ScriptFormState) => void;
  lexicons: LexiconSummary[];
  lexiconsLoading?: boolean;
}) {
  const sortedLexicons = useMemo(
    () => [...lexicons].sort((a, b) => a.id.localeCompare(b.id)),
    [lexicons],
  );
  const actions = useMemo(
    () => actionsFor(state.source, lexicons),
    [state.source, lexicons],
  );

  const isJob = state.source === JOB_SOURCE;

  const stateRef = useRef(state);
  stateRef.current = state;

  useEffect(() => {
    if (actions.length === 0) return;
    const current = stateRef.current;
    if (!actions.some((a) => a.kind === current.kind)) {
      onChange({ ...current, kind: actions[0].kind });
    }
  }, [actions, onChange]);

  function handleSourceChange(next: string) {
    const wasJob = state.source === JOB_SOURCE;
    const isNowJob = next === JOB_SOURCE;
    const bodyIsDefault =
      state.body === DEFAULT_SCRIPT_BODY ||
      state.body === DEFAULT_JOB_SCRIPT_BODY;

    if (isNowJob) {
      onChange({
        ...state,
        source: JOB_SOURCE,
        suffix: "",
        kind: "job.run",
        body: bodyIsDefault ? DEFAULT_JOB_SCRIPT_BODY : state.body,
      });
      return;
    }
    const nextActions = actionsFor(next, lexicons);
    const nextKind = nextActions.some((a) => a.kind === state.kind)
      ? state.kind
      : (nextActions[0]?.kind ?? state.kind);
    onChange({
      ...state,
      source: next,
      suffix: next,
      kind: nextKind,
      body: wasJob && bodyIsDefault ? DEFAULT_SCRIPT_BODY : state.body,
    });
  }

  const triggerPreview =
    state.suffix && actions.length > 0 ? composeTriggerId(state) : null;

  return (
    <>
      <div className="grid gap-4 sm:grid-cols-2">
        <div className="flex flex-col gap-1">
          <Label htmlFor="source-pick" className="text-xs">
            Trigger source
          </Label>
          <Select value={state.source} onValueChange={handleSourceChange}>
            <SelectTrigger id="source-pick" size="sm" className="w-full">
              <SelectValue placeholder="Choose a source" />
            </SelectTrigger>
            <SelectContent>
              <SelectGroup>
                <SelectItem value={ACTOR_SUFFIX}>
                  Actor
                  <span className="text-muted-foreground ml-2 text-xs">
                    (labels on bare DIDs)
                  </span>
                </SelectItem>
                <SelectItem value={JOB_SOURCE}>
                  Job
                  <span className="text-muted-foreground ml-2 text-xs">
                    (background job runner)
                  </span>
                </SelectItem>
              </SelectGroup>
              <SelectSeparator />
              <SelectGroup>
                <SelectLabel className="text-xs">Lexicons</SelectLabel>
                {lexiconsLoading ? (
                  <SelectItem value="__loading__" disabled>
                    Loading…
                  </SelectItem>
                ) : sortedLexicons.length === 0 ? (
                  <SelectItem value="__no_lexicons__" disabled>
                    No lexicons yet
                  </SelectItem>
                ) : (
                  sortedLexicons.map((lex) => (
                    <SelectItem key={lex.id} value={lex.id}>
                      <span className="font-mono">{lex.id}</span>
                      <span className="text-muted-foreground ml-2 text-xs">
                        ({lex.lexicon_type})
                      </span>
                    </SelectItem>
                  ))
                )}
              </SelectGroup>
            </SelectContent>
          </Select>
          <p className="text-xs text-muted-foreground">
            What fires this script: a lexicon event, a label, or a background
            job.
          </p>
        </div>
        <div className="flex flex-col gap-1">
          {isJob ? (
            <>
              <Label htmlFor="job-type-input" className="text-xs">
                Job type
              </Label>
              <Input
                id="job-type-input"
                value={state.suffix}
                onChange={(e) => onChange({ ...state, suffix: e.target.value })}
                placeholder="e.g. export, migrate, sync"
                className="h-8 text-sm font-mono"
                aria-invalid={
                  state.suffix.length > 0 && !isValidJobType(state.suffix)
                }
              />
              {state.suffix.length > 0 && !isValidJobType(state.suffix) ? (
                <p className="text-xs text-destructive">
                  Lowercase letters, numbers, dots, hyphens, and underscores
                  only.
                </p>
              ) : (
                <p className="text-xs text-muted-foreground">
                  Must match the type passed to{" "}
                  <code className="bg-muted px-1 rounded font-mono text-xs">
                    jobs.create()
                  </code>{" "}
                  in the queuing script.
                </p>
              )}
            </>
          ) : (
            <>
              <Label htmlFor="action-pick" className="text-xs">
                Action
              </Label>
              <Select
                value={state.kind}
                onValueChange={(v) =>
                  onChange({ ...state, kind: v as TriggerKind })
                }
                disabled={actions.length <= 1}
              >
                <SelectTrigger id="action-pick" size="sm" className="w-full">
                  <SelectValue placeholder="Choose a source first" />
                </SelectTrigger>
                <SelectContent>
                  {actions.map((a) => (
                    <SelectItem key={a.kind} value={a.kind}>
                      {a.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </>
          )}
        </div>
      </div>

      <div>
        <Label className="text-xs text-muted-foreground">
          Resolved trigger id
        </Label>
        <p className="mt-1 font-mono text-sm">
          {triggerPreview ? (
            <Badge variant="outline">{triggerPreview}</Badge>
          ) : (
            <span className="text-muted-foreground">
              {isJob
                ? "Enter a job type to compose the trigger id."
                : "Pick a source to compose the trigger id."}
            </span>
          )}
        </p>
      </div>
    </>
  );
}
