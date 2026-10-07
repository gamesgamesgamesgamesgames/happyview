"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import {
  type ColumnDef,
  type VisibilityState,
  getCoreRowModel,
  useReactTable,
} from "@tanstack/react-table";
import { useRouter } from "next/navigation";
import { ChevronLeft, ChevronRight } from "lucide-react";

import { toastError } from "@/lib/format";
import { getAdminSpaces } from "@/lib/api";
import type { AdminSpace } from "@/types/spaces";
import { DataTable } from "@/components/data-table/data-table";
import { DataTableViewOptions } from "@/components/data-table/data-table-view-options";
import { SiteHeader } from "@/components/site-header";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { InspectorDisabled } from "@/components/spaces/inspector-disabled";
import { useConfig } from "@/lib/config-context";

const PAGE_SIZE = 50;

function SpacesList() {
  const router = useRouter();
  const [spaces, setSpaces] = useState<AdminSpace[]>([]);
  const [cursorStack, setCursorStack] = useState<string[]>([]);
  const [nextCursor, setNextCursor] = useState<string | undefined>();
  const [loading, setLoading] = useState(false);
  const [columnVisibility, setColumnVisibility] = useState<VisibilityState>({});
  const [lookupDid, setLookupDid] = useState("");

  const fetchSpaces = useCallback(async (cursor?: string) => {
    setLoading(true);
    try {
      const data = await getAdminSpaces(PAGE_SIZE, cursor);
      setSpaces(data.spaces);
      setNextCursor(data.cursor);
    } catch (e: unknown) {
      toastError("Failed to load spaces", e);
      setSpaces([]);
      setNextCursor(undefined);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchSpaces();
  }, [fetchSpaces]);

  const columns = useMemo<ColumnDef<AdminSpace>[]>(
    () => [
      {
        id: "name",
        accessorFn: (row) => row.display_name ?? row.skey,
        header: "Name",
        cell: ({ row }) => (
          <div className="flex flex-col">
            <span className="text-sm">
              {row.original.display_name ?? row.original.skey}
            </span>
            {row.original.description && (
              <span className="text-muted-foreground max-w-sm truncate text-xs">
                {row.original.description}
              </span>
            )}
          </div>
        ),
        enableHiding: false,
        meta: { label: "Name" },
      },
      {
        id: "type",
        accessorKey: "type",
        header: "Type",
        cell: ({ getValue }) => (
          <span className="font-mono text-xs">{getValue<string>()}</span>
        ),
        meta: { label: "Type" },
      },
      {
        id: "creator_did",
        accessorKey: "creator_did",
        header: "Creator",
        cell: ({ getValue }) => (
          <span className="font-mono text-xs whitespace-nowrap">
            {getValue<string>()}
          </span>
        ),
        meta: { label: "Creator" },
      },
      {
        id: "uri",
        accessorKey: "uri",
        header: "URI",
        cell: ({ getValue }) => (
          <span className="font-mono text-xs break-all">
            {getValue<string>()}
          </span>
        ),
        meta: { label: "URI" },
      },
      {
        id: "created_at",
        accessorKey: "created_at",
        header: "Created",
        cell: ({ getValue }) => (
          <span className="text-xs whitespace-nowrap">
            {new Date(getValue<string>()).toLocaleString()}
          </span>
        ),
        meta: { label: "Created" },
      },
    ],
    [],
  );

  const table = useReactTable({
    data: spaces,
    columns,
    state: { columnVisibility },
    onColumnVisibilityChange: setColumnVisibility,
    getCoreRowModel: getCoreRowModel(),
    getRowId: (row) => row.id,
  });

  function handleNext() {
    if (!nextCursor) return;
    setCursorStack((prev) => [...prev, nextCursor]);
    fetchSpaces(nextCursor);
  }

  function handlePrevious() {
    if (cursorStack.length === 0) return;
    const stack = cursorStack.slice(0, -1);
    setCursorStack(stack);
    fetchSpaces(stack[stack.length - 1]);
  }

  return (
    <>
      <SiteHeader title="Spaces" />
      <div className="flex flex-1 flex-col gap-4 p-4 md:p-6">
        <DataTable
          table={table}
          showPagination={false}
          onRowClick={(space) =>
            router.push(`/dashboard/spaces/${encodeURIComponent(space.id)}`)
          }
        >
          <div className="flex w-full items-center justify-between gap-2 p-1">
            <form
              className="mr-auto flex items-center gap-2"
              onSubmit={(e) => {
                e.preventDefault();
                const did = lookupDid.trim();
                if (did) router.push(`/dashboard/spaces/account/?did=${encodeURIComponent(did)}`);
              }}
            >
              <Label htmlFor="account-did" className="sr-only">
                Account DID
              </Label>
              <Input
                id="account-did"
                className="h-8 w-72 font-mono text-xs"
                placeholder="did:plc:…"
                value={lookupDid}
                onChange={(e) => setLookupDid(e.target.value)}
              />
              <Button type="submit" variant="outline" size="sm">
                Open account
              </Button>
            </form>
            <DataTableViewOptions table={table} />
          </div>
        </DataTable>

        <div className="flex w-full items-center justify-between gap-4 overflow-auto p-1">
          <p className="text-muted-foreground flex-1 whitespace-nowrap text-sm">
            {spaces.length} space(s) on this page.
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
      </div>
    </>
  );
}

export default function SpacesPage() {
  const { features } = useConfig();
  if (!features.space_inspector) return <InspectorDisabled title="Spaces" />;
  return <SpacesList />;
}
