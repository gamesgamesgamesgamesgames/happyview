"use client";

import type { Publication } from "@/types/lexicon-publishing";

import { UnpublishButton } from "./row-actions";

/**
 * Records still in a repo after their lexicon was deleted. The lexicon's own
 * page is gone with it, so this is the only place they can be unpublished.
 */
export function OrphanedPublications({
  orphans,
  canPublish,
  busy,
  onUnpublish,
}: {
  orphans: Publication[];
  canPublish: boolean;
  busy: ReadonlySet<string>;
  onUnpublish: (nsid: string) => void;
}) {
  if (orphans.length === 0) return null;
  return (
    <section
      aria-labelledby="publishing-orphans"
      className="flex w-full max-w-3xl flex-col gap-3"
    >
      <div className="flex flex-col gap-1">
        <h2 id="publishing-orphans" className="text-lg font-semibold">
          Published records without a lexicon
        </h2>
        <p className="text-muted-foreground text-sm text-pretty">
          These lexicons were deleted after they were published. Their records
          remain until you unpublish them.
        </p>
      </div>
      <ul className="divide-y rounded-lg border">
        {orphans.map((p) => (
          <li
            key={p.nsid}
            className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2 px-4 py-3"
          >
            <div className="flex min-w-0 flex-col gap-0.5">
              <span className="font-mono text-sm break-all">{p.nsid}</span>
              {p.did && (
                <span className="text-muted-foreground text-xs">
                  In <span className="font-mono break-all">{p.did}</span>
                </span>
              )}
            </div>
            {canPublish && (
              <UnpublishButton
                nsid={p.nsid}
                did={p.did}
                busy={busy.has(p.nsid)}
                onConfirm={() => onUnpublish(p.nsid)}
              />
            )}
          </li>
        ))}
      </ul>
    </section>
  );
}
