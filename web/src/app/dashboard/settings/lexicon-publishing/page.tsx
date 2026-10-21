"use client";

import { useCallback, useEffect, useState } from "react";

import { useCurrentUser } from "@/hooks/use-current-user";
import {
  deletePublishRule,
  getLexiconPublishing,
  updatePublishRule,
} from "@/lib/api";
import { toastError } from "@/lib/format";
import type {
  LexiconPublishingOverview,
  PublishRule,
} from "@/types/lexicon-publishing";
import { DataTableSkeleton } from "@/components/data-table/data-table-skeleton";
import { SiteHeader } from "@/components/site-header";
import { Button } from "@/components/ui/button";
import { choosableTargets } from "@/components/lexicon-publishing/model";
import { OrphanedPublications } from "@/components/lexicon-publishing/orphaned-publications";
import { PublishingSetup } from "@/components/lexicon-publishing/publishing-setup";
import { RulesTable } from "@/components/lexicon-publishing/rules-table";
import { RuleSheet } from "@/components/lexicon-publishing/rule-sheet";
import { usePublish } from "@/components/lexicon-publishing/use-publish";

export default function LexiconPublishingPage() {
  const { hasPermission } = useCurrentUser();
  const canPublish = hasPermission("lexicons:publish");
  const canSeeLinkedRepos = hasPermission("linked-repos:read");

  const [overview, setOverview] = useState<LexiconPublishingOverview | null>(
    null,
  );
  const [loadFailed, setLoadFailed] = useState(false);
  const [sheetOpen, setSheetOpen] = useState(false);
  const [editing, setEditing] = useState<PublishRule | null>(null);
  const [pendingToggle, setPendingToggle] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setOverview(await getLexiconPublishing());
      setLoadFailed(false);
    } catch (e) {
      setLoadFailed(true);
      toastError("Failed to load lexicon publishing", e);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const { busy, unpublish, announcement } = usePublish(load);

  async function toggleAuto(rule: PublishRule, value: boolean) {
    setPendingToggle(rule.id);
    try {
      await updatePublishRule(rule.id, { auto_publish: value });
      await load();
    } catch (e) {
      toastError("Failed to change auto-publish", e);
    } finally {
      setPendingToggle(null);
    }
  }

  async function removeRule(rule: PublishRule) {
    try {
      await deletePublishRule(rule.id);
      await load();
    } catch (e) {
      toastError("Failed to delete rule", e);
    }
  }

  const noTargets =
    overview !== null &&
    !choosableTargets(overview.targets, canSeeLinkedRepos).some(
      (t) => t.available,
    );

  return (
    <>
      <SiteHeader title="Lexicon Publishing" />
      <div className="flex flex-1 flex-col gap-10 p-4 md:p-6">
        {overview && noTargets && (
          <PublishingSetup
            targets={overview.targets}
            canSeeLinkedRepos={canSeeLinkedRepos}
            canStillUnpublish={overview.usable}
            className="w-full max-w-3xl"
          />
        )}

        {!(overview && noTargets && overview.rules.length === 0) && (
          <section
            aria-labelledby="publishing-rules"
            className="flex w-full flex-col gap-3"
          >
            <div className="flex flex-wrap items-end justify-between gap-x-4 gap-y-2">
              <div className="flex flex-col gap-1">
                <h2 id="publishing-rules" className="text-lg font-semibold">
                  Publishing rules
                </h2>
                <p className="text-muted-foreground text-sm">
                  Each lexicon is published by the first rule that matches it.
                </p>
              </div>
              {canPublish && overview && !noTargets && (
                <Button
                  onClick={() => {
                    setEditing(null);
                    setSheetOpen(true);
                  }}
                >
                  Add rule
                </Button>
              )}
            </div>

            {overview ? (
              <RulesTable
                rules={overview.rules}
                canEdit={canPublish}
                pendingToggle={pendingToggle}
                onEdit={(rule) => {
                  setEditing(rule);
                  setSheetOpen(true);
                }}
                onDelete={removeRule}
                onToggleAuto={toggleAuto}
              />
            ) : loadFailed ? (
              <LoadFailed onRetry={load} />
            ) : (
              <DataTableSkeleton
                columnCount={4}
                rowCount={2}
                filterCount={0}
                withViewOptions={false}
                withPagination={false}
              />
            )}
          </section>
        )}

        {overview && (
          <OrphanedPublications
            orphans={overview.orphans}
            canPublish={canPublish}
            busy={busy}
            onUnpublish={unpublish}
          />
        )}

        <p className="sr-only" aria-live="polite" role="status">
          {announcement}
        </p>
      </div>

      {overview && canPublish && (
        <RuleSheet
          open={sheetOpen}
          onOpenChange={setSheetOpen}
          overview={overview}
          rule={editing}
          onSaved={load}
          canSeeLinkedRepos={canSeeLinkedRepos}
        />
      )}
    </>
  );
}

function LoadFailed({ onRetry }: { onRetry: () => void }) {
  return (
    <div className="flex items-center gap-3 rounded-lg border p-4 text-sm">
      <p className="flex-1">Couldn&apos;t load publishing rules.</p>
      <Button variant="outline" size="sm" onClick={onRetry}>
        Retry
      </Button>
    </div>
  );
}
