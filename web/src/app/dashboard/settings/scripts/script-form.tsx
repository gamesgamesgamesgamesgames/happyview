"use client";

import { useEffect, useMemo } from "react";

import { MonacoEditor } from "@/components/monaco-editor";
import { Badge } from "@/components/ui/badge";
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
import { TRIGGER_KIND_LABELS, parseTriggerId } from "@/types/scripts";

/**
 * Sentinel suffix used when the operator picks "Actor" in the lexicon
 * dropdown. Combined with `kind = "labeler.apply"` it yields the
 * `labeler.apply:_actor` trigger.
 */
export const ACTOR_SUFFIX = "_actor";

export interface ScriptFormState {
  /** Trigger kind selector value (e.g. `record.create`). */
  kind: TriggerKind;
  /**
   * Suffix portion of the trigger id — usually an NSID (= a lexicon id),
   * or the literal `_actor` when `kind === "labeler.apply"` for
   * actor-level labels.
   */
  suffix: string;
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
  return {
    kind: parsed?.kind ?? "record.index",
    suffix: parsed?.suffix ?? "",
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

function actionsFor(suffix: string, lexicons: LexiconSummary[]): ActionOption[] {
  if (suffix === ACTOR_SUFFIX) return ACTOR_ACTIONS;
  const lex = lexicons.find((l) => l.id === suffix);
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
}: {
  state: ScriptFormState;
  onChange: (next: ScriptFormState) => void;
  idLocked?: boolean;
  /** Required when `idLocked` is false; ignored otherwise. */
  lexicons?: LexiconSummary[];
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
}: {
  state: ScriptFormState;
  onChange: (next: ScriptFormState) => void;
  lexicons: LexiconSummary[];
}) {
  const sortedLexicons = useMemo(
    () => [...lexicons].sort((a, b) => a.id.localeCompare(b.id)),
    [lexicons],
  );
  const actions = useMemo(
    () => actionsFor(state.suffix, lexicons),
    [state.suffix, lexicons],
  );

  // If the current kind doesn't match the actions valid for the current
  // suffix (e.g. user just switched lexicon, the old kind is invalid),
  // snap to the first available action.
  useEffect(() => {
    if (actions.length === 0) return;
    if (!actions.some((a) => a.kind === state.kind)) {
      onChange({ ...state, kind: actions[0].kind });
    }
    // We deliberately omit `state` from deps to avoid re-running when
    // the user edits other fields (description / body).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [actions, state.suffix]);

  function handleSuffixChange(next: string) {
    // Pre-snap kind so the resolved trigger id badge updates immediately
    // rather than flickering through an invalid state.
    const nextActions = actionsFor(next, lexicons);
    const nextKind = nextActions.some((a) => a.kind === state.kind)
      ? state.kind
      : (nextActions[0]?.kind ?? state.kind);
    onChange({ ...state, suffix: next, kind: nextKind });
  }

  const triggerPreview =
    state.suffix && actions.length > 0 ? composeTriggerId(state) : null;

  return (
    <>
      <div className="grid gap-4 sm:grid-cols-2">
        <div className="flex flex-col gap-1">
          <Label htmlFor="lexicon-pick" className="text-xs">
            Lexicon
          </Label>
          <Select value={state.suffix} onValueChange={handleSuffixChange}>
            <SelectTrigger id="lexicon-pick" size="sm" className="w-full">
              <SelectValue placeholder="Choose a lexicon" />
            </SelectTrigger>
            <SelectContent>
              <SelectGroup>
                <SelectItem value={ACTOR_SUFFIX}>
                  Actor
                  <span className="text-muted-foreground ml-2 text-xs">
                    (labels on bare DIDs)
                  </span>
                </SelectItem>
              </SelectGroup>
              <SelectSeparator />
              <SelectGroup>
                <SelectLabel className="text-xs">Lexicons</SelectLabel>
                {sortedLexicons.length === 0 ? (
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
        </div>
        <div className="flex flex-col gap-1">
          <Label htmlFor="action-pick" className="text-xs">
            Action
          </Label>
          <Select
            value={state.kind}
            onValueChange={(v) => onChange({ ...state, kind: v as TriggerKind })}
            disabled={actions.length <= 1}
          >
            <SelectTrigger id="action-pick" size="sm" className="w-full">
              <SelectValue placeholder="Pick a lexicon first" />
            </SelectTrigger>
            <SelectContent>
              {actions.map((a) => (
                <SelectItem key={a.kind} value={a.kind}>
                  {a.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
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
              Pick a lexicon to compose the trigger id.
            </span>
          )}
        </p>
      </div>
    </>
  );
}
