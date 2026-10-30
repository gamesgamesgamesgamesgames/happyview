"use client";

import { Pencil, Trash2, TriangleAlert } from "lucide-react";

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
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import type { PublishRule } from "@/types/lexicon-publishing";

import { describeStoredPattern, nameIsDid, unavailableSentence } from "./model";
import { TargetName } from "./target-name";

const ACTIONS = "w-20";

/**
 * The rules in the order they win, most specific first, so the first row that
 * covers a lexicon is the one that decides where it goes. The closing row
 * says what happens to whatever none of them cover.
 */
export function RulesTable({
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
  const columns = canEdit ? 4 : 3;

  return (
    <div className="overflow-clip rounded-md border">
      <Table aria-label="Publishing rules, most specific first">
        <TableHeader>
          <TableRow>
            <TableHead>Pattern</TableHead>
            <TableHead>Publishes to</TableHead>
            <TableHead className="max-md:hidden">Auto-publish</TableHead>
            {canEdit && (
              <TableHead className={ACTIONS}>
                <span className="sr-only">Actions</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {rules.length === 0 && (
            <TableRow>
              <TableCell
                colSpan={columns}
                className="text-muted-foreground py-6 text-center whitespace-normal"
              >
                No publishing rules. Add a <code className="font-mono">*</code>{" "}
                rule to publish every lexicon to one account.
              </TableCell>
            </TableRow>
          )}

          {rules.map((rule) => (
            <TableRow key={rule.id} className="bg-background">
              <TableCell className="whitespace-normal">
                <p className="font-mono text-sm break-all">{rule.pattern}</p>
                <p className="text-muted-foreground text-xs">
                  {describeStoredPattern(rule.pattern)}
                </p>
                <p className="text-muted-foreground mt-1 text-xs md:hidden">
                  {rule.auto_publish ? "Auto-publish on" : "Auto-publish off"}
                </p>
              </TableCell>
              <TableCell className="whitespace-normal">
                <Destination rule={rule} />
              </TableCell>
              <TableCell className="max-md:hidden">
                <div className="flex items-center">
                  {canEdit ? (
                    <Switch
                      checked={rule.auto_publish}
                      disabled={pendingToggle === rule.id}
                      onCheckedChange={(v) => onToggleAuto(rule, v)}
                      aria-label={`Auto-publish lexicons under ${rule.pattern}`}
                    />
                  ) : (
                    <span className="text-sm">
                      {rule.auto_publish ? "On" : "Off"}
                    </span>
                  )}
                </div>
              </TableCell>
              {canEdit && (
                <TableCell className={ACTIONS}>
                  <div className="flex justify-end gap-1">
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={`Edit the rule for ${rule.pattern}`}
                      onClick={() => onEdit(rule)}
                    >
                      <Pencil aria-hidden />
                    </Button>
                    <DeleteRule rule={rule} onDelete={onDelete} />
                  </div>
                </TableCell>
              )}
            </TableRow>
          ))}

          {rules.length > 0 && !hasCatchAll && (
            <TableRow className="bg-muted/40 text-muted-foreground hover:bg-muted/40">
              <TableCell className="whitespace-normal">
                <p className="text-sm">Everything else</p>
                <p className="text-xs">Not matched by a rule above</p>
              </TableCell>
              <TableCell colSpan={columns - 1} className="text-sm">
                Not published
              </TableCell>
            </TableRow>
          )}
        </TableBody>
      </Table>
    </div>
  );
}

function Destination({ rule }: { rule: PublishRule }) {
  const { target } = rule;
  if (!target.available) {
    return (
      <div className="min-w-0">
        <p className="flex items-start gap-1.5 text-sm">
          <TriangleAlert
            className="text-warning mt-0.5 size-4 shrink-0"
            aria-hidden
          />
          Destination unavailable
        </p>
        <p className="text-muted-foreground text-xs text-pretty">
          {unavailableSentence(target)}
        </p>
      </div>
    );
  }
  return (
    <div className="min-w-0">
      <TargetName target={target} className="block text-sm" />
      {target.did && !nameIsDid(target) && (
        <p className="text-muted-foreground font-mono text-xs break-all max-md:hidden">
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
          aria-label={`Delete the rule for ${rule.pattern}`}
        >
          <Trash2 aria-hidden />
        </Button>
      </AlertDialogTrigger>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Delete this rule?</AlertDialogTitle>
          <AlertDialogDescription>
            Lexicons it covers will follow the next matching rule, or stop being
            published. Published records aren&apos;t removed.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction
            variant="destructive"
            onClick={() => onDelete(rule)}
          >
            Delete
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
