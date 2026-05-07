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

import { useCurrentUser } from "@/hooks/use-current-user";
import {
  deleteLexicon,
  deleteNetworkLexicon,
  getLexicons,
} from "@/lib/api";
import type { LexiconSummary } from "@/types/lexicons";
import { DataTable } from "@/components/data-table/data-table";
import { DataTableColumnHeader } from "@/components/data-table/data-table-column-header";
import { DataTableToolbar } from "@/components/data-table/data-table-toolbar";
import { SiteHeader } from "@/components/site-header";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Eye, Rows3, Trash2 } from "lucide-react";

export default function LexiconsPage() {
  const { hasPermission } = useCurrentUser();
  const router = useRouter();
  const [lexicons, setLexicons] = useState<LexiconSummary[]>([]);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    getLexicons()
      .then(setLexicons)
      .catch((e) => setError(e.message));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  async function handleDelete(lex: LexiconSummary) {
    try {
      if (lex.source === "network") {
        await deleteNetworkLexicon(lex.id);
      } else {
        await deleteLexicon(lex.id);
      }
      load();
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  const columns = useMemo<ColumnDef<LexiconSummary>[]>(
    () => [
      {
        id: "id",
        accessorKey: "id",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} label="ID" />
        ),
        cell: ({ row }) => (
          <span className="font-mono text-sm">{row.original.id}</span>
        ),
        filterFn: "includesString",
        enableColumnFilter: true,
        enableSorting: true,
        enableHiding: false,
        meta: {
          label: "ID",
          placeholder: "Filter by ID...",
          variant: "text",
        },
      },
      {
        id: "lexicon_type",
        accessorKey: "lexicon_type",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} label="Type" />
        ),
        cell: ({ row }) => (
          <Badge variant="outline">{row.original.lexicon_type}</Badge>
        ),
        filterFn: (row, columnId, filterValue) => {
          if (!Array.isArray(filterValue) || filterValue.length === 0)
            return true;
          return filterValue.includes(row.getValue(columnId));
        },
        enableColumnFilter: true,
        enableSorting: true,
        meta: {
          label: "Type",
          variant: "multiSelect",
          options: [
            { label: "Record", value: "record" },
            { label: "Query", value: "query" },
            { label: "Procedure", value: "procedure" },
          ],
        },
      },
      {
        id: "source",
        accessorKey: "source",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} label="Source" />
        ),
        cell: ({ row }) => (
          <Badge
            variant={
              row.original.source === "network" ? "secondary" : "outline"
            }
          >
            {row.original.source}
          </Badge>
        ),
        filterFn: (row, columnId, filterValue) => {
          if (!Array.isArray(filterValue) || filterValue.length === 0)
            return true;
          return filterValue.includes(row.getValue(columnId));
        },
        enableColumnFilter: true,
        enableSorting: true,
        meta: {
          label: "Source",
          variant: "select",
          options: [
            { label: "Manual", value: "manual" },
            { label: "Network", value: "network" },
          ],
        },
      },
      {
        id: "action",
        accessorKey: "action",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} label="Action" />
        ),
        cell: ({ row }) => row.original.action ?? "--",
        enableSorting: true,
      },
      {
        id: "backfill",
        accessorKey: "backfill",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} label="Backfill" />
        ),
        cell: ({ row }) => (row.original.backfill ? "Yes" : "No"),
        enableSorting: true,
      },
      {
        id: "revision",
        accessorKey: "revision",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} label="Revision" />
        ),
        cell: ({ row }) => (
          <span className="tabular-nums">{row.original.revision}</span>
        ),
        enableSorting: true,
      },
      {
        id: "actions",
        header: "",
        cell: ({ row }) => (
          <div className="flex justify-end gap-1">
            {row.original.lexicon_type === "record" && (
              <Button
                variant="outline"
                size="icon"
                className="size-8 text-muted-foreground"
                title="View records"
                aria-label="View records"
                asChild
              >
                <Link
                  href={`/dashboard/records?collection=${encodeURIComponent(row.original.id)}`}
                  onClick={(e) => e.stopPropagation()}
                >
                  <Rows3 className="size-4" />
                </Link>
              </Button>
            )}

            <Button
              variant="outline"
              size="icon"
              className="size-8 text-muted-foreground"
              title="View lexicon"
              aria-label="View lexicon"
              asChild
            >
              <Link
                href={`/dashboard/lexicons/${encodeURIComponent(row.original.id)}`}
                onClick={(e) => e.stopPropagation()}
              >
                <Eye className="size-4" />
              </Link>
            </Button>

            {hasPermission("lexicons:delete") && (
              <Button
                variant="destructive"
                size="icon"
                className="size-8 text-muted-foreground hover:text-destructive"
                title="Delete lexicon"
                aria-label="Delete lexicon"
                onClick={(e) => {
                  e.stopPropagation();
                  handleDelete(row.original);
                }}
              >
                <Trash2 className="size-4" />
              </Button>
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
    { id: "id", desc: false },
  ]);
  const [columnFilters, setColumnFilters] = useState<ColumnFiltersState>([]);
  const [columnVisibility, setColumnVisibility] = useState<VisibilityState>({});
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
      <SiteHeader title="Lexicons" />
      <div className="flex flex-1 flex-col gap-4 p-4 md:p-6">
        {error && <p className="text-destructive text-sm">{error}</p>}

        <DataTable
          table={table}
          onRowClick={(lex) =>
            router.push(`/dashboard/lexicons/${encodeURIComponent(lex.id)}`)
          }
        >
          <DataTableToolbar table={table}>
            {hasPermission("lexicons:create") && (
              <Button asChild>
                <Link href="/dashboard/lexicons/new">Add Lexicon</Link>
              </Button>
            )}
          </DataTableToolbar>
        </DataTable>
      </div>
    </>
  );
}
