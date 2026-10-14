"use client";

import {
  type ColumnDef,
  type ColumnFiltersState,
  type PaginationState,
  type SortingState,
  getCoreRowModel,
  getFacetedRowModel,
  getFacetedUniqueValues,
  getFilteredRowModel,
  getPaginationRowModel,
  getSortedRowModel,
  useReactTable,
} from "@tanstack/react-table";
import { useCallback, useEffect, useMemo, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { FileCode, Info, Loader2 } from "lucide-react";

import { useCurrentUser } from "@/hooks/use-current-user";
import { useIsMobile } from "@/hooks/use-mobile";
import {
  deletePublishRule,
  getLexiconPublishing,
  getLexiconPublishingDns,
  updatePublishRule,
} from "@/lib/api";
import { toastError } from "@/lib/format";
import type {
  LexiconDnsEntry,
  LexiconPublishingOverview,
  PublishableLexicon,
  PublishRule,
  PublishStatus,
} from "@/types/lexicon-publishing";
import { DataTable } from "@/components/data-table/data-table";
import { DataTableColumnHeader } from "@/components/data-table/data-table-column-header";
import { DataTableSkeleton } from "@/components/data-table/data-table-skeleton";
import { DataTableToolbar } from "@/components/data-table/data-table-toolbar";
import { SiteHeader } from "@/components/site-header";
import { Button } from "@/components/ui/button";
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { DnsStatus, dnsState } from "@/components/lexicon-publishing/dns-status";
import {
  asSentence,
  expectedDid,
  isPending,
  nsidAuthority,
  targetName,
} from "@/components/lexicon-publishing/model";
import { PublishStatusBadge } from "@/components/lexicon-publishing/publish-status";
import { RoutingCascade } from "@/components/lexicon-publishing/routing-cascade";
import {
  RowActions,
  hasRowActions,
} from "@/components/lexicon-publishing/row-actions";
import { TargetName } from "@/components/lexicon-publishing/target-name";
import { RuleSheet } from "@/components/lexicon-publishing/rule-sheet";
import { usePublish } from "@/components/lexicon-publishing/use-publish";

const STATUS_OPTIONS: { label: string; value: PublishStatus }[] = [
  { label: "Published", value: "current" },
  { label: "Outdated", value: "outdated" },
  { label: "Published elsewhere", value: "moved" },
  { label: "Not published", value: "unpublished" },
  { label: "Withdrawn", value: "withdrawn" },
  { label: "Destination unavailable", value: "target_unavailable" },
  { label: "No default", value: "no_rule" },
];

export default function LexiconPublishingPage() {
  const { hasPermission } = useCurrentUser();
  const canPublish = hasPermission("lexicons:publish");
  const router = useRouter();

  const [overview, setOverview] = useState<LexiconPublishingOverview | null>(
    null,
  );
  const [loadFailed, setLoadFailed] = useState(false);
  const [dns, setDns] = useState<Map<string, LexiconDnsEntry> | null>(null);
  const [dnsFailure, setDnsFailure] = useState<string | null>(null);
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

  // DNS answers in seconds rather than milliseconds, so it loads on its own
  // and never holds up the table.
  const loadDns = useCallback(() => {
    getLexiconPublishingDns()
      .then((entries) => {
        setDns(new Map(entries.map((e) => [e.authority, e])));
        setDnsFailure(null);
      })
      .catch((e) => {
        setDns(new Map());
        setDnsFailure(e instanceof Error ? e.message : String(e));
      });
  }, []);

  useEffect(() => {
    load();
    loadDns();
  }, [load, loadDns]);

  const { busy, publish, unpublish, announcement } = usePublish(load);

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
      toastError("Failed to delete default", e);
    }
  }

  const lexicons = useMemo(() => overview?.lexicons ?? [], [overview]);
  const pending = useMemo(() => lexicons.filter(isPending), [lexicons]);
  const authorityCounts = useMemo(() => {
    const counts = new Map<string, number>();
    for (const l of lexicons) {
      const a = nsidAuthority(l.nsid);
      counts.set(a, (counts.get(a) ?? 0) + 1);
    }
    return counts;
  }, [lexicons]);

  const noTargets =
    overview !== null && !overview.targets.some((t) => t.available);

  const columns = useMemo<ColumnDef<PublishableLexicon>[]>(
    () => [
      {
        id: "nsid",
        accessorKey: "nsid",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} label="Lexicon" />
        ),
        // Below md the status column would sit under the pinned actions, so
        // the status moves into this cell instead.
        cell: ({ row }) => (
          <div className="flex flex-col gap-1">
            <span className="font-mono text-sm">{row.original.nsid}</span>
            <PublishStatusBadge
              lexicon={row.original}
              publishing={busy.has(row.original.nsid)}
              compact
              className="md:hidden"
            />
          </div>
        ),
        filterFn: "includesString",
        enableColumnFilter: true,
        enableHiding: false,
        meta: {
          label: "Lexicon",
          placeholder: "Filter by NSID...",
          variant: "text",
        },
      },
      {
        id: "status",
        accessorKey: "status",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} label="Status" />
        ),
        cell: ({ row }) => (
          <PublishStatusBadge
            lexicon={row.original}
            publishing={busy.has(row.original.nsid)}
          />
        ),
        filterFn: (row, columnId, filterValue) => {
          if (!Array.isArray(filterValue) || filterValue.length === 0)
            return true;
          return filterValue.includes(row.getValue(columnId));
        },
        enableColumnFilter: true,
        meta: {
          label: "Status",
          variant: "multiSelect",
          options: STATUS_OPTIONS,
          className: "max-md:hidden",
        },
      },
      {
        id: "destination",
        accessorFn: (l) => (l.target ? targetName(l.target) : ""),
        header: ({ column }) => (
          <DataTableColumnHeader column={column} label="Destination" />
        ),
        cell: ({ row }) => <DestinationCell lexicon={row.original} />,
      },
      {
        id: "dns",
        header: "DNS",
        cell: ({ row }) => {
          const l = row.original;
          const authority = nsidAuthority(l.nsid);
          const entry = dnsFailure
            ? {
                authority,
                record_name: `_lexicon.${authority}`,
                did: null,
                error: dnsFailure,
              }
            : dns?.get(authority);
          return (
            <DnsStatus
              state={dnsState(entry, dns === null, expectedDid(l))}
              coverCount={authorityCounts.get(authority) ?? 1}
            />
          );
        },
        enableSorting: false,
      },
      {
        id: "actions",
        header: () => <span className="sr-only">Actions</span>,
        cell: ({ row }) =>
          canPublish ? (
            <RowActions
              lexicon={row.original}
              busy={busy.has(row.original.nsid)}
              onPublish={() => publish([row.original.nsid])}
              onUnpublish={() => unpublish(row.original.nsid)}
            />
          ) : null,
        enableSorting: false,
        enableHiding: false,
      },
    ],
    [busy, canPublish, dns, dnsFailure, authorityCounts, publish, unpublish],
  );

  const [sorting, setSorting] = useState<SortingState>([
    { id: "nsid", desc: false },
  ]);
  const [columnFilters, setColumnFilters] = useState<ColumnFiltersState>([]);
  const [pagination, setPagination] = useState<PaginationState>({
    pageIndex: 0,
    pageSize: 20,
  });

  const table = useReactTable({
    data: lexicons,
    columns,
    state: {
      sorting,
      columnFilters,
      pagination,
    },
    initialState: { columnPinning: { right: ["actions"] } },
    defaultColumn: { enableColumnFilter: false },
    onSortingChange: setSorting,
    onColumnFiltersChange: setColumnFilters,
    onPaginationChange: setPagination,
    getCoreRowModel: getCoreRowModel(),
    getFilteredRowModel: getFilteredRowModel(),
    getSortedRowModel: getSortedRowModel(),
    getPaginationRowModel: getPaginationRowModel(),
    getFacetedRowModel: getFacetedRowModel(),
    getFacetedUniqueValues: getFacetedUniqueValues(),
    getRowId: (row) => row.nsid,
  });

  // Below md a pinned column covers the cells it floats over, so it pins only
  // when this page has a button in it; an empty pinned cell would hide text
  // for nothing.
  const isMobile = useIsMobile();
  useEffect(() => {
    const pageHasActions =
      canPublish &&
      table.getRowModel().rows.some((r) => hasRowActions(r.original));
    table.setColumnPinning(
      !isMobile || pageHasActions ? { right: ["actions"] } : {},
    );
  }, [table, isMobile, canPublish, lexicons, sorting, columnFilters, pagination]);

  const publishingPending = pending.some((l) => busy.has(l.nsid));

  return (
    <>
      <SiteHeader title="Lexicon publishing" backHref="/dashboard/lexicons" />
      <div className="flex flex-1 flex-col gap-10 p-4 md:p-6">
        <section
          aria-labelledby="publishing-defaults"
          className="flex w-full max-w-3xl flex-col gap-3"
        >
          <div className="flex flex-wrap items-end justify-between gap-x-4 gap-y-2">
            <div className="flex flex-col gap-1">
              <h2 id="publishing-defaults" className="text-lg font-semibold">
                Defaults
              </h2>
              <p className="text-muted-foreground text-sm">
                Each lexicon publishes to the first default that covers it,
                read from the top.
              </p>
            </div>
            {canPublish && overview && !noTargets && (
              <Button
                onClick={() => {
                  setEditing(null);
                  setSheetOpen(true);
                }}
              >
                Add default
              </Button>
            )}
          </div>

          {noTargets && <NoTargets />}

          {overview ? (
            <>
              <RoutingCascade
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
              {overview.rules.length === 0 && !noTargets && (
                <p className="text-muted-foreground text-sm">
                  Nothing publishes until a default names an account. Start
                  with <code className="font-mono">*</code> to send every
                  lexicon to one place.
                </p>
              )}
            </>
          ) : loadFailed ? (
            <LoadFailed onRetry={load} />
          ) : (
            <CascadeSkeleton />
          )}
        </section>

        <section
          aria-labelledby="publishing-lexicons"
          className="flex flex-col gap-3"
        >
          <div className="flex flex-col gap-1">
            <h2 id="publishing-lexicons" className="text-lg font-semibold">
              Lexicons
            </h2>
            <p className="text-muted-foreground text-sm">
              Lexicons this instance defines. Network lexicons belong to
              whoever published them.
            </p>
          </div>

          {!overview ? (
            loadFailed ? null : (
              <DataTableSkeleton
                columnCount={5}
                rowCount={6}
                filterCount={2}
                withViewOptions
                withPagination={false}
              />
            )
          ) : lexicons.length === 0 ? (
            <NoLexicons canCreate={hasPermission("lexicons:create")} />
          ) : (
            <DataTable
              table={table}
              onRowClick={(l) =>
                router.push(`/dashboard/lexicons/${encodeURIComponent(l.nsid)}`)
              }
            >
              <DataTableToolbar table={table}>
                {canPublish && (
                  <BulkPublish
                    count={pending.length}
                    busy={publishingPending}
                    onPublish={() => publish(pending.map((l) => l.nsid))}
                  />
                )}
              </DataTableToolbar>
            </DataTable>
          )}
        </section>

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
        />
      )}
    </>
  );
}

function DestinationCell({ lexicon }: { lexicon: PublishableLexicon }) {
  const { target, publication } = lexicon;
  if (!target) {
    return <span className="text-muted-foreground text-sm">—</span>;
  }
  return (
    <div className="flex min-w-0 max-w-64 flex-col">
      <TargetName target={target} className="text-sm" />
      {lexicon.status === "target_unavailable" ? (
        <span className="text-muted-foreground text-xs text-pretty">
          {asSentence(target.unavailable_reason)}
        </span>
      ) : lexicon.status === "moved" && publication?.did ? (
        <span className="text-muted-foreground truncate text-xs">
          Now in <span className="font-mono">{publication.did}</span>
        </span>
      ) : (
        lexicon.rule_pattern && (
          <span className="text-muted-foreground truncate text-xs">
            via <span className="font-mono">{lexicon.rule_pattern}</span>
          </span>
        )
      )}
    </div>
  );
}

function BulkPublish({
  count,
  busy,
  onPublish,
}: {
  count: number;
  busy: boolean;
  onPublish: () => void;
}) {
  const label =
    count === 0 ? "Publish pending" : `Publish ${count} pending`;
  if (count === 0) {
    // aria-disabled rather than disabled, so the button keeps focus and a
    // screen reader hears why it does nothing; the tooltip shows the same
    // reason to a pointer.
    const reason =
      "Nothing is pending: no covered lexicon is new, changed or moved.";
    return (
      <>
        <Tooltip>
          <TooltipTrigger asChild>
            <Button
              size="sm"
              aria-disabled="true"
              aria-describedby="bulk-publish-reason"
              className="aria-disabled:cursor-not-allowed aria-disabled:opacity-50 aria-disabled:hover:bg-primary"
              onClick={(e) => e.preventDefault()}
            >
              {label}
            </Button>
          </TooltipTrigger>
          <TooltipContent>{reason}</TooltipContent>
        </Tooltip>
        <span id="bulk-publish-reason" className="sr-only">
          {reason}
        </span>
      </>
    );
  }
  return (
    <Button size="sm" onClick={onPublish} disabled={busy}>
      {busy && (
        <Loader2 className="animate-spin motion-reduce:animate-none" aria-hidden />
      )}
      {label}
    </Button>
  );
}

function NoTargets() {
  return (
    <div className="bg-muted/40 flex items-start gap-3 rounded-lg border p-4 text-sm">
      <Info className="text-muted-foreground mt-0.5 size-4 shrink-0" aria-hidden />
      <p>
        Nothing can receive lexicons yet: attach an account in{" "}
        <Link
          href="/dashboard/settings/service-identity"
          className="font-medium underline underline-offset-4"
        >
          Service Identity
        </Link>
        , or link a repo with the lexicon publishing scope in{" "}
        <Link
          href="/dashboard/settings/linked-repos"
          className="font-medium underline underline-offset-4"
        >
          Linked Repos
        </Link>
        .
      </p>
    </div>
  );
}

function NoLexicons({ canCreate }: { canCreate: boolean }) {
  return (
    <Empty className="border">
      <EmptyHeader>
        <EmptyMedia variant="icon">
          <FileCode aria-hidden />
        </EmptyMedia>
        <EmptyTitle>No lexicons to publish</EmptyTitle>
        <EmptyDescription>
          Lexicons you add here, or define in the project, can be published.
          Network lexicons can&apos;t, since someone else owns them.
        </EmptyDescription>
      </EmptyHeader>
      {canCreate && (
        <EmptyContent>
          <Button asChild>
            <Link href="/dashboard/lexicons/new">Add Lexicon</Link>
          </Button>
        </EmptyContent>
      )}
    </Empty>
  );
}

function LoadFailed({ onRetry }: { onRetry: () => void }) {
  return (
    <div className="flex items-center gap-3 rounded-lg border p-4 text-sm">
      <p className="flex-1">Couldn&apos;t load publishing.</p>
      <Button variant="outline" size="sm" onClick={onRetry}>
        Retry
      </Button>
    </div>
  );
}

function CascadeSkeleton() {
  return (
    <div className="divide-y rounded-lg border" aria-busy="true">
      {[0, 1].map((i) => (
        <div key={i} className="flex items-center gap-4 px-4 py-3">
          <div className="flex flex-1 flex-col gap-1.5">
            <Skeleton className="h-4 w-40" />
            <Skeleton className="h-3 w-56" />
          </div>
          <div className="flex flex-1 flex-col gap-1.5">
            <Skeleton className="h-4 w-32" />
            <Skeleton className="h-3 w-48" />
          </div>
        </div>
      ))}
      <span className="sr-only">Loading defaults</span>
    </div>
  );
}
