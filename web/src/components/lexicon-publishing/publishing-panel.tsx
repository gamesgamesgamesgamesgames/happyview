"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import { Loader2 } from "lucide-react";

import { getLexiconPublishing } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import type { PublishableLexicon } from "@/types/lexicon-publishing";

import { LexiconDnsCheck } from "./lexicon-dns-check";
import { nsidAuthority, unavailableSentence } from "./model";
import { NoTargets } from "./no-targets";
import { TargetName } from "./target-name";
import { PublishStatusBadge, statusExplanation } from "./publish-status";
import {
  canPublishNow,
  isPublished,
  publishLabel,
  UnpublishButton,
} from "./row-actions";
import { usePublish } from "./use-publish";

const PUBLISHING_HREF = "/dashboard/settings/lexicon-publishing";

/**
 * Where this lexicon publishes and why, beside the scripts that target it.
 * Best-effort like the scripts panel: a failed load leaves the editor usable.
 */
export function PublishingPanel({
  nsid,
  revision,
  canPublish,
}: {
  nsid: string;
  /** A saved edit changes the record's CID, so the status is re-read. */
  revision: number;
  canPublish: boolean;
}) {
  const [lexicon, setLexicon] = useState<PublishableLexicon | null>(null);
  const [usable, setUsable] = useState(true);
  const [coverCount, setCoverCount] = useState(1);
  const [state, setState] = useState<"loading" | "ready" | "failed">(
    "loading",
  );

  const authority = nsidAuthority(nsid);

  const read = useCallback(
    () =>
      getLexiconPublishing().then(
        (overview) => {
          setLexicon(overview.lexicons.find((l) => l.nsid === nsid) ?? null);
          setUsable(overview.usable);
          setCoverCount(
            overview.lexicons.filter((l) => nsidAuthority(l.nsid) === authority)
              .length || 1,
          );
          setState("ready");
        },
        () => setState("failed"),
      ),
    [nsid, authority],
  );

  useEffect(() => {
    read();
  }, [read, revision]);

  const { busy, publish, unpublish, announcement } = usePublish(read);
  const working = busy.has(nsid);

  return (
    <section aria-labelledby="publishing-panel-title">
      <Label
        id="publishing-panel-title"
        className="text-muted-foreground mb-3 block"
      >
        Publishing
      </Label>

      {state === "loading" ? (
        <div className="flex flex-col gap-2 rounded-md border p-3">
          <Skeleton className="h-4 w-28" />
          <Skeleton className="h-3 w-44" />
          <Skeleton className="h-3 w-36" />
        </div>
      ) : state === "failed" || !lexicon ? (
        <p className="text-muted-foreground rounded-md border border-dashed p-3 text-sm">
          Couldn&apos;t load publishing for this lexicon.
        </p>
      ) : !usable ? (
        <div className="flex flex-col gap-3 rounded-md border p-3">
          <NoTargets className="border-0 bg-transparent p-0" />
          <dl className="flex flex-col gap-2 text-sm">
            <DnsLine authority={authority}>
              <LexiconDnsCheck nsid={nsid} coverCount={coverCount} />
            </DnsLine>
          </dl>
        </div>
      ) : (
        <div className="flex flex-col gap-3 rounded-md border p-3">
          <div className="flex flex-col gap-1">
            <PublishStatusBadge lexicon={lexicon} publishing={working} />
            <p className="text-muted-foreground text-xs">
              {lexicon.status === "target_unavailable"
                ? unavailableSentence(lexicon.target)
                : statusExplanation(lexicon.status)}
            </p>
          </div>

          <dl className="flex flex-col gap-2 text-sm">
            <div className="flex flex-col gap-0.5">
              <dt className="text-muted-foreground text-xs">Destination</dt>
              <dd className="min-w-0">
                {lexicon.target ? (
                  <>
                    <TargetName target={lexicon.target} className="block" />
                    <span className="text-muted-foreground block text-xs">
                      Decided by{" "}
                      <Link
                        href={PUBLISHING_HREF}
                        className="font-mono underline underline-offset-4"
                      >
                        {lexicon.rule_pattern}
                      </Link>
                      {lexicon.auto_publish ? ", auto-publish on" : ""}
                    </span>
                  </>
                ) : (
                  <span className="text-muted-foreground text-xs">
                    No default covers it.{" "}
                    <Link
                      href={PUBLISHING_HREF}
                      className="text-foreground underline underline-offset-4"
                    >
                      {canPublish ? "Add one" : "See defaults"}
                    </Link>
                  </span>
                )}
              </dd>
            </div>
            <DnsLine authority={authority}>
              <LexiconDnsCheck nsid={nsid} coverCount={coverCount} />
            </DnsLine>
          </dl>

          {canPublish && (canPublishNow(lexicon) || isPublished(lexicon)) && (
            <div className="flex flex-wrap gap-2">
              {canPublishNow(lexicon) && (
                <Button
                  size="sm"
                  onClick={() => publish([nsid])}
                  disabled={working}
                >
                  {working && (
                    <Loader2
                      className="animate-spin motion-reduce:animate-none"
                      aria-hidden
                    />
                  )}
                  {publishLabel(lexicon)}
                </Button>
              )}
              {isPublished(lexicon) && (
                <UnpublishButton
                  nsid={lexicon.nsid}
                  did={lexicon.publication?.did}
                  autoPublish={lexicon.auto_publish}
                  busy={working}
                  onConfirm={() => unpublish(nsid)}
                />
              )}
            </div>
          )}
        </div>
      )}

      <p className="sr-only" aria-live="polite" role="status">
        {announcement}
      </p>
    </section>
  );
}

function DnsLine({
  authority,
  children,
}: {
  authority: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex flex-col gap-1">
      <dt className="text-muted-foreground text-xs">
        DNS <span className="font-mono">_lexicon.{authority}</span>
      </dt>
      <dd className="min-w-0">{children}</dd>
    </div>
  );
}
