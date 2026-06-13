"use client";

import { useCallback, useEffect, useState } from "react";
import { Trash2 } from "lucide-react";

import {
  getServiceEntryXrpcs,
  updateServiceEntry,
  removeServiceEntryXrpcs,
  addServiceEntryXrpcs,
  deleteServiceEntry,
  type ServiceEntry,
} from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

interface ServiceEntrySheetProps {
  entry: ServiceEntry;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSaved: () => void;
}

export function ServiceEntrySheet({
  entry,
  open,
  onOpenChange,
  onSaved,
}: ServiceEntrySheetProps) {
  const [accessMode, setAccessMode] = useState<string>(entry.access_mode);
  const [xrpcs, setXrpcs] = useState<string[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [newXrpc, setNewXrpc] = useState("");
  const [adding, setAdding] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [deleting, setDeleting] = useState(false);

  const loadXrpcs = useCallback(async () => {
    if (accessMode !== "specific") return;
    try {
      const list = await getServiceEntryXrpcs(entry.id);
      setXrpcs(list);
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [entry.id, accessMode]);

  useEffect(() => {
    if (open) {
      setAccessMode(entry.access_mode);
      setSelected(new Set());
      setError(null);
    }
  }, [open, entry]);

  useEffect(() => {
    if (open) {
      loadXrpcs();
    }
  }, [open, loadXrpcs]);

  function toggleSelectAll() {
    if (selected.size === xrpcs.length) {
      setSelected(new Set());
    } else {
      setSelected(new Set(xrpcs));
    }
  }

  function toggleSelect(xrpc: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(xrpc)) {
        next.delete(xrpc);
      } else {
        next.add(xrpc);
      }
      return next;
    });
  }

  async function handleRemoveSelected() {
    if (selected.size === 0) return;
    setError(null);
    try {
      await removeServiceEntryXrpcs(entry.id, Array.from(selected));
      setSelected(new Set());
      await loadXrpcs();
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  async function handleAddXrpc() {
    const value = newXrpc.trim();
    if (!value) return;
    setError(null);
    setAdding(true);
    try {
      await addServiceEntryXrpcs(entry.id, [value]);
      setNewXrpc("");
      await loadXrpcs();
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setAdding(false);
    }
  }

  async function handleSave() {
    setError(null);
    setSaving(true);
    try {
      await updateServiceEntry(entry.id, { access_mode: accessMode });
      onSaved();
      onOpenChange(false);
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  }

  async function handleDelete() {
    setError(null);
    setDeleting(true);
    try {
      await deleteServiceEntry(entry.id);
      onSaved();
      onOpenChange(false);
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setDeleting(false);
    }
  }

  const allSelected = xrpcs.length > 0 && selected.size === xrpcs.length;
  const someSelected = selected.size > 0 && selected.size < xrpcs.length;

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent className="flex flex-col gap-0 overflow-y-auto">
        <SheetHeader className="border-b pb-4">
          <SheetTitle className="font-mono">{entry.fragment_id}</SheetTitle>
          <SheetDescription>{entry.service_type}</SheetDescription>
        </SheetHeader>

        <div className="flex flex-col gap-6 flex-1 p-4">
          {error && <p className="text-destructive text-sm">{error}</p>}

          <div className="flex flex-col gap-2">
            <p className="text-sm font-medium">XRPC Access</p>
            <div className="flex gap-2">
              <Button
                variant={accessMode === "all" ? "default" : "outline"}
                size="sm"
                onClick={() => setAccessMode("all")}
              >
                All XRPCs
              </Button>
              <Button
                variant={accessMode === "specific" ? "default" : "outline"}
                size="sm"
                onClick={() => {
                  setAccessMode("specific");
                  loadXrpcs();
                }}
              >
                Specific XRPCs
              </Button>
            </div>
          </div>

          {accessMode === "specific" && (
            <div className="flex flex-col gap-3">
              <div className="flex items-center justify-between">
                <p className="text-sm font-medium">Allowed XRPCs</p>
                {selected.size > 0 && (
                  <Button
                    variant="destructive"
                    size="sm"
                    onClick={handleRemoveSelected}
                  >
                    Remove Selected ({selected.size})
                  </Button>
                )}
              </div>

              <div className="overflow-clip rounded-lg border">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead className="w-10">
                        <Checkbox
                          checked={allSelected}
                          ref={(el) => {
                            if (el)
                              (
                                el as HTMLButtonElement & {
                                  indeterminate: boolean;
                                }
                              ).indeterminate = someSelected;
                          }}
                          onCheckedChange={toggleSelectAll}
                          aria-label="Select all"
                        />
                      </TableHead>
                      <TableHead>XRPC</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {xrpcs.length === 0 && (
                      <TableRow>
                        <TableCell
                          colSpan={2}
                          className="text-muted-foreground text-center text-sm"
                        >
                          No XRPCs configured. Add XRPCs that this service can
                          access.
                        </TableCell>
                      </TableRow>
                    )}
                    {xrpcs.map((xrpc) => (
                      <TableRow key={xrpc}>
                        <TableCell className="w-10">
                          <Checkbox
                            checked={selected.has(xrpc)}
                            onCheckedChange={() => toggleSelect(xrpc)}
                            aria-label={`Select ${xrpc}`}
                          />
                        </TableCell>
                        <TableCell className="font-mono text-sm">
                          {xrpc}
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </div>

              <div className="flex gap-2">
                <Input
                  className="font-mono"
                  placeholder="games.birb.chess.getGame"
                  value={newXrpc}
                  onChange={(e) => setNewXrpc(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") handleAddXrpc();
                  }}
                  disabled={adding}
                />
                <Button
                  size="sm"
                  onClick={handleAddXrpc}
                  disabled={adding || !newXrpc.trim()}
                >
                  {adding ? "Adding..." : "Add"}
                </Button>
              </div>
            </div>
          )}
        </div>

        <SheetFooter className="border-t pt-4 flex-row justify-between">
          <Button
            variant="destructive"
            size="sm"
            onClick={handleDelete}
            disabled={deleting}
          >
            <Trash2 className="size-4 mr-2" />
            {deleting ? "Deleting..." : "Delete Service"}
          </Button>
          <Button onClick={handleSave} disabled={saving} size="sm">
            {saving ? "Saving..." : "Save"}
          </Button>
        </SheetFooter>
      </SheetContent>
    </Sheet>
  );
}
