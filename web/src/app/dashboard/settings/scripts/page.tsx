"use client";

import {
  type ColumnDef,
  type ColumnFiltersState,
  type PaginationState,
  type SortingState,
  type VisibilityState,
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
import { ExternalLink, Eye, Trash2 } from "lucide-react";
import { toast } from "sonner";

import { useCurrentUser } from "@/hooks/use-current-user";
import { toastError } from "@/lib/format";
import { deleteScript, getScripts } from "@/lib/api";
import type { Script } from "@/types/scripts";
import {
  TRIGGER_FAMILY_LABELS,
  TRIGGER_KIND_LABELS,
  type TriggerFamily,
  type TriggerKind,
  familyOf,
  parseTriggerId,
} from "@/types/scripts";
import { DataTable } from "@/components/data-table/data-table";
import { DataTableColumnHeader } from "@/components/data-table/data-table-column-header";
import { DataTableToolbar } from "@/components/data-table/data-table-toolbar";
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

interface ScriptRow extends Script {
  suffix: string;
  kind: TriggerKind | null;
  family: TriggerFamily | null;
}

export default function ScriptsPage() {
  const { hasPermission } = useCurrentUser();
  const router = useRouter();
  const [scripts, setScripts] = useState<Script[]>([]);

  const load = useCallback(() => {
    getScripts()
      .then(setScripts)
      .catch((e) => toastError("Failed to load scripts", e));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const rows = useMemo<ScriptRow[]>(
    () =>
      scripts.map((s) => {
        const parsed = parseTriggerId(s.id);
        return {
          ...s,
          suffix: parsed?.suffix ?? s.id,
          kind: parsed?.kind ?? null,
          family: parsed ? familyOf(parsed.kind) : null,
        };
      }),
    [scripts],
  );

  async function handleDelete(id: string) {
    try {
      await deleteScript(id);
      toast.success("Script deleted");
      load();
    } catch (e: unknown) {
      toastError("Failed to delete script", e);
    }
  }

  const columns = useMemo<ColumnDef<ScriptRow>[]>(
    () => [
      {
        id: "suffix",
        accessorKey: "suffix",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} label="Lexicon" />
        ),
        cell: ({ row }) => (
          <span className="font-mono text-xs">{row.original.suffix}</span>
        ),
        filterFn: "includesString",
        enableColumnFilter: true,
        enableSorting: true,
        enableHiding: false,
        meta: {
          label: "Lexicon",
          placeholder: "Filter by lexicon...",
          variant: "text",
        },
      },
      {
        id: "kind",
        accessorKey: "kind",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} label="Kind" />
        ),
        cell: ({ row }) =>
          row.original.kind ? (
            <Badge variant="outline">
              {TRIGGER_KIND_LABELS[row.original.kind]}
            </Badge>
          ) : (
            <Badge variant="destructive">malformed</Badge>
          ),
        filterFn: (row, columnId, filterValue) => {
          if (!Array.isArray(filterValue) || filterValue.length === 0)
            return true;
          return filterValue.includes(row.getValue(columnId));
        },
        enableColumnFilter: true,
        enableSorting: true,
        meta: {
          label: "Kind",
          variant: "multiSelect",
          options: (
            Object.entries(TRIGGER_KIND_LABELS) as [TriggerKind, string][]
          ).map(([value, label]) => ({ value, label })),
        },
      },
      {
        id: "family",
        accessorKey: "family",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} label="Family" />
        ),
        cell: ({ row }) =>
          row.original.family ? (
            <Badge variant="secondary">
              {TRIGGER_FAMILY_LABELS[row.original.family]}
            </Badge>
          ) : null,
        filterFn: (row, columnId, filterValue) => {
          if (!Array.isArray(filterValue) || filterValue.length === 0)
            return true;
          return filterValue.includes(row.getValue(columnId));
        },
        enableColumnFilter: true,
        enableSorting: true,
        meta: {
          label: "Family",
          variant: "multiSelect",
          options: (
            Object.entries(TRIGGER_FAMILY_LABELS) as [TriggerFamily, string][]
          ).map(([value, label]) => ({ value, label })),
        },
      },
      {
        id: "description",
        accessorKey: "description",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} label="Description" />
        ),
        cell: ({ row }) => (
          <span className="text-muted-foreground text-sm max-w-64 truncate block">
            {row.original.description ?? ""}
          </span>
        ),
        enableSorting: true,
      },
      {
        id: "updated_at",
        accessorKey: "updated_at",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} label="Updated" />
        ),
        cell: ({ row }) => (
          <span className="text-muted-foreground text-sm">
            {new Date(row.original.updated_at).toLocaleString()}
          </span>
        ),
        enableSorting: true,
      },
      {
        id: "actions",
        header: "",
        cell: ({ row }) => (
          <div className="flex justify-end gap-1">
            {row.original.kind && (
              <Button
                variant="outline"
                size="icon"
                className="size-8 text-muted-foreground"
                title="View lexicon"
                aria-label="View lexicon"
                asChild
              >
                <Link
                  href={`/dashboard/lexicons/${encodeURIComponent(row.original.suffix)}`}
                  onClick={(e) => e.stopPropagation()}
                >
                  <ExternalLink className="size-4" />
                </Link>
              </Button>
            )}
            <Button
              variant="outline"
              size="icon"
              className="size-8 text-muted-foreground"
              title="View script"
              aria-label="View script"
              asChild
            >
              <Link
                href={`/dashboard/settings/scripts/${encodeURIComponent(row.original.id)}`}
                onClick={(e) => e.stopPropagation()}
              >
                <Eye className="size-4" />
              </Link>
            </Button>
            {hasPermission("scripts:manage") && (
              <AlertDialog>
                <AlertDialogTrigger asChild>
                  <Button
                    variant="destructive"
                    size="icon"
                    className="size-8 text-muted-foreground hover:text-destructive"
                    title="Delete script"
                    aria-label="Delete script"
                    onClick={(e) => e.stopPropagation()}
                  >
                    <Trash2 className="size-4" />
                  </Button>
                </AlertDialogTrigger>
                <AlertDialogContent onClick={(e) => e.stopPropagation()}>
                  <AlertDialogHeader>
                    <AlertDialogTitle>Delete script?</AlertDialogTitle>
                    <AlertDialogDescription>
                      This will permanently remove the script. This action cannot be undone.
                    </AlertDialogDescription>
                  </AlertDialogHeader>
                  <AlertDialogFooter>
                    <AlertDialogCancel>Cancel</AlertDialogCancel>
                    <AlertDialogAction variant="destructive" onClick={() => handleDelete(row.original.id)}>Delete</AlertDialogAction>
                  </AlertDialogFooter>
                </AlertDialogContent>
              </AlertDialog>
            )}
          </div>
        ),
        enableSorting: false,
        enableHiding: false,
      },
    ],
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [hasPermission],
  );

  const [sorting, setSorting] = useState<SortingState>([
    { id: "suffix", desc: false },
  ]);
  const [columnFilters, setColumnFilters] = useState<ColumnFiltersState>([]);
  const [columnVisibility, setColumnVisibility] = useState<VisibilityState>({});
  const [pagination, setPagination] = useState<PaginationState>({
    pageIndex: 0,
    pageSize: 20,
  });

  const table = useReactTable({
    data: rows,
    columns,
    state: {
      sorting,
      columnFilters,
      columnVisibility,
      pagination,
      columnPinning: { right: ["actions"] },
    },
    defaultColumn: {
      enableColumnFilter: false,
    },
    onSortingChange: setSorting,
    onColumnFiltersChange: setColumnFilters,
    onColumnVisibilityChange: setColumnVisibility,
    onPaginationChange: setPagination,
    getCoreRowModel: getCoreRowModel(),
    getFilteredRowModel: getFilteredRowModel(),
    getSortedRowModel: getSortedRowModel(),
    getPaginationRowModel: getPaginationRowModel(),
    getFacetedRowModel: getFacetedRowModel(),
    getFacetedUniqueValues: getFacetedUniqueValues(),
    getRowId: (row) => row.id,
  });

  return (
    <>
      <SiteHeader title="Scripts" />
      <div className="flex flex-1 flex-col gap-4 p-4 md:p-6">
        <DataTable
          table={table}
          onRowClick={(row) =>
            router.push(
              `/dashboard/settings/scripts/${encodeURIComponent(row.id)}`,
            )
          }
        >
          <DataTableToolbar table={table}>
            {hasPermission("scripts:manage") && (
              <Button asChild>
                <Link href="/dashboard/settings/scripts/new">New script</Link>
              </Button>
            )}
          </DataTableToolbar>
        </DataTable>
      </div>
    </>
  );
}
