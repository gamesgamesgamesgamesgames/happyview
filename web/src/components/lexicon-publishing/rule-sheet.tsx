"use client";

import { useMemo, useState } from "react";
import { Loader2 } from "lucide-react";
import { toast } from "sonner";

import { createPublishRule, updatePublishRule } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import type {
  LexiconPublishingOverview,
  PublishRule,
  PublishTarget,
} from "@/types/lexicon-publishing";

import {
  describePattern,
  matches,
  moreSpecific,
  nameIsDid,
  parsePattern,
  ruleTargetKey,
  targetKey,
} from "./model";
import { TargetName } from "./target-name";

interface RuleSheetProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  overview: LexiconPublishingOverview;
  /** The default being edited; absent to add one. */
  rule: PublishRule | null;
  onSaved: () => void;
}

export function RuleSheet(props: RuleSheetProps) {
  return (
    <Sheet open={props.open} onOpenChange={props.onOpenChange}>
      <SheetContent className="sm:max-w-xl flex flex-col overflow-hidden">
        {/* Keyed so every opening starts from the rule it edits, rather than
            whatever the previous session left behind. */}
        {props.open && (
          <RuleForm key={props.rule?.id ?? "new"} {...props} />
        )}
      </SheetContent>
    </Sheet>
  );
}

function RuleForm({ onOpenChange, overview, rule, onSaved }: RuleSheetProps) {
  const editing = rule !== null;
  const firstAvailable = overview.targets.find((t) => t.available);
  const [pattern, setPattern] = useState(rule?.pattern ?? "");
  const [destination, setDestination] = useState<string>(
    rule ? ruleTargetKey(rule) : firstAvailable ? targetKey(firstAvailable) : "",
  );
  const [autoPublish, setAutoPublish] = useState(rule?.auto_publish ?? true);
  const [touched, setTouched] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const parsed = parsePattern(pattern);
  const target = overview.targets.find((t) => targetKey(t) === destination);
  // A kept destination needs no target to match: one whose linked repo was
  // removed is in no list, and the default can still have auto-publish changed.
  const keepsDestination = editing && destination === ruleTargetKey(rule);
  // `null` keeps the rule's destination; `undefined` means nothing valid is
  // chosen.
  const newTarget: PublishTarget | null | undefined = keepsDestination
    ? null
    : target?.available
      ? target
      : undefined;
  const destinationValid = newTarget !== undefined;

  const coverage = useMemo(() => {
    if (!parsed.ok) return null;
    const others = overview.rules
      .filter((r) => r.id !== rule?.id)
      .map((r) => parsePattern(r.pattern))
      .flatMap((p) => (p.ok ? [p.pattern] : []));
    const matched = overview.lexicons.filter((l) =>
      matches(parsed.pattern, l.nsid),
    );
    const claimed = matched.filter((l) =>
      others.some((o) => matches(o, l.nsid) && moreSpecific(o, parsed.pattern)),
    );
    return { matched: matched.length, claimed: claimed.length };
  }, [parsed, overview, rule]);

  const canSave = !saving && (editing || parsed.ok) && destinationValid;

  async function submit(request: () => Promise<unknown>, done: string) {
    setSaving(true);
    setError(null);
    try {
      await request();
      toast.success(done);
      onSaved();
      onOpenChange(false);
    } catch (e) {
      // Shown in place rather than through toastError, which rewrites the
      // server's "already exists" into a generic sentence that drops the
      // pattern it collided with.
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  }

  function save() {
    setTouched(true);
    // canSave already excludes an undefined target; the check narrows it.
    if (!canSave || newTarget === undefined) return;
    const fields = newTarget && {
      target_kind: newTarget.kind,
      linked_repo_id:
        newTarget.kind === "linked_repo" ? newTarget.linked_repo_id : null,
    };
    if (rule) {
      submit(
        () =>
          updatePublishRule(rule.id, { ...fields, auto_publish: autoPublish }),
        "Default saved",
      );
    } else if (fields) {
      // A new default never keeps a destination, so fields is always set.
      submit(
        () =>
          createPublishRule({
            pattern: pattern.trim(),
            target_kind: fields.target_kind,
            linked_repo_id: fields.linked_repo_id ?? undefined,
            auto_publish: autoPublish,
          }),
        "Default added",
      );
    }
  }

  const patternError = !editing && touched && !parsed.ok ? parsed.error : null;

  return (
    <form
      className="flex min-h-0 flex-1 flex-col"
      onSubmit={(e) => {
        e.preventDefault();
        save();
      }}
    >
      <SheetHeader>
        <SheetTitle>{editing ? "Edit default" : "Add a default"}</SheetTitle>
        <SheetDescription>
          A default sends every lexicon its pattern covers to one account. The
          most specific default wins.
        </SheetDescription>
      </SheetHeader>

      <div className="flex flex-1 flex-col gap-6 overflow-y-auto px-4 pb-4">
        <div className="flex flex-col gap-2">
          <Label htmlFor="publish-rule-pattern">Pattern</Label>
          {editing ? (
            <p className="bg-muted/50 rounded-md border px-3 py-2 font-mono text-sm">
              {rule.pattern}
            </p>
          ) : (
            <Input
              id="publish-rule-pattern"
              value={pattern}
              onChange={(e) => setPattern(e.target.value)}
              onBlur={() => setTouched(true)}
              placeholder="com.example.*"
              autoComplete="off"
              spellCheck={false}
              autoFocus
              aria-invalid={patternError !== null}
              aria-describedby="publish-rule-pattern-help"
              className="font-mono"
            />
          )}
          <div
            id="publish-rule-pattern-help"
            className="flex flex-col gap-0.5 text-xs"
            aria-live="polite"
          >
            {patternError ? (
              <p className="text-destructive">{patternError}</p>
            ) : parsed.ok ? (
              <>
                <p>{describePattern(parsed.pattern)}</p>
                {coverage && (
                  <p className="text-muted-foreground">
                    {coverageSentence(coverage.matched, coverage.claimed)}
                  </p>
                )}
              </>
            ) : (
              <p className="text-muted-foreground">
                A full NSID, a prefix ending in{" "}
                <code className="font-mono">.*</code>, or{" "}
                <code className="font-mono">*</code> for everything.
              </p>
            )}
            {editing && (
              <p className="text-muted-foreground">
                A pattern can&apos;t change. Delete this default and add another
                to cover something else.
              </p>
            )}
          </div>
        </div>

        <fieldset className="flex flex-col gap-2">
          <legend className="mb-2 text-sm font-medium">Destination</legend>
          {editing && rule.target.available === false &&
            destination === ruleTargetKey(rule) && (
              <p className="text-muted-foreground text-xs">
                Its current destination is unavailable:{" "}
                {rule.target.unavailable_reason}. Choose another, or keep it
                and change only auto-publish.
              </p>
            )}
          <RadioGroup
            value={destination}
            onValueChange={setDestination}
            className="gap-2"
          >
            {overview.targets.map((t) => (
              <DestinationOption
                key={targetKey(t)}
                target={t}
                selected={destination === targetKey(t)}
              />
            ))}
          </RadioGroup>
        </fieldset>

        <div className="flex flex-col gap-1.5">
          <div className="flex items-center justify-between gap-4">
            <Label htmlFor="publish-rule-auto">Auto-publish</Label>
            <Switch
              id="publish-rule-auto"
              checked={autoPublish}
              onCheckedChange={setAutoPublish}
              aria-describedby="publish-rule-auto-help"
            />
          </div>
          <p id="publish-rule-auto-help" className="text-muted-foreground text-xs">
            {autoPublish
              ? "Matching lexicons publish whenever they change."
              : "Matching lexicons publish only when someone presses Publish."}
          </p>
        </div>

        {error && (
          <p
            role="alert"
            className="border-destructive/50 bg-destructive/10 text-destructive rounded-lg border p-3 text-sm"
          >
            {error}
          </p>
        )}
      </div>

      <SheetFooter className="flex-row justify-end">
        <Button
          type="button"
          variant="outline"
          onClick={() => onOpenChange(false)}
          disabled={saving}
        >
          Cancel
        </Button>
        <Button type="submit" disabled={!canSave}>
          {saving && (
            <Loader2 className="animate-spin motion-reduce:animate-none" aria-hidden />
          )}
          {editing ? "Save" : "Add default"}
        </Button>
      </SheetFooter>
    </form>
  );
}

function coverageSentence(matched: number, claimed: number): string {
  if (matched === 0) return "No lexicon here matches it yet.";
  const head =
    matched === 1 ? "Matches 1 lexicon here" : `Matches ${matched} lexicons here`;
  if (claimed === 0) return `${head}.`;
  return `${head}; a more specific default keeps ${claimed} of them.`;
}

function DestinationOption({
  target,
  selected,
}: {
  target: PublishTarget;
  selected: boolean;
}) {
  const id = `publish-dest-${targetKey(target)}`;
  return (
    <label
      htmlFor={id}
      className={cn(
        "flex items-start gap-3 rounded-md border p-3 transition-colors",
        target.available
          ? "hover:bg-accent/50 cursor-pointer"
          : "border-dashed cursor-not-allowed",
        selected && "border-ring bg-accent/30",
      )}
    >
      <RadioGroupItem
        id={id}
        value={targetKey(target)}
        disabled={!target.available}
        className="mt-0.5"
      />
      <span className="flex min-w-0 flex-col gap-0.5">
        <TargetName
          target={target}
          className={cn(
            "text-sm font-medium",
            !target.available && "text-muted-foreground",
          )}
        />
        {target.did && !nameIsDid(target) && (
          <span className="text-muted-foreground font-mono text-xs break-all">
            {target.did}
          </span>
        )}
        {target.unavailable_reason && (
          <span className="text-muted-foreground text-xs">
            Unavailable: {target.unavailable_reason}
          </span>
        )}
        {target.available && !target.can_unpublish && (
          <span className="text-muted-foreground text-xs">
            Can publish, but its grant can&apos;t delete, so unpublishing from
            here won&apos;t work.
          </span>
        )}
      </span>
    </label>
  );
}
