"use client";

import { useCallback, useEffect, useState } from "react";
import { Trash2 } from "lucide-react";
import { toast } from "sonner";

import { toastError } from "@/lib/format";
import {
  getServiceEntryXrpcs,
  updateServiceEntry,
  removeServiceEntryXrpcs,
  addServiceEntryXrpcs,
  deleteServiceEntry,
  type ServiceEntry,
} from "@/lib/api";
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
  const [saving, setSaving] = useState(false);
  const [deleting, setDeleting] = useState(false);

  const loadXrpcs = useCallback(async () => {
    if (accessMode !== "specific") return;
    try {
      const list = await getServiceEntryXrpcs(entry.id);
      setXrpcs(list);
    } catch (e: unknown) {
      toastError("Failed to load XRPC list", e);
    }
  }, [entry.id, accessMode]);

  useEffect(() => {
    if (open) {
      setAccessMode(entry.access_mode);
      setSelected(new Set());
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
    try {
      await removeServiceEntryXrpcs(entry.id, Array.from(selected));
      toast.success(`Removed ${selected.size} XRPC${selected.size > 1 ? "s" : ""}`);
      setSelected(new Set());
      await loadXrpcs();
    } catch (e: unknown) {
      toastError("Failed to remove XRPCs", e);
    }
  }

  async function handleAddXrpc() {
    const value = newXrpc.trim();
    if (!value) return;
    setAdding(true);
    try {
      await addServiceEntryXrpcs(entry.id, [value]);
      toast.success(`Added ${value}`);
      setNewXrpc("");
      await loadXrpcs();
    } catch (e: unknown) {
      toastError("Failed to add XRPC", e);
    } finally {
      setAdding(false);
    }
  }

  async function handleSave() {
    setSaving(true);
    try {
      await updateServiceEntry(entry.id, { access_mode: accessMode });
      toast.success("Service entry updated");
      onSaved();
      onOpenChange(false);
    } catch (e: unknown) {
      toastError("Failed to update service entry", e);
    } finally {
      setSaving(false);
    }
  }

  async function handleDelete() {
    setDeleting(true);
    try {
      await deleteServiceEntry(entry.id);
      toast.success(`Deleted ${entry.fragment_id}`);
      onSaved();
      onOpenChange(false);
    } catch (e: unknown) {
      toastError("Failed to delete service entry", e);
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
                          checked={allSelected || (someSelected && "indeterminate")}
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
                          No XRPCs configured. Add methods below that this
                          service entry can access.
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
                  {adding ? "Adding…" : "Add"}
                </Button>
              </div>
            </div>
          )}
        </div>

        <SheetFooter className="border-t pt-4 flex-row justify-between">
          <AlertDialog>
            <AlertDialogTrigger asChild>
              <Button variant="destructive" size="sm">
                <Trash2 className="size-4 mr-2" />
                Delete Service
              </Button>
            </AlertDialogTrigger>
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogTitle>
                  Delete {entry.fragment_id}?
                </AlertDialogTitle>
                <AlertDialogDescription>
                  This will permanently remove the service entry and its XRPC
                  configuration. The change will take effect in the DID document
                  after your next PLC sync.
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel>Cancel</AlertDialogCancel>
                <AlertDialogAction
                  variant="destructive"
                  onClick={handleDelete}
                  disabled={deleting}
                >
                  {deleting ? "Deleting…" : "Delete"}
                </AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>
          <Button onClick={handleSave} disabled={saving} size="sm">
            {saving ? "Saving…" : "Save"}
          </Button>
        </SheetFooter>
      </SheetContent>
    </Sheet>
  );
}
