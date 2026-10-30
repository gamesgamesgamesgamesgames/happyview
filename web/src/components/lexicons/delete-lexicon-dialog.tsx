"use client";

import { type ReactNode, useState } from "react";
import { Trash2 } from "lucide-react";

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
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import { getLexiconRecordCount } from "@/lib/api";
import type { LexiconSummary } from "@/types/lexicons";

type Props = {
  lexicon: LexiconSummary;
  /** Whether this user may also delete the collection's records. */
  canDeleteRecords: boolean;
  onConfirm: (lexicon: LexiconSummary, deleteRecords: boolean) => void;
  /**
   * What opens the dialog. The lexicons table's row action by default; the
   * detail page passes its own full-width button. Both share this dialog so
   * the two entry points cannot drift apart again.
   */
  trigger?: ReactNode;
};

export function DeleteLexiconDialog({
  lexicon,
  canDeleteRecords,
  onConfirm,
  trigger,
}: Props) {
  const [deleteRecords, setDeleteRecords] = useState(false);
  const [count, setCount] = useState<number | null>(null);
  const [countFailed, setCountFailed] = useState(false);
  /** Null until the count answers; false when a shared store serves it. */
  const [deletable, setDeletable] = useState<boolean | null>(null);
  const [notDeletable, setNotDeletable] = useState<string | null>(null);

  // Only a record lexicon has records of its own. A query or procedure
  // lexicon reads another lexicon's collection through target_collection, so
  // offering to delete "its" records would delete someone else's.
  const hasRecords = lexicon.lexicon_type === "record";
  // Whether the records are this instance's to delete is the server's call,
  // not a guess from the lexicon's source: a collection a shared record
  // store serves is refused, and the count endpoint says so. Until it
  // answers, nothing is offered — better a moment's wait than a checkbox
  // that turns out to be refused.
  const offerRecords = hasRecords && canDeleteRecords && deletable === true;

  // Counting is a COUNT(*) over the collection, so it waits for the dialog
  // rather than loading with the table.
  function onOpenChange(open: boolean) {
    if (!open) {
      setDeleteRecords(false);
      return;
    }
    if (!hasRecords || !canDeleteRecords || count !== null) return;
    getLexiconRecordCount(lexicon.id)
      .then((r) => {
        setCount(r.count);
        setDeletable(r.deletable);
        setNotDeletable(r.not_deletable_reason);
      })
      .catch(() => setCountFailed(true));
  }

  const recordLabel =
    count === null
      ? countFailed
        ? "Also delete this collection's indexed records"
        : "Also delete this collection's indexed records (counting…)"
      : `Also delete this collection's ${count.toLocaleString()} indexed ${
          count === 1 ? "record" : "records"
        }`;

  return (
    <AlertDialog onOpenChange={onOpenChange}>
      <AlertDialogTrigger asChild>
        {trigger ?? (
          <Button
            variant="destructive"
            size="icon"
            className="size-8 text-muted-foreground hover:text-destructive"
            title="Delete lexicon"
            aria-label="Delete lexicon"
            onClick={(e) => e.stopPropagation()}
            onPointerDown={(e) => e.stopPropagation()}
          >
            <Trash2 className="size-4" />
          </Button>
        )}
      </AlertDialogTrigger>
      <AlertDialogContent onClick={(e) => e.stopPropagation()}>
        <AlertDialogHeader>
          <AlertDialogTitle>Delete lexicon?</AlertDialogTitle>
          <AlertDialogDescription>
            This will permanently remove the lexicon and its associated routes.
            This action cannot be undone.
          </AlertDialogDescription>
        </AlertDialogHeader>

        {offerRecords && (
          <div className="space-y-2 rounded-md border p-3">
            <div className="flex items-start gap-3">
              <Checkbox
                id={`delete-records-${lexicon.id}`}
                checked={deleteRecords}
                onCheckedChange={(v) => setDeleteRecords(v === true)}
              />
              <Label
                htmlFor={`delete-records-${lexicon.id}`}
                className="text-sm leading-snug font-normal"
              >
                {recordLabel}
              </Label>
            </div>
            <p className="text-muted-foreground text-xs">
              {deleteRecords
                ? "The records are deleted in the background as a job you can follow, pause or cancel on the Jobs page. Records a script kept after the source deleted them are included."
                : "Left unchecked, the records stay indexed and queryable until you delete them yourself."}
            </p>
          </div>
        )}
        {hasRecords && !offerRecords && (
          <p className="text-muted-foreground text-xs">
            {!canDeleteRecords
              ? "The collection's indexed records will stay. Deleting them needs the records:delete-collection permission."
              : countFailed
                ? "The collection's indexed records will stay: its record count could not be read."
                : (notDeletable ??
                  "Checking whether this collection's records can be deleted…")}
          </p>
        )}

        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction
            variant="destructive"
            onClick={() => onConfirm(lexicon, offerRecords && deleteRecords)}
          >
            {offerRecords && deleteRecords ? "Delete both" : "Delete"}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
