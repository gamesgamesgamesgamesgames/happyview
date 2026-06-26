"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  type ColumnDef,
  type RowSelectionState,
  type VisibilityState,
  getCoreRowModel,
  useReactTable,
} from "@tanstack/react-table";
import { toast } from "sonner";

import { useSearchParams } from "next/navigation";
import { useCurrentUser } from "@/hooks/use-current-user";
import { toastError } from "@/lib/format";
import {
  getCollections,
  getAdminRecords,
  deleteRecord,
  deleteCollectionRecords,
} from "@/lib/api";
import type { AdminRecord } from "@/types/records";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import {
  ResponsiveDialog,
  ResponsiveDialogClose,
  ResponsiveDialogContent,
  ResponsiveDialogDescription,
  ResponsiveDialogFooter,
  ResponsiveDialogHeader,
  ResponsiveDialogTitle,
} from "@/components/ui/responsive-dialog";
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { Checkbox } from "@/components/ui/checkbox";
import { CodeBlock } from "@/components/code-block";
import { DataTable } from "@/components/data-table/data-table";
import { DataTableViewOptions } from "@/components/data-table/data-table-view-options";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { LabelBadges } from "@/components/label-badges";
import { SiteHeader } from "@/components/site-header";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Input } from "@/components/ui/input";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { ChevronLeft, ChevronRight, Trash2 } from "lucide-react";
import {
  Field,
  FieldContent,
  FieldDescription,
  FieldLabel,
  FieldTitle,
} from "@/components/ui/field";

function parseAtUri(uri: string): { did: string; rkey: string } {
  const parts = uri.replace("at://", "").split("/");
  return { did: parts[0] ?? "", rkey: parts[2] ?? "" };
}

function formatCellValue(value: unknown): string {
  if (value === null || value === undefined) return "";
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean")
    return String(value);
  return JSON.stringify(value);
}

export default function RecordsPage() {
  const { hasPermission } = useCurrentUser();
  const searchParams = useSearchParams();
  const initialCollection = searchParams.get("collection") ?? "";
  const appliedInitial = useRef(false);
  const [collections, setCollections] = useState<string[]>([]);
  const [selectedCollection, setSelectedCollection] = useState<string>("");
  const [records, setRecords] = useState<AdminRecord[]>([]);
  const [cursorStack, setCursorStack] = useState<string[]>([]);
  const [nextCursor, setNextCursor] = useState<string | undefined>();
  const [loading, setLoading] = useState(false);
  const [viewRecord, setViewRecord] = useState<AdminRecord | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [deleteUri, setDeleteUri] = useState<string | null>(null);
  const [bulkDeleteOpen, setBulkDeleteOpen] = useState(false);
  const [bulkDeleteMode, setBulkDeleteMode] = useState<"selected" | "all">(
    "selected",
  );
  const [bulkDeleteConfirm, setBulkDeleteConfirm] = useState("");
  const [deletingAll, setDeletingAll] = useState(false);

  const [columnVisibility, setColumnVisibility] = useState<VisibilityState>({});
  const [rowSelection, setRowSelection] = useState<RowSelectionState>({});

  useEffect(() => {
    getCollections()
      .then((data) => setCollections(data.collections))
      .catch((e) => toastError("Failed to load collections", e));
  }, []);

  useEffect(() => {
    if (Object.keys(rowSelection).length === 0) return;
    const validUris = new Set(records.map((r) => r.uri));
    const pruned: RowSelectionState = {};
    let changed = false;
    for (const [uri, selected] of Object.entries(rowSelection)) {
      if (validUris.has(uri)) {
        pruned[uri] = selected;
      } else {
        changed = true;
      }
    }
    if (changed) setRowSelection(pruned);
  }, [records]);

  // Auto-select collection from URL search param on initial load.
  useEffect(() => {
    if (appliedInitial.current || !initialCollection || collections.length === 0)
      return;
    if (collections.includes(initialCollection)) {
      appliedInitial.current = true;
      handleSelectCollection(initialCollection);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [collections, initialCollection]);

  const fetchRecords = useCallback(
    async (collection: string, cursor?: string) => {
      setLoading(true);
      try {
        const data = await getAdminRecords(collection, 20, cursor);
        setRecords(data.records);
        setNextCursor(data.cursor);
      } catch (e: unknown) {
        toastError("Failed to load records", e);
        setRecords([]);
        setNextCursor(undefined);
      } finally {
        setLoading(false);
      }
    },
    [],
  );

  const handleDeleteRecord = useCallback(
    async (uri: string) => {
      setDeleting(true);
      try {
        await deleteRecord(uri);
        setDeleteUri(null);
        setViewRecord(null);
        toast.success("Record deleted");
        if (selectedCollection) {
          const currentCursor =
            cursorStack.length > 0
              ? cursorStack[cursorStack.length - 1]
              : undefined;
          fetchRecords(selectedCollection, currentCursor);
        }
      } catch (e: unknown) {
        toastError("Failed to delete record", e);
      } finally {
        setDeleting(false);
      }
    },
    [selectedCollection, cursorStack, fetchRecords],
  );

  const handleDeleteAll = useCallback(async () => {
    if (!selectedCollection) return;
    setDeletingAll(true);
    try {
      await deleteCollectionRecords(selectedCollection);
      setBulkDeleteOpen(false);
      setBulkDeleteMode("selected");
      setBulkDeleteConfirm("");
      setRowSelection({});
      toast.success("All records deleted");
      const data = await getCollections();
      setCollections(data.collections);
      setCursorStack([]);
      setNextCursor(undefined);
      setRecords([]);
      setSelectedCollection("");
    } catch (e: unknown) {
      toastError("Failed to delete collection", e);
    } finally {
      setDeletingAll(false);
    }
  }, [selectedCollection]);

  const handleBulkDelete = useCallback(async () => {
    setDeleting(true);
    const selectedUris = Object.keys(rowSelection);
    const results = await Promise.allSettled(selectedUris.map((uri) => deleteRecord(uri)));
    const succeeded = selectedUris.filter((_, i) => results[i].status === "fulfilled");
    const failed = selectedUris.length - succeeded.length;

    if (failed === 0) {
      toast.success(`Deleted ${succeeded.length} ${succeeded.length === 1 ? "record" : "records"}`);
    } else if (succeeded.length === 0) {
      toast.error("Failed to delete records");
    } else {
      toast.warning(`Deleted ${succeeded.length} of ${selectedUris.length} records`, {
        description: `${failed} ${failed === 1 ? "record" : "records"} failed to delete.`,
      });
    }

    setRowSelection({});
    setBulkDeleteOpen(false);
    if (selectedCollection) {
      const currentCursor =
        cursorStack.length > 0
          ? cursorStack[cursorStack.length - 1]
          : undefined;
      fetchRecords(selectedCollection, currentCursor);
    }
    setDeleting(false);
  }, [rowSelection, selectedCollection, cursorStack, fetchRecords]);

  // Build columns dynamically from the union of all record keys
  const columns = useMemo<ColumnDef<AdminRecord>[]>(() => {
    const keySet = new Set<string>();
    for (const r of records) {
      for (const key of Object.keys(r.record)) {
        keySet.add(key);
      }
    }

    const cols: ColumnDef<AdminRecord>[] = [
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
          <Checkbox
            checked={row.getIsSelected()}
            onCheckedChange={(value) => row.toggleSelected(!!value)}
            onClick={(e) => e.stopPropagation()}
            aria-label="Select row"
          />
        ),
        enableSorting: false,
        enableHiding: false,
      },
      {
        id: "did",
        accessorFn: (row) => parseAtUri(row.uri).did,
        header: "DID",
        cell: ({ getValue }) => (
          <span className="font-mono text-xs whitespace-nowrap">
            {getValue<string>()}
          </span>
        ),
        enableSorting: false,
        enableHiding: false,
        meta: { label: "DID" },
      },
      {
        id: "rkey",
        accessorFn: (row) => parseAtUri(row.uri).rkey,
        header: "Record Key",
        cell: ({ getValue }) => (
          <span className="font-mono text-xs whitespace-nowrap">
            {getValue<string>()}
          </span>
        ),
        enableSorting: false,
        enableHiding: false,
        meta: { label: "Record Key" },
      },
      {
        id: "labels",
        accessorFn: (row) => row.labels,
        header: "Labels",
        enableSorting: false,
        cell: ({ row }) => (
          <LabelBadges
            labels={row.original.labels}
            recordDid={row.original.did}
          />
        ),
        meta: { label: "Labels" },
      },
    ];

    for (const key of keySet) {
      cols.push({
        id: key,
        accessorFn: (row) => row.record[key],
        header: key,
        enableSorting: false,
        cell: ({ getValue }) => {
          const val = getValue<unknown>();
          const str = formatCellValue(val);
          return (
            <span
              className="font-mono text-xs block max-w-xs truncate"
              title={str}
            >
              {str}
            </span>
          );
        },
        meta: { label: key },
      });
    }

    return cols;
  }, [records]);

  const table = useReactTable({
    data: records,
    columns,
    state: {
      columnVisibility,
      columnPinning: { left: ["select"] },
      rowSelection,
    },
    onColumnVisibilityChange: setColumnVisibility,
    onRowSelectionChange: setRowSelection,
    enableRowSelection: true,
    getCoreRowModel: getCoreRowModel(),
    getRowId: (row) => row.uri,
  });

  function handleSelectCollection(collection: string) {
    setSelectedCollection(collection);
    setCursorStack([]);
    setNextCursor(undefined);
    setColumnVisibility({});
    setRowSelection({});
    fetchRecords(collection);
  }

  function handleNext() {
    if (!nextCursor || !selectedCollection) return;
    setCursorStack((prev) => [...prev, nextCursor]);
    fetchRecords(selectedCollection, nextCursor);
  }

  function handlePrevious() {
    if (cursorStack.length === 0 || !selectedCollection) return;
    const stack = [...cursorStack];
    stack.pop();
    const prevCursor = stack.length > 0 ? stack[stack.length - 1] : undefined;
    setCursorStack(stack);
    fetchRecords(selectedCollection, prevCursor);
  }

  return (
    <>
      <SiteHeader title="Records" />
      <div className="flex flex-1 flex-col gap-4 p-4 md:p-6">
        <DataTable
          table={table}
          showPagination={false}
          onRowClick={setViewRecord}
        >
          <div className="flex w-full items-center justify-between gap-2 p-1">
            <Select
              value={selectedCollection}
              onValueChange={handleSelectCollection}
            >
              <SelectTrigger className="h-8 w-80 text-sm">
                <SelectValue placeholder="Select a collection" />
              </SelectTrigger>
              <SelectContent>
                {collections.map((col) => (
                  <SelectItem key={col} value={col}>
                    {col}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <div className="flex items-center gap-2">
              {hasPermission("records:delete") && (
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button
                      variant="outline"
                      size="sm"
                      className="h-8"
                      disabled={Object.keys(rowSelection).length === 0}
                    >
                      Actions
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    <DropdownMenuItem
                      variant="destructive"
                      onClick={() => setBulkDeleteOpen(true)}
                    >
                      <Trash2 className="size-4" />
                      Delete
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
              )}
              <DataTableViewOptions table={table} />
            </div>
          </div>
        </DataTable>

        {selectedCollection && (
          <div className="flex w-full items-center justify-between gap-4 overflow-auto p-1">
            <p className="text-muted-foreground flex-1 whitespace-nowrap text-sm">
              {records.length} record(s) on this page.
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
        )}

        <Sheet
          open={viewRecord != null}
          onOpenChange={(open) => {
            if (!open) setViewRecord(null);
          }}
        >
          <SheetContent className="overflow-hidden flex flex-col">
            {viewRecord && (
              <>
                <SheetHeader>
                  <SheetTitle className="sr-only">Record Detail</SheetTitle>
                </SheetHeader>
                <div className="flex-1 min-h-0 overflow-y-auto px-4 flex flex-col gap-4">
                  <div className="grid grid-cols-2 gap-4 text-sm">
                    <div className="col-span-2">
                      <span className="text-muted-foreground">URI</span>
                      <p className="font-mono text-xs break-all">{viewRecord.uri}</p>
                    </div>
                    <div>
                      <span className="text-muted-foreground">DID</span>
                      <p className="font-mono text-xs break-all">{viewRecord.did}</p>
                    </div>
                    <div>
                      <span className="text-muted-foreground">Collection</span>
                      <p className="font-mono text-xs">{viewRecord.collection}</p>
                    </div>
                    <div>
                      <span className="text-muted-foreground">Record Key</span>
                      <p className="font-mono text-xs">{viewRecord.rkey}</p>
                    </div>
                    <div>
                      <span className="text-muted-foreground">CID</span>
                      <p className="font-mono text-xs break-all">{viewRecord.cid}</p>
                    </div>
                    {viewRecord.indexed_at && (
                      <div>
                        <span className="text-muted-foreground">Indexed</span>
                        <p className="text-xs">
                          {new Date(viewRecord.indexed_at).toLocaleString()}
                        </p>
                      </div>
                    )}
                    {viewRecord.labels.length > 0 && (
                      <div className="col-span-2">
                        <span className="text-muted-foreground">Labels</span>
                        <div className="flex flex-wrap gap-1 mt-1">
                          {viewRecord.labels.map((l, i) => (
                            <span
                              key={i}
                              className="bg-muted rounded px-1.5 py-0.5 font-mono text-xs"
                            >
                              {l.val}
                            </span>
                          ))}
                        </div>
                      </div>
                    )}
                  </div>

                  <div className="flex flex-col flex-1 min-h-0">
                    <span className="text-muted-foreground text-sm">Record</span>
                    <div className="mt-1">
                      <CodeBlock code={JSON.stringify(viewRecord.record, null, 2)} />
                    </div>
                  </div>
                </div>
                {hasPermission("records:delete") && (
                  <div className="flex justify-end border-t p-4">
                    <Button
                      variant="destructive"
                      onClick={() => setDeleteUri(viewRecord.uri)}
                      disabled={deleting}
                    >
                      {deleting ? "Deleting..." : "Delete Record"}
                    </Button>
                  </div>
                )}
              </>
            )}
          </SheetContent>
        </Sheet>

        <AlertDialog open={!!deleteUri} onOpenChange={(open) => { if (!open) setDeleteUri(null); }}>
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>Delete record?</AlertDialogTitle>
              <AlertDialogDescription>
                This will permanently delete the record. This action cannot be
                undone.
              </AlertDialogDescription>
            </AlertDialogHeader>
            {deleteUri && (
              <code className="text-muted-foreground block truncate text-xs">
                {deleteUri}
              </code>
            )}
            <AlertDialogFooter>
              <AlertDialogCancel disabled={deleting}>Cancel</AlertDialogCancel>
              <AlertDialogAction
                variant="destructive"
                disabled={deleting}
                onClick={() => {
                  if (deleteUri) handleDeleteRecord(deleteUri);
                }}
              >
                {deleting ? "Deleting..." : "Delete"}
              </AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>

        <ResponsiveDialog
          open={bulkDeleteOpen}
          onOpenChange={(open) => {
            if (!open) {
              setBulkDeleteOpen(false);
              setBulkDeleteMode("selected");
              setBulkDeleteConfirm("");
            }
          }}
        >
          <ResponsiveDialogContent>
            {(() => {
              const selectedCount = Object.keys(rowSelection).length;

              if (!table.getIsAllPageRowsSelected()) {
                return (
                  <>
                    <ResponsiveDialogHeader>
                      <ResponsiveDialogTitle>
                        Delete {selectedCount} record(s)?
                      </ResponsiveDialogTitle>
                      <ResponsiveDialogDescription>
                        This action cannot be undone.
                      </ResponsiveDialogDescription>
                    </ResponsiveDialogHeader>
                    <ResponsiveDialogFooter>
                      <ResponsiveDialogClose asChild>
                        <Button variant="outline" disabled={deleting}>
                          Cancel
                        </Button>
                      </ResponsiveDialogClose>
                      <Button
                        variant="destructive"
                        disabled={deleting}
                        onClick={handleBulkDelete}
                      >
                        {deleting ? "Deleting..." : "Delete"}
                      </Button>
                    </ResponsiveDialogFooter>
                  </>
                );
              }

              return (
                <>
                  <ResponsiveDialogHeader>
                    <ResponsiveDialogTitle>Delete records?</ResponsiveDialogTitle>
                    <ResponsiveDialogDescription>
                      This action cannot be undone.
                    </ResponsiveDialogDescription>
                  </ResponsiveDialogHeader>
                  <RadioGroup
                    value={bulkDeleteMode}
                    onValueChange={(v) => {
                      setBulkDeleteMode(v as "selected" | "all");
                      setBulkDeleteConfirm("");
                    }}
                  >
                    <FieldLabel htmlFor="bulk-delete-selected">
                      <Field orientation="horizontal">
                        <RadioGroupItem
                          value="selected"
                          id="bulk-delete-selected"
                        />
                        <FieldContent>
                          <FieldTitle>Delete selected only</FieldTitle>
                          <FieldDescription>
                            {`${selectedCount} items selected`}
                          </FieldDescription>
                        </FieldContent>
                      </Field>
                    </FieldLabel>

                    <FieldLabel htmlFor="bulk-delete-all">
                      <Field orientation="horizontal">
                        <RadioGroupItem value="all" id="bulk-delete-all" />
                        <FieldContent>
                          <FieldTitle>Delete all records</FieldTitle>
                          <FieldDescription>
                            <code className="font-semibold">
                              {selectedCollection}
                            </code>
                          </FieldDescription>
                        </FieldContent>
                      </Field>
                    </FieldLabel>
                  </RadioGroup>
                  {bulkDeleteMode === "all" && (
                    <div className="flex flex-col gap-2">
                      <label
                        className="text-sm"
                        htmlFor="bulk-delete-confirm"
                      >
                        Type{" "}
                        <code className="font-semibold">
                          {selectedCollection}
                        </code>{" "}
                        to confirm:
                      </label>
                      <Input
                        id="bulk-delete-confirm"
                        value={bulkDeleteConfirm}
                        onChange={(e) => setBulkDeleteConfirm(e.target.value)}
                        placeholder={selectedCollection}
                      />
                    </div>
                  )}
                  <ResponsiveDialogFooter>
                    <ResponsiveDialogClose asChild>
                      <Button variant="outline" disabled={deleting || deletingAll}>
                        Cancel
                      </Button>
                    </ResponsiveDialogClose>
                    <Button
                      variant="destructive"
                      disabled={
                        deleting ||
                        deletingAll ||
                        (bulkDeleteMode === "all" &&
                          bulkDeleteConfirm !== selectedCollection)
                      }
                      onClick={
                        bulkDeleteMode === "all"
                          ? handleDeleteAll
                          : handleBulkDelete
                      }
                    >
                      {deleting || deletingAll ? "Deleting..." : "Delete"}
                    </Button>
                  </ResponsiveDialogFooter>
                </>
              );
            })()}
          </ResponsiveDialogContent>
        </ResponsiveDialog>
      </div>
    </>
  );
}
