"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  type ColumnDef,
  type ColumnFiltersState,
  type RowSelectionState,
  type VisibilityState,
  getCoreRowModel,
  useReactTable,
} from "@tanstack/react-table";
import { toast } from "sonner";

import { toastError } from "@/lib/format";
import {
  getDeadLetters,
  getDeadLetter,
  retryDeadLetter,
  reindexDeadLetter,
  dismissDeadLetter,
  bulkRetryDeadLetters,
  bulkReindexDeadLetters,
  bulkDismissDeadLetters,
} from "@/lib/api";
import type { DeadLetterSummary, DeadLetterDetail } from "@/types/dead-letters";
import { DataTable } from "@/components/data-table/data-table";
import { DataTableColumnHeader } from "@/components/data-table/data-table-column-header";
import { DataTableToolbar } from "@/components/data-table/data-table-toolbar";
import { CodeBlock } from "@/components/code-block";
import { MonacoEditor } from "@/components/monaco-editor";
import { SiteHeader } from "@/components/site-header";
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
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Sheet,
  SheetContent,
  SheetFooter,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  ChevronLeft,
  ChevronRight,
  RotateCcw,
  RefreshCw,
  XCircle,
  MoreHorizontal,
} from "lucide-react";
import { Checkbox } from "@/components/ui/checkbox";

function actionBadge(action: string) {
  switch (action) {
    case "delete":
      return <Badge variant="destructive">delete</Badge>;
    case "update":
      return (
        <Badge className="bg-amber-500/15 text-amber-700 dark:text-amber-400 hover:bg-amber-500/25 border-amber-500/20">
          update
        </Badge>
      );
    default:
      return <Badge variant="secondary">{action}</Badge>;
  }
}

function timeAgo(dateStr: string): string {
  const now = Date.now();
  const then = new Date(dateStr).getTime();
  const seconds = Math.floor((now - then) / 1000);
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}

function DetailBody({ detail }: { detail: DeadLetterDetail }) {
  return (
    <div className="flex flex-col gap-4 flex-1 min-h-0">
      <div className="grid grid-cols-2 gap-4 text-sm">
        <div>
          <span className="text-muted-foreground">URI</span>
          <p className="font-mono text-xs break-all">{detail.uri}</p>
        </div>
        <div>
          <span className="text-muted-foreground">DID</span>
          <p className="font-mono text-xs break-all">{detail.did}</p>
        </div>
        <div>
          <span className="text-muted-foreground">Collection</span>
          <p className="font-mono text-xs">{detail.collection}</p>
        </div>
        <div>
          <span className="text-muted-foreground">Lexicon</span>
          <p className="font-mono text-xs">{detail.lexicon_id}</p>
        </div>
        <div>
          <span className="text-muted-foreground">Action</span>
          <p>{actionBadge(detail.action)}</p>
        </div>
        <div>
          <span className="text-muted-foreground">Attempts</span>
          <p className="text-xs tabular-nums">{detail.attempts}</p>
        </div>
        <div>
          <span className="text-muted-foreground">Created</span>
          <p className="text-xs">
            {new Date(detail.created_at).toLocaleString()}
          </p>
        </div>
        {detail.resolved_at && (
          <div>
            <span className="text-muted-foreground">Resolved</span>
            <p className="text-xs">
              {new Date(detail.resolved_at).toLocaleString()}
            </p>
          </div>
        )}
      </div>

      <div>
        <span className="text-muted-foreground text-sm">Error</span>
        <div className="bg-destructive/10 text-destructive mt-1 rounded-md p-3 font-mono text-xs whitespace-pre-wrap">
          {detail.error}
        </div>
      </div>

      {detail.record && (
        <div className="flex flex-col flex-1 min-h-0">
          <span className="text-muted-foreground text-sm">Record</span>
          <MonacoEditor
            value={JSON.stringify(detail.record, null, 2)}
            language="json"
            readOnly
            className="mt-1 flex-1 min-h-[200px] rounded-md overflow-hidden border"
          />
        </div>
      )}
    </div>
  );
}

export default function DeadLettersPage() {
  const [items, setItems] = useState<DeadLetterSummary[]>([]);
  const [loading, setLoading] = useState(false);
  const [viewDetail, setViewDetail] = useState<DeadLetterDetail | null>(null);
  const [actionLoading, setActionLoading] = useState(false);
  const [resolvedFilter, setResolvedFilter] = useState("false");
  const [rowSelection, setRowSelection] = useState<RowSelectionState>({});
  const [dismissAllOpen, setDismissAllOpen] = useState(false);

  const [cursorStack, setCursorStack] = useState<string[]>([]);
  const [nextCursor, setNextCursor] = useState<string | null>(null);

  const [columnFilters, setColumnFilters] = useState<ColumnFiltersState>([]);
  const [columnVisibility, setColumnVisibility] = useState<VisibilityState>({});
  const debounceRef = useRef<ReturnType<typeof setTimeout>>(null);
  const [debouncedFilters, setDebouncedFilters] =
    useState<ColumnFiltersState>(columnFilters);

  useEffect(() => {
    if (debounceRef.current) clearTimeout(debounceRef.current);
    debounceRef.current = setTimeout(() => {
      setDebouncedFilters(columnFilters);
    }, 300);
  }, [columnFilters]);

  const collectionFilter = useMemo(
    () =>
      (
        debouncedFilters.find((f) => f.id === "collection")?.value as
          | string[]
          | undefined
      )?.join(",") || undefined,
    [debouncedFilters],
  );

  const fetchItems = useCallback(
    async (cursor?: string) => {
      setLoading(true);
      try {
        const data = await getDeadLetters({
          collection: collectionFilter,
          resolved: resolvedFilter,
          cursor,
          limit: 50,
        });
        setItems(data.dead_letters);
        setNextCursor(data.cursor);
        setRowSelection({});
      } catch (e: unknown) {
        toastError("Failed to load dead letters", e);
        setItems([]);
        setNextCursor(null);
      } finally {
        setLoading(false);
      }
    },
    [collectionFilter, resolvedFilter],
  );

  useEffect(() => {
    setCursorStack([]);
    fetchItems();
  }, [fetchItems]);

  useEffect(() => {
    if (Object.keys(rowSelection).length === 0) return;
    const validIds = new Set(items.map((item) => item.id));
    const pruned: RowSelectionState = {};
    let changed = false;
    for (const [id, selected] of Object.entries(rowSelection)) {
      if (validIds.has(id)) {
        pruned[id] = selected;
      } else {
        changed = true;
      }
    }
    if (changed) setRowSelection(pruned);
  }, [items]);

  function handleNext() {
    if (!nextCursor) return;
    setCursorStack((prev) => [...prev, nextCursor]);
    fetchItems(nextCursor);
  }

  function handlePrevious() {
    if (cursorStack.length === 0) return;
    const stack = [...cursorStack];
    stack.pop();
    const prevCursor = stack.length > 0 ? stack[stack.length - 1] : undefined;
    setCursorStack(stack);
    fetchItems(prevCursor);
  }

  async function openDetail(row: DeadLetterSummary) {
    try {
      const detail = await getDeadLetter(row.id);
      setViewDetail(detail);
    } catch (e: unknown) {
      toastError("Failed to load dead letter detail", e);
    }
  }

  async function handleDetailAction(action: "retry" | "reindex" | "dismiss") {
    if (!viewDetail) return;
    setActionLoading(true);
    try {
      if (action === "retry") await retryDeadLetter(viewDetail.id);
      else if (action === "reindex") await reindexDeadLetter(viewDetail.id);
      else await dismissDeadLetter(viewDetail.id);
      toast.success(action === "retry" ? "Dead letter retried" : action === "reindex" ? "Dead letter re-indexed" : "Dead letter dismissed");
      setViewDetail(null);
      fetchItems();
    } catch (e: unknown) {
      toastError(`Failed to ${action} dead letter`, e);
    } finally {
      setActionLoading(false);
    }
  }

  const selectedIds = useMemo(
    () => Object.keys(rowSelection).filter((k) => rowSelection[k]),
    [rowSelection],
  );

  async function handleBulkAction(
    action: "retry" | "reindex" | "dismiss",
    scope: "selected" | "all",
  ) {
    setLoading(true);
    try {
      const count = selectedIds.length;
      const body =
        scope === "all"
          ? { all: true, collection: collectionFilter }
          : { ids: selectedIds };
      if (action === "retry") await bulkRetryDeadLetters(body);
      else if (action === "reindex") await bulkReindexDeadLetters(body);
      else await bulkDismissDeadLetters(body);
      const verb = action === "retry" ? "retried" : action === "reindex" ? "re-indexed" : "dismissed";
      toast.success(scope === "all"
        ? `All matching dead letters ${verb}`
        : `${count} dead ${count === 1 ? "letter" : "letters"} ${verb}`);
      setRowSelection({});
      fetchItems();
    } catch (e: unknown) {
      toastError(`Failed to ${action} dead letters`, e);
    } finally {
      setLoading(false);
    }
  }

  const columns = useMemo<ColumnDef<DeadLetterSummary>[]>(
    () => [
      {
        id: "select",
        header: ({ table }) => (
          <Checkbox
            checked={
              table.getIsAllPageRowsSelected() ||
              (table.getIsSomePageRowsSelected() && "indeterminate")
            }
            onCheckedChange={(value) =>
              table.toggleAllPageRowsSelected(!!value)
            }
            aria-label="Select all"
          />
        ),
        cell: ({ row }) => (
          <div onClick={(e) => e.stopPropagation()}>
            <Checkbox
              checked={row.getIsSelected()}
              onCheckedChange={(value) => row.toggleSelected(!!value)}
              aria-label="Select row"
            />
          </div>
        ),
        enableSorting: false,
        enableColumnFilter: false,
        size: 32,
      },
      {
        id: "collection",
        accessorKey: "collection",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} label="Collection" />
        ),
        cell: ({ row }) => (
          <span
            className="font-mono text-xs block max-w-[200px] truncate"
            title={row.original.collection}
          >
            {row.original.collection}
          </span>
        ),
        enableColumnFilter: true,
        enableSorting: false,
        meta: {
          label: "Collection",
          placeholder: "Filter by collection...",
          variant: "text",
        },
      },
      {
        id: "uri",
        accessorKey: "uri",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} label="URI" />
        ),
        cell: ({ row }) => (
          <span
            className="font-mono text-xs block max-w-xs truncate"
            title={row.original.uri}
          >
            {row.original.uri}
          </span>
        ),
        enableSorting: false,
      },
      {
        id: "action",
        accessorKey: "action",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} label="Action" />
        ),
        cell: ({ row }) => actionBadge(row.original.action),
        enableSorting: false,
      },
      {
        id: "error",
        accessorKey: "error",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} label="Error" />
        ),
        cell: ({ row }) => (
          <span
            className="text-destructive text-xs block max-w-xs truncate"
            title={row.original.error}
          >
            {row.original.error.split("\n")[0]}
          </span>
        ),
        enableSorting: false,
      },
      {
        id: "attempts",
        accessorKey: "attempts",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} label="Attempts" />
        ),
        cell: ({ row }) => (
          <span className="text-sm tabular-nums">{row.original.attempts}</span>
        ),
        enableSorting: false,
      },
      {
        id: "created_at",
        accessorKey: "created_at",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} label="Created" />
        ),
        cell: ({ row }) => (
          <span
            className="text-muted-foreground whitespace-nowrap text-sm tabular-nums"
            title={new Date(row.original.created_at).toLocaleString()}
          >
            {timeAgo(row.original.created_at)}
          </span>
        ),
        enableSorting: false,
      },
      ...(resolvedFilter !== "false"
        ? [
            {
              id: "status",
              accessorKey: "resolved_at" as const,
              header: ({ column }) => (
                <DataTableColumnHeader column={column} label="Status" />
              ),
              cell: ({ row }) =>
                row.original.resolved_at ? (
                  <Badge variant="secondary">resolved</Badge>
                ) : (
                  <Badge variant="destructive">unresolved</Badge>
                ),
              enableSorting: false,
            } satisfies ColumnDef<DeadLetterSummary>,
          ]
        : []),
    ],
    [resolvedFilter],
  );

  const table = useReactTable({
    data: items,
    columns,
    state: { columnFilters, columnVisibility, rowSelection },
    defaultColumn: { enableColumnFilter: false },
    onColumnFiltersChange: setColumnFilters,
    onColumnVisibilityChange: setColumnVisibility,
    onRowSelectionChange: setRowSelection,
    getCoreRowModel: getCoreRowModel(),
    getRowId: (row) => row.id,
  });

  return (
    <>
      <SiteHeader title="Dead Letters" />
      <div className="flex flex-1 flex-col gap-4 p-4 md:p-6">
        <div className="flex items-center gap-2">
          <DataTableToolbar table={table} />
          <Select value={resolvedFilter} onValueChange={setResolvedFilter}>
            <SelectTrigger className="w-[160px] h-8">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="false">Unresolved</SelectItem>
              <SelectItem value="true">Resolved</SelectItem>
              <SelectItem value="all">All</SelectItem>
            </SelectContent>
          </Select>
        </div>

        {selectedIds.length > 0 && (
          <div className="flex items-center gap-2 rounded-md border p-2 text-sm">
            <span className="text-muted-foreground">
              {selectedIds.length} selected
            </span>
            <Button
              size="sm"
              variant="outline"
              disabled={loading}
              onClick={() => handleBulkAction("retry", "selected")}
            >
              <RotateCcw className="mr-1 size-3.5" />
              Retry
            </Button>
            <Button
              size="sm"
              variant="outline"
              disabled={loading}
              onClick={() => handleBulkAction("reindex", "selected")}
            >
              <RefreshCw className="mr-1 size-3.5" />
              Re-index
            </Button>
            <AlertDialog>
              <AlertDialogTrigger asChild>
                <Button size="sm" variant="ghost" disabled={loading}>
                  <XCircle className="mr-1 size-3.5" />
                  Dismiss
                </Button>
              </AlertDialogTrigger>
              <AlertDialogContent>
                <AlertDialogHeader>
                  <AlertDialogTitle>Dismiss {selectedIds.length} dead {selectedIds.length === 1 ? "letter" : "letters"}?</AlertDialogTitle>
                  <AlertDialogDescription>
                    Dismissed dead letters will be marked as resolved and will not be retried.
                  </AlertDialogDescription>
                </AlertDialogHeader>
                <AlertDialogFooter>
                  <AlertDialogCancel>Cancel</AlertDialogCancel>
                  <AlertDialogAction variant="destructive" onClick={() => handleBulkAction("dismiss", "selected")}>Dismiss</AlertDialogAction>
                </AlertDialogFooter>
              </AlertDialogContent>
            </AlertDialog>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button size="sm" variant="ghost" disabled={loading}>
                  <MoreHorizontal className="size-4" />
                  All matching
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent>
                <DropdownMenuItem
                  onClick={() => handleBulkAction("retry", "all")}
                >
                  Retry all matching
                </DropdownMenuItem>
                <DropdownMenuItem
                  onClick={() => handleBulkAction("reindex", "all")}
                >
                  Re-index all matching
                </DropdownMenuItem>
                <DropdownMenuItem
                  onClick={() => setDismissAllOpen(true)}
                >
                  Dismiss all matching
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        )}

        <DataTable
          table={table}
          showPagination={false}
          onRowClick={openDetail}
        />

        <div className="flex w-full items-center justify-between gap-4 overflow-auto p-1">
          <p className="text-muted-foreground flex-1 whitespace-nowrap text-sm">
            {items.length} item(s) on this page.
          </p>
          <div className="flex items-center space-x-2">
            <Button
              aria-label="Go to previous page"
              title="Previous page"
              variant="outline"
              size="icon"
              className="size-8"
              disabled={cursorStack.length === 0 || loading}
              onClick={handlePrevious}
            >
              <ChevronLeft />
            </Button>
            <Button
              aria-label="Go to next page"
              title="Next page"
              variant="outline"
              size="icon"
              className="size-8"
              disabled={!nextCursor || loading}
              onClick={handleNext}
            >
              <ChevronRight />
            </Button>
          </div>
        </div>

        <Sheet
          open={viewDetail != null}
          onOpenChange={(open) => {
            if (!open) setViewDetail(null);
          }}
        >
          <SheetContent className="overflow-hidden flex flex-col">
            {viewDetail && (
              <>
                <SheetHeader>
                  <SheetTitle className="flex items-center gap-2">
                    {actionBadge(viewDetail.action)}
                    <span className="font-mono text-sm">
                      {viewDetail.collection}
                    </span>
                  </SheetTitle>
                </SheetHeader>
                <div className="flex-1 min-h-0 flex flex-col px-4">
                  <DetailBody detail={viewDetail} />
                </div>
                {viewDetail.resolved_at == null && (
                  <SheetFooter className="border-t flex-row">
                    <div className="mr-auto">
                      <Button
                        size="sm"
                        variant="destructive"
                        disabled={actionLoading}
                        onClick={() => handleDetailAction("dismiss")}
                      >
                        Dismiss
                      </Button>
                    </div>
                    <Button
                      size="sm"
                      variant="outline"
                      disabled={actionLoading}
                      onClick={() => handleDetailAction("retry")}
                    >
                      <RotateCcw className="mr-1 size-3.5" />
                      Retry Hook
                    </Button>
                    <Button
                      size="sm"
                      variant="outline"
                      disabled={actionLoading}
                      onClick={() => handleDetailAction("reindex")}
                    >
                      <RefreshCw className="mr-1 size-3.5" />
                      Re-index
                    </Button>
                  </SheetFooter>
                )}
              </>
            )}
          </SheetContent>
        </Sheet>

        <AlertDialog open={dismissAllOpen} onOpenChange={setDismissAllOpen}>
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>Dismiss all matching dead letters?</AlertDialogTitle>
              <AlertDialogDescription>
                All dead letters matching the current filters will be marked as resolved. This action cannot be undone.
              </AlertDialogDescription>
            </AlertDialogHeader>
            <AlertDialogFooter>
              <AlertDialogCancel>Cancel</AlertDialogCancel>
              <AlertDialogAction variant="destructive" onClick={() => handleBulkAction("dismiss", "all")}>Dismiss All</AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>
      </div>
    </>
  );
}
