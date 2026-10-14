"use client";

import { CloudOff, Loader2 } from "lucide-react";

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
import { cn } from "@/lib/utils";
import type { PublishableLexicon } from "@/types/lexicon-publishing";

import { isPending, lastFailure } from "./model";

export function canPublishNow(lexicon: PublishableLexicon): boolean {
  // Withdrawn is not pending, since auto-publish skips it, but publishing it
  // by hand is how an operator brings it back.
  if (isPending(lexicon) || lexicon.status === "withdrawn") return true;
  // A failed republish of a current record still deserves a retry.
  return lexicon.status === "current" && lastFailure(lexicon) !== null;
}

/** "Publish" for a lexicon with no record, "Republish" for one that has one. */
export function publishLabel(lexicon: PublishableLexicon): string {
  return lexicon.status === "unpublished" || lexicon.status === "withdrawn"
    ? "Publish"
    : "Republish";
}

export function isPublished(lexicon: PublishableLexicon): boolean {
  return Boolean(lexicon.publication?.uri);
}

/** Whether {@link RowActions} renders anything for this lexicon. */
export function hasRowActions(lexicon: PublishableLexicon): boolean {
  return canPublishNow(lexicon) || isPublished(lexicon);
}

export function RowActions({
  lexicon,
  busy,
  onPublish,
  onUnpublish,
  className,
}: {
  lexicon: PublishableLexicon;
  busy: boolean;
  onPublish: () => void;
  onUnpublish: () => void;
  className?: string;
}) {
  const publishable = canPublishNow(lexicon);
  const published = isPublished(lexicon);
  if (!hasRowActions(lexicon)) return null;

  return (
    <div
      className={cn("flex justify-end gap-1", className)}
      onClick={(e) => e.stopPropagation()}
    >
      {published && (
        <UnpublishButton
          nsid={lexicon.nsid}
          did={lexicon.publication?.did}
          autoPublish={lexicon.auto_publish}
          busy={busy}
          onConfirm={onUnpublish}
          compact
        />
      )}
      {publishable && (
        <Button size="sm" variant="outline" onClick={onPublish} disabled={busy}>
          {busy && (
            <Loader2
              className="animate-spin motion-reduce:animate-none"
              aria-hidden
            />
          )}
          {publishLabel(lexicon)}
        </Button>
      )}
    </div>
  );
}

export function UnpublishButton({
  nsid,
  did: where,
  autoPublish = false,
  busy,
  onConfirm,
  compact = false,
}: {
  nsid: string;
  /** The repo the record is in. */
  did: string | null | undefined;
  /** Whether a default would otherwise publish it again on its own. */
  autoPublish?: boolean;
  busy: boolean;
  onConfirm: () => void;
  /** Icon only, for table rows where width is scarce. */
  compact?: boolean;
}) {
  return (
    <AlertDialog>
      <AlertDialogTrigger asChild>
        <Button
          size={compact ? "icon-sm" : "sm"}
          variant="outline"
          className={cn(
            "hover:text-destructive",
            compact && "text-muted-foreground",
          )}
          disabled={busy}
          title={compact ? "Unpublish" : undefined}
          aria-label={compact ? `Unpublish ${nsid}` : undefined}
          onPointerDown={(e) => e.stopPropagation()}
        >
          {compact ? <CloudOff aria-hidden /> : "Unpublish"}
        </Button>
      </AlertDialogTrigger>
      <AlertDialogContent onClick={(e) => e.stopPropagation()}>
        <AlertDialogHeader>
          <AlertDialogTitle>Unpublish {nsid}?</AlertDialogTitle>
          <AlertDialogDescription>
            This deletes its schema record
            {where ? (
              <>
                {" "}
                from <span className="font-mono break-all">{where}</span>
              </>
            ) : null}
            . Other apps can no longer resolve this lexicon, and anything built
            against it may break.
            {autoPublish &&
              " Auto-publish won't bring it back; it stays unpublished until you publish it again."}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction variant="destructive" onClick={onConfirm}>
            Unpublish
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
