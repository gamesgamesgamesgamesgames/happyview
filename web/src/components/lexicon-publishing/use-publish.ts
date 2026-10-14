"use client";

import { useCallback, useState } from "react";
import { toast } from "sonner";

import { ApiError, publishLexicons, unpublishLexicon } from "@/lib/api";
import { toastError } from "@/lib/format";
import type { PublishOutcome } from "@/types/lexicon-publishing";

/** The server's MAX_PUBLISH_BATCH; a longer list is refused whole. */
const MAX_PUBLISH_BATCH = 500;

/**
 * Publish and unpublish with per-NSID progress, plus a sentence for an
 * `aria-live` region: a toast is announced once and gone, while the region
 * keeps the last outcome readable beside the rows it changed.
 */
export function usePublish(onChanged: () => Promise<void> | void) {
  const [busy, setBusy] = useState<ReadonlySet<string>>(new Set());
  const [announcement, setAnnouncement] = useState("");

  const mark = useCallback((nsids: string[], on: boolean) => {
    setBusy((prev) => {
      const next = new Set(prev);
      for (const n of nsids) {
        if (on) next.add(n);
        else next.delete(n);
      }
      return next;
    });
  }, []);

  const publish = useCallback(
    async (nsids: string[]) => {
      if (nsids.length === 0) return;
      mark(nsids, true);
      const results: PublishOutcome[] = [];
      try {
        for (let i = 0; i < nsids.length; i += MAX_PUBLISH_BATCH) {
          const batch = await publishLexicons(
            nsids.slice(i, i + MAX_PUBLISH_BATCH),
          );
          results.push(...batch.results);
        }
        const failed = results.filter((r) => !r.ok);
        const ok = results.length - failed.length;
        const sentence =
          failed.length === 0
            ? ok === 1
              ? `Published ${results[0].nsid}`
              : `Published ${ok} lexicons`
            : ok === 0
              ? failed.length === 1
                ? `Couldn't publish ${failed[0].nsid}`
                : `Couldn't publish ${failed.length} lexicons`
              : `Published ${ok}; ${failed.length} failed`;
        setAnnouncement(sentence);
        if (failed.length === 0) {
          toast.success(sentence);
        } else {
          toast.error(sentence, {
            description:
              failed.length === 1
                ? (failed[0].error ?? undefined)
                : "Each failed row shows its error.",
          });
        }
      } catch (e) {
        // Earlier batches have already published, so say how far it got.
        const ok = results.filter((r) => r.ok).length;
        setAnnouncement(
          ok === 0 ? "Publishing failed" : `Published ${ok}, then publishing failed`,
        );
        toastError("Failed to publish", e);
      } finally {
        // Reload before clearing, so a row goes from "Publishing…" straight
        // to its new status instead of flashing the old one.
        await onChanged();
        mark(nsids, false);
      }
    },
    [mark, onChanged],
  );

  const unpublish = useCallback(
    async (nsid: string) => {
      mark([nsid], true);
      try {
        await unpublishLexicon(nsid);
        setAnnouncement(`Unpublished ${nsid}`);
        toast.success(`Unpublished ${nsid}`);
      } catch (e) {
        setAnnouncement(`Couldn't unpublish ${nsid}`);
        // The server's sentence names the account or grant at fault, which
        // toastError's keyword rewrites would replace with a generic one.
        if (e instanceof ApiError && e.status !== 403) {
          toast.error(`Couldn't unpublish ${nsid}`, { description: e.message });
        } else {
          toastError("Failed to unpublish", e);
        }
      } finally {
        await onChanged();
        mark([nsid], false);
      }
    },
    [mark, onChanged],
  );

  return { busy, publish, unpublish, announcement };
}
