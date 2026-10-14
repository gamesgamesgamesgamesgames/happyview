"use client";

import { ArrowRight, Pencil, Trash2, TriangleAlert } from "lucide-react";

import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import type { PublishRule } from "@/types/lexicon-publishing";

import { describeStoredPattern, nameIsDid, unavailableSentence } from "./model";
import { TargetName } from "./target-name";

const ROW =
  "grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-4 gap-y-2 px-4 py-3 md:grid-cols-[minmax(0,1fr)_1rem_minmax(0,1fr)_8.5rem_4.25rem]";

/**
 * The defaults read top to bottom like a routing table: the first row that
 * covers a lexicon decides where it goes, and the closing row says what
 * happens to whatever none of them cover.
 */
export function RoutingCascade({
  rules,
  canEdit,
  pendingToggle,
  onEdit,
  onDelete,
  onToggleAuto,
}: {
  rules: PublishRule[];
  canEdit: boolean;
  pendingToggle: string | null;
  onEdit: (rule: PublishRule) => void;
  onDelete: (rule: PublishRule) => void;
  onToggleAuto: (rule: PublishRule, value: boolean) => void;
}) {
  const hasCatchAll = rules.some((r) => r.pattern === "*");

  return (
    <ol
      aria-label="Publishing defaults, most specific first"
      className="divide-y rounded-lg border"
    >
      {rules.map((rule) => (
        <li key={rule.id} className={ROW}>
          <div className="min-w-0">
            <p className="font-mono text-sm break-all">{rule.pattern}</p>
            <p className="text-muted-foreground text-xs">
              {describeStoredPattern(rule.pattern)}
            </p>
          </div>

          <ArrowRight
            className="text-muted-foreground hidden size-4 md:block"
            aria-hidden
          />

          <Destination rule={rule} />

          <div className="col-start-1 flex items-center gap-2 md:col-start-auto">
            {canEdit ? (
              <>
                <Switch
                  id={`auto-${rule.id}`}
                  checked={rule.auto_publish}
                  disabled={pendingToggle === rule.id}
                  onCheckedChange={(v) => onToggleAuto(rule, v)}
                />
                <label
                  htmlFor={`auto-${rule.id}`}
                  className="text-sm whitespace-nowrap"
                >
                  Auto-publish
                </label>
              </>
            ) : (
              <span className="text-muted-foreground text-sm whitespace-nowrap">
                {rule.auto_publish ? "Auto-publish on" : "Auto-publish off"}
              </span>
            )}
          </div>

          {canEdit ? (
            <div className="row-start-1 col-start-2 flex justify-end gap-1 md:row-start-auto md:col-start-auto">
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label={`Edit the default for ${rule.pattern}`}
                onClick={() => onEdit(rule)}
              >
                <Pencil aria-hidden />
              </Button>
              <DeleteRule rule={rule} onDelete={onDelete} />
            </div>
          ) : (
            <span className="hidden md:block" />
          )}
        </li>
      ))}

      {!hasCatchAll && (
        <li className={cn(ROW, "bg-muted/40 text-muted-foreground")}>
          <p className="text-sm">
            {rules.length === 0 ? "Every lexicon" : "Everything else"}
          </p>
          <ArrowRight className="hidden size-4 md:block" aria-hidden />
          <p className="text-sm">
            <span className="sr-only">: </span>Not published
          </p>
        </li>
      )}
    </ol>
  );
}

function Destination({ rule }: { rule: PublishRule }) {
  const { target } = rule;
  if (!target.available) {
    return (
      <div className="col-span-2 min-w-0 md:col-span-1">
        <p className="flex items-center gap-1.5 text-sm">
          <TriangleAlert
            className="text-warning size-4 shrink-0"
            aria-hidden
          />
          Destination unavailable
        </p>
        <p className="text-muted-foreground text-xs">
          {unavailableSentence(target)}
        </p>
      </div>
    );
  }
  return (
    <div className="col-span-2 min-w-0 md:col-span-1">
      <TargetName target={target} className="block text-sm" />
      {target.did && !nameIsDid(target) && (
        <p className="text-muted-foreground truncate font-mono text-xs">
          {target.did}
        </p>
      )}
    </div>
  );
}

function DeleteRule({
  rule,
  onDelete,
}: {
  rule: PublishRule;
  onDelete: (rule: PublishRule) => void;
}) {
  return (
    <AlertDialog>
      <AlertDialogTrigger asChild>
        <Button
          variant="ghost"
          size="icon-sm"
          className="hover:text-destructive"
          aria-label={`Delete the default for ${rule.pattern}`}
        >
          <Trash2 aria-hidden />
        </Button>
      </AlertDialogTrigger>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Delete this default?</AlertDialogTitle>
          <AlertDialogDescription>
            Lexicons it covers fall to the next default that matches, or stop
            publishing. Records already published stay where they are.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction variant="destructive" onClick={() => onDelete(rule)}>
            Delete
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
