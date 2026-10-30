"use client";

import { useState } from "react";
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
};

export function DeleteLexiconDialog({
  lexicon,
  canDeleteRecords,
  onConfirm,
}: Props) {
  const [deleteRecords, setDeleteRecords] = useState(false);
  const [count, setCount] = useState<number | null>(null);
  const [countFailed, setCountFailed] = useState(false);

  // Only a record lexicon has records of its own. A query or procedure
  // lexicon reads another lexicon's collection through target_collection, so
  // offering to delete "its" records would delete someone else's.
  const hasRecords = lexicon.lexicon_type === "record";
  const offerRecords = hasRecords && canDeleteRecords;

  // Counting is a COUNT(*) over the collection, so it waits for the dialog
  // rather than loading with the table.
  function onOpenChange(open: boolean) {
    if (!open) {
      setDeleteRecords(false);
      return;
    }
    if (!offerRecords || count !== null) return;
    getLexiconRecordCount(lexicon.id)
      .then((r) => setCount(r.count))
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
        {hasRecords && !canDeleteRecords && (
          <p className="text-muted-foreground text-xs">
            The collection&apos;s indexed records will stay. Deleting them needs
            the <code>records:delete-collection</code> permission.
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
