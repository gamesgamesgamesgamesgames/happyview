"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useRouter } from "next/navigation";
import { AlertTriangle, HelpCircle, KeyRound, Plus, RefreshCw, Search, Trash2 } from "lucide-react";
import { toast } from "sonner";

import { useCurrentUser } from "@/hooks/use-current-user";
import { toastError } from "@/lib/format";
import {
  getServiceIdentity,
  getServiceEntries,
  createServiceEntry,
  deleteServiceEntry,
  updateServiceIdentity,
  syncPlc,
  syncPlcRequest,
  syncPlcSubmit,
  confirmAttachAuth,
  type ServiceIdentityResponse,
  type ServiceEntry,
} from "@/lib/api";
import { SiteHeader } from "@/components/site-header";
import { ServiceEntrySheet } from "@/components/service-entry-sheet";
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
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { Separator } from "@/components/ui/separator";
import { Skeleton } from "@/components/ui/skeleton";
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
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";

const SYNC_STORAGE_KEY = "happyview:service-identity:last-synced-at";
const REAUTH_STORAGE_KEY = "happyview:service-identity:reauth";
const REAUTH_MAX_AGE_MS = 10 * 60 * 1000;
const IS_MAC = typeof navigator !== "undefined" && /Mac|iPhone/.test(navigator.userAgent);
const MOD_KEY = IS_MAC ? "⌘" : "Ctrl+";
const FRAGMENT_ID_RE = /^#?[a-zA-Z][a-zA-Z0-9_-]*$/;

function formatMode(mode: string): string {
  switch (mode) {
    case "did_web": return "did:web (domain-based)";
    case "did_plc": return "did:plc (PLC directory)";
    case "attach_account": return "Attached account";
    case "not_exposed": return "Not exposed";
    default: return mode;
  }
}

function HelpTip({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <button type="button" className="inline-flex text-muted-foreground hover:text-foreground" aria-label={`Help for ${label}`}>
          <HelpCircle className="size-3.5" />
        </button>
      </TooltipTrigger>
      <TooltipContent side="top" className="max-w-64">
        {children}
      </TooltipContent>
    </Tooltip>
  );
}

function getLastSyncedAt(): string | null {
  try {
    return localStorage.getItem(SYNC_STORAGE_KEY);
  } catch {
    return null;
  }
}

function setLastSyncedAt() {
  try {
    localStorage.setItem(SYNC_STORAGE_KEY, new Date().toISOString());
  } catch {
    // localStorage unavailable
  }
}

export default function ServiceIdentityPage() {
  const router = useRouter();
  const { hasPermission } = useCurrentUser();
  const canManage = hasPermission("settings:manage");
  const [changingMode, setChangingMode] = useState(false);
  const [loading, setLoading] = useState(true);

  const [identity, setIdentity] = useState<ServiceIdentityResponse | null>(null);
  const [entries, setEntries] = useState<ServiceEntry[]>([]);

  const [fragmentId, setFragmentId] = useState("");
  const [serviceType, setServiceType] = useState("");
  const [adding, setAdding] = useState(false);
  const [addSheetOpen, setAddSheetOpen] = useState(false);
  const [filterQuery, setFilterQuery] = useState("");

  const [selectedEntry, setSelectedEntry] = useState<ServiceEntry | null>(null);
  const [editSheetOpen, setEditSheetOpen] = useState(false);

  // PLC sync state — did_plc uses a popover, attach_account uses a sheet
  const [syncPopoverOpen, setSyncPopoverOpen] = useState(false);
  const [syncSheetOpen, setSyncSheetOpen] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const [requestingCode, setRequestingCode] = useState(false);
  const [codeRequested, setCodeRequested] = useState(false);
  const [plcToken, setPlcToken] = useState("");
  const [submittingToken, setSubmittingToken] = useState(false);
  const [sessionDirty, setSessionDirty] = useState(false);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [bulkDeleting, setBulkDeleting] = useState(false);
  const [reauthing, setReauthing] = useState(false);

  const fragmentIdRef = useRef<HTMLInputElement>(null);

  const fragmentIdError = fragmentId.trim() && !FRAGMENT_ID_RE.test(fragmentId.trim())
    ? "Must start with a letter and contain only letters, numbers, hyphens, and underscores."
    : null;

  const filteredEntries = useMemo(() => {
    if (!filterQuery) return entries;
    const q = filterQuery.toLowerCase();
    return entries.filter(
      (e) =>
        e.fragment_id.toLowerCase().includes(q) ||
        e.service_type.toLowerCase().includes(q),
    );
  }, [entries, filterQuery]);

  const needsSync = useMemo(() => {
    if (sessionDirty) return true;
    const lastSynced = getLastSyncedAt();
    if (!lastSynced || entries.length === 0) return false;
    return entries.some((e) => e.updated_at > lastSynced);
  }, [entries, sessionDirty]);

  const load = useCallback(async () => {
    try {
      const [id, ents] = await Promise.all([
        getServiceIdentity(),
        getServiceEntries(),
      ]);
      setIdentity(id);
      setEntries(ents);
    } catch (e: unknown) {
      toastError("Failed to load service identity", e);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  useEffect(() => {
    if (selected.size === 0) return;
    const validIds = new Set(entries.map((e) => e.id));
    setSelected((prev) => {
      const pruned = new Set([...prev].filter((id) => validIds.has(id)));
      return pruned.size === prev.size ? prev : pruned;
    });
  }, [entries]);

  useEffect(() => {
    if (!canManage) return;
    function onKeyDown(e: KeyboardEvent) {
      if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement) return;
      const mod = e.metaKey || e.ctrlKey;
      if (!mod) return;

      if (e.key === "n") {
        e.preventDefault();
        setAddSheetOpen(true);
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [canManage]);

  useEffect(() => {
    const stored = localStorage.getItem(REAUTH_STORAGE_KEY);
    if (!stored) return;

    let payload: { originalDid: string; timestamp?: number };
    try {
      payload = JSON.parse(stored);
    } catch {
      localStorage.removeItem(REAUTH_STORAGE_KEY);
      return;
    }

    if (payload.timestamp && Date.now() - payload.timestamp > REAUTH_MAX_AGE_MS) {
      localStorage.removeItem(REAUTH_STORAGE_KEY);
      return;
    }

    localStorage.removeItem(REAUTH_STORAGE_KEY);
    setReauthing(true);

    confirmAttachAuth({ original_did: payload.originalDid })
      .then(() => {
        toast.success("PDS session refreshed");
        load();
      })
      .catch((e) => {
        toastError("Failed to restore admin session", e);
      })
      .finally(() => setReauthing(false));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  function handleReauthenticate() {
    if (!identity?.attached_account_did) return;
    setReauthing(true);

    fetch("/auth/me", { credentials: "same-origin" })
      .then((res) => {
        if (!res.ok) throw new Error("Failed to fetch current user");
        return res.json() as Promise<{ did: string }>;
      })
      .then(({ did: originalDid }) => {
        localStorage.setItem(
          REAUTH_STORAGE_KEY,
          JSON.stringify({ originalDid, timestamp: Date.now() }),
        );
        return fetch(
          `/auth/login?handle=${encodeURIComponent(identity.attached_account_did!)}&scope=${encodeURIComponent("atproto identity:*")}&redirect_uri=${encodeURIComponent("/dashboard/settings/service-identity")}`,
          { credentials: "same-origin" },
        );
      })
      .then((resp) => {
        if (!resp.ok) throw new Error("Login request failed");
        return resp.json() as Promise<{ url: string }>;
      })
      .then(({ url }) => {
        window.location.href = url;
      })
      .catch((e) => {
        toastError("Failed to start re-authentication", e);
        setReauthing(false);
      });
  }

  const showSyncButton = canManage && identity &&
    (identity.mode === "did_plc" || identity.mode === "attach_account");

  async function handleAdd() {
    setAdding(true);
    try {
      const fid = fragmentId.startsWith("#") ? fragmentId : `#${fragmentId}`;
      await createServiceEntry({ fragment_id: fid, service_type: serviceType });
      setFragmentId("");
      setServiceType("");
      setAddSheetOpen(false);
      setSessionDirty(true);
      toast.success("Service entry added", {
        description: "Sync to the PLC directory to publish this change.",
      });
      await load();
    } catch (e: unknown) {
      toastError("Failed to add service entry", e);
    } finally {
      setAdding(false);
    }
  }

  async function handleDelete(entry: ServiceEntry) {
    try {
      await deleteServiceEntry(entry.id);
      setSelected((prev) => {
        if (!prev.has(entry.id)) return prev;
        const next = new Set(prev);
        next.delete(entry.id);
        return next;
      });
      setSessionDirty(true);
      toast.success(`Deleted ${entry.fragment_id}`, {
        description: "Sync to the PLC directory to publish this change.",
      });
      await load();
    } catch (e: unknown) {
      toastError("Failed to delete service entry", e);
    }
  }

  function handleEntryClick(entry: ServiceEntry) {
    setSelectedEntry(entry);
    setEditSheetOpen(true);
  }

  function handleEntrySaved() {
    setSessionDirty(true);
    load();
  }

  function toggleSelectAll() {
    if (selected.size === filteredEntries.length) {
      setSelected(new Set());
    } else {
      setSelected(new Set(filteredEntries.map((e) => e.id)));
    }
  }

  function toggleSelect(id: number) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  async function handleBulkDelete() {
    setBulkDeleting(true);
    const ids = Array.from(selected);
    const results = await Promise.allSettled(ids.map((id) => deleteServiceEntry(id)));
    const succeeded = ids.filter((_, i) => results[i].status === "fulfilled");
    const failed = ids.length - succeeded.length;

    if (succeeded.length > 0) {
      setSelected((prev) => {
        const next = new Set(prev);
        for (const id of succeeded) next.delete(id);
        return next;
      });
      setSessionDirty(true);
    }

    if (failed === 0) {
      toast.success(`Deleted ${succeeded.length} service ${succeeded.length === 1 ? "entry" : "entries"}`, {
        description: "Sync to the PLC directory to publish this change.",
      });
    } else if (succeeded.length === 0) {
      toast.error("Failed to delete service entries");
    } else {
      toast.warning(`Deleted ${succeeded.length} of ${ids.length} entries`, {
        description: `${failed} ${failed === 1 ? "entry" : "entries"} failed to delete.`,
      });
    }

    await load();
    setBulkDeleting(false);
  }

  const allSelected = filteredEntries.length > 0 && selected.size === filteredEntries.length;
  const someSelected = selected.size > 0 && selected.size < filteredEntries.length;

  async function handleSyncPlc() {
    setSyncing(true);
    try {
      await syncPlc();
      setSessionDirty(false);
      setLastSyncedAt();
      setSyncPopoverOpen(false);
      toast.success("DID document synced", {
        description: "Your service entries are now published to the PLC directory.",
      });
    } catch (e: unknown) {
      toastError("Failed to sync to PLC directory", e);
    } finally {
      setSyncing(false);
    }
  }

  async function handleSyncPlcRequest() {
    setRequestingCode(true);
    try {
      await syncPlcRequest();
      setCodeRequested(true);
      toast.success("Confirmation code sent", {
        description: "Check the inbox for the attached account's email.",
      });
    } catch (e: unknown) {
      toastError("Failed to request confirmation code", e);
    } finally {
      setRequestingCode(false);
    }
  }

  async function handleSyncPlcSubmit() {
    setSubmittingToken(true);
    try {
      await syncPlcSubmit(plcToken);
      setSessionDirty(false);
      setLastSyncedAt();
      setSyncSheetOpen(false);
      setCodeRequested(false);
      setPlcToken("");
      toast.success("DID document synced", {
        description: "Your service entries are now published to the PLC directory.",
      });
    } catch (e: unknown) {
      toastError("Failed to submit confirmation code", e);
    } finally {
      setSubmittingToken(false);
    }
  }

  async function handleConfirmChangeMode() {
    setChangingMode(true);
    try {
      await updateServiceIdentity({ mode: "not_exposed" });
      router.push("/setup");
    } catch (e: unknown) {
      toastError("Failed to change identity mode", e);
      setChangingMode(false);
    }
  }

  function handleAddKeyDown(e: React.KeyboardEvent) {
    if (e.key === "Enter" && fragmentId.trim() && serviceType.trim() && !adding && !fragmentIdError) {
      handleAdd();
    }
  }

  return (
    <>
      <SiteHeader title="Service Identity" />
      <div className="flex flex-1 flex-col gap-6 p-4 md:p-6">

        {/* Identity metadata grid */}
        {loading ? (
          <div className="grid grid-cols-2 gap-x-8 gap-y-3 sm:grid-cols-3 lg:grid-cols-4">
            {[1, 2, 3].map((i) => (
              <div key={i}>
                <Skeleton className="h-4 w-16 mb-1" />
                <Skeleton className="h-5 w-32" />
              </div>
            ))}
          </div>
        ) : identity ? (
          <div className="grid grid-cols-2 gap-x-8 gap-y-3 sm:grid-cols-3 lg:grid-cols-4">
            <div>
              <Label className="text-muted-foreground">
                <span className="inline-flex items-center gap-1.5">
                  Mode
                  <HelpTip label="Mode">
                    {identity.mode === "not_exposed"
                      ? "Your service is not discoverable through a DID document. Other services cannot resolve your endpoints."
                      : "Determines how your service identity is published and verified on the AT Protocol network."}
                  </HelpTip>
                </span>
              </Label>
              <div className="mt-1">
                <Badge variant="outline">{formatMode(identity.mode)}</Badge>
              </div>
            </div>
            <div className="col-span-2">
              <Label className="text-muted-foreground">
                <span className="inline-flex items-center gap-1.5">
                  DID
                  <HelpTip label="DID">
                    Your decentralized identifier — a globally unique address
                    that other services use to find and verify this AppView.
                  </HelpTip>
                </span>
              </Label>
              <p className="mt-1 font-mono text-sm break-all">
                {identity.did ?? (identity.mode === "did_web" ? `did:web:${typeof window !== "undefined" ? window.location.host : "…"}` : <em className="text-muted-foreground">not set</em>)}
              </p>
            </div>
            <div>
              <Label className="text-muted-foreground">
                <span className="inline-flex items-center gap-1.5">
                  Status
                  <HelpTip label="Status">
                    {identity.setup_complete
                      ? "Setup is complete and your identity is ready to use."
                      : "Setup has not been completed. Run the setup wizard to finish configuring your identity."}
                  </HelpTip>
                </span>
              </Label>
              <div className="mt-1">
                <Badge variant={identity.setup_complete ? "secondary" : "outline"}>
                  {identity.setup_complete ? "Complete" : "Incomplete"}
                </Badge>
              </div>
            </div>
          </div>
        ) : (
          <div className="flex flex-col gap-2">
            <p className="text-muted-foreground text-sm">No identity configured.</p>
            <Button variant="outline" size="sm" className="w-fit" onClick={() => router.push("/setup")}>
              Run Setup Wizard
            </Button>
          </div>
        )}

        <Separator />

        {/* Action bar */}
        <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
          <h2 className="text-lg font-semibold">Service Entries</h2>
          <div className="flex items-center gap-2 flex-wrap">
            {canManage && (
              <AlertDialog>
                <AlertDialogTrigger asChild>
                  <Button variant="ghost" size="sm">
                    Change Mode
                  </Button>
                </AlertDialogTrigger>
                <AlertDialogContent>
                  <AlertDialogHeader>
                    <AlertDialogTitle>Change identity mode?</AlertDialogTitle>
                    <AlertDialogDescription asChild>
                      <div className="flex flex-col gap-2">
                        <p>
                          This will reset your service identity configuration
                          and redirect you to the setup wizard.
                        </p>
                        <p className="font-medium text-foreground">
                          What changes:
                        </p>
                        <ul className="list-disc pl-4 text-sm">
                          <li>DID and signing keys will be regenerated</li>
                          <li>PLC directory state will need to be re-synced</li>
                        </ul>
                        <p className="font-medium text-foreground">
                          What stays:
                        </p>
                        <ul className="list-disc pl-4 text-sm">
                          <li>Service entries are preserved</li>
                          <li>Records and lexicons are unaffected</li>
                        </ul>
                      </div>
                    </AlertDialogDescription>
                  </AlertDialogHeader>
                  <AlertDialogFooter>
                    <AlertDialogCancel>Cancel</AlertDialogCancel>
                    <AlertDialogAction
                      variant="destructive"
                      onClick={handleConfirmChangeMode}
                      disabled={changingMode}
                    >
                      {changingMode ? "Resetting…" : "Continue"}
                    </AlertDialogAction>
                  </AlertDialogFooter>
                </AlertDialogContent>
              </AlertDialog>
            )}
            {canManage && identity?.mode === "attach_account" && identity.attached_account_did && (
              <Button
                variant="outline"
                size="sm"
                onClick={handleReauthenticate}
                disabled={reauthing}
              >
                <KeyRound className="size-3.5" />
                {reauthing ? "Redirecting…" : "Re-authenticate"}
              </Button>
            )}
            {showSyncButton && (
              identity?.mode === "did_plc" ? (
                needsSync ? (
                  <Popover open={syncPopoverOpen} onOpenChange={setSyncPopoverOpen}>
                    <PopoverTrigger asChild>
                      <Button variant="outline" size="sm">
                        <AlertTriangle className="size-3.5 text-amber-500" />
                        <RefreshCw className="size-3.5" />
                        Sync
                      </Button>
                    </PopoverTrigger>
                    <PopoverContent className="w-80">
                      <div className="flex flex-col gap-3">
                        <p className="text-sm font-medium">Sync to PLC Directory</p>
                        <p className="text-sm text-muted-foreground">
                          Publish your current service entries. This signs and
                          submits a PLC update operation. Changes take effect
                          immediately.
                        </p>
                        <div className="flex justify-end gap-2">
                          <Button
                            variant="outline"
                            size="sm"
                            onClick={() => setSyncPopoverOpen(false)}
                          >
                            Cancel
                          </Button>
                          <Button
                            size="sm"
                            onClick={handleSyncPlc}
                            disabled={syncing}
                          >
                            {syncing ? "Syncing…" : "Sync Now"}
                          </Button>
                        </div>
                      </div>
                    </PopoverContent>
                  </Popover>
                ) : (
                  <Tooltip>
                    <TooltipTrigger asChild>
                      <span>
                        <Button variant="outline" size="sm" disabled>
                          <RefreshCw className="size-3.5" />
                          Sync
                        </Button>
                      </span>
                    </TooltipTrigger>
                    <TooltipContent>
                      Your DID document is up to date.
                    </TooltipContent>
                  </Tooltip>
                )
              ) : needsSync ? (
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setSyncSheetOpen(true)}
                >
                  <AlertTriangle className="size-3.5 text-amber-500" />
                  <RefreshCw className="size-3.5" />
                  Sync
                </Button>
              ) : (
                <Tooltip>
                  <TooltipTrigger asChild>
                    <span>
                      <Button variant="outline" size="sm" disabled>
                        <RefreshCw className="size-3.5" />
                        Sync
                      </Button>
                    </span>
                  </TooltipTrigger>
                  <TooltipContent>
                    Your DID document is up to date.
                  </TooltipContent>
                </Tooltip>
              )
            )}
            {canManage && (
              <Tooltip>
                <TooltipTrigger asChild>
                  <Button size="sm" onClick={() => setAddSheetOpen(true)}>
                    <Plus className="size-3.5" />
                    Add Service Entry
                  </Button>
                </TooltipTrigger>
                <TooltipContent>{MOD_KEY}N</TooltipContent>
              </Tooltip>
            )}
          </div>
        </div>

        {/* Bulk action bar */}
        {selected.size > 0 && (
          <div className="flex items-center gap-3 rounded-lg border bg-muted/50 px-4 py-2">
            <span className="text-sm font-medium">
              {selected.size} {filterQuery ? `of ${entries.length} ` : ""}{selected.size === 1 ? "entry" : "entries"} selected
            </span>
            <AlertDialog>
              <AlertDialogTrigger asChild>
                <Button variant="destructive" size="sm" disabled={bulkDeleting}>
                  <Trash2 className="size-3.5" />
                  {bulkDeleting ? "Deleting…" : "Delete Selected"}
                </Button>
              </AlertDialogTrigger>
              <AlertDialogContent>
                <AlertDialogHeader>
                  <AlertDialogTitle>
                    Delete {selected.size} service {selected.size === 1 ? "entry" : "entries"}?
                  </AlertDialogTitle>
                  <AlertDialogDescription>
                    This will remove the selected service entries from your
                    configuration. Changes take effect in the DID document
                    after your next PLC sync.
                  </AlertDialogDescription>
                </AlertDialogHeader>
                <AlertDialogFooter>
                  <AlertDialogCancel>Cancel</AlertDialogCancel>
                  <AlertDialogAction
                    variant="destructive"
                    onClick={handleBulkDelete}
                    disabled={bulkDeleting}
                  >
                    {bulkDeleting ? "Deleting…" : "Delete"}
                  </AlertDialogAction>
                </AlertDialogFooter>
              </AlertDialogContent>
            </AlertDialog>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setSelected(new Set())}
            >
              Clear Selection
            </Button>
          </div>
        )}

        {/* Filter */}
        {!loading && entries.length > 0 && (
          <div className="relative">
            <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 size-3.5 text-muted-foreground" />
            <Input
              value={filterQuery}
              onChange={(e) => setFilterQuery(e.target.value)}
              placeholder="Filter entries…"
              aria-label="Filter service entries"
              className="pl-8 h-9"
            />
          </div>
        )}

        {/* Service entries table */}
        {loading ? (
          <div className="overflow-clip rounded-lg border">
            <Table>
              <TableHeader>
                <TableRow>
                  {canManage && <TableHead className="w-10" />}
                  <TableHead>Fragment ID</TableHead>
                  <TableHead>Type</TableHead>
                  <TableHead>XRPC Access</TableHead>
                  {canManage && <TableHead className="w-10" />}
                </TableRow>
              </TableHeader>
              <TableBody>
                {[1, 2].map((i) => (
                  <TableRow key={i}>
                    {canManage && <TableCell><Skeleton className="size-4" /></TableCell>}
                    <TableCell><Skeleton className="h-4 w-28" /></TableCell>
                    <TableCell><Skeleton className="h-4 w-40" /></TableCell>
                    <TableCell><Skeleton className="h-5 w-20 rounded-full" /></TableCell>
                    {canManage && <TableCell><Skeleton className="size-8" /></TableCell>}
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        ) : entries.length === 0 ? (
          <div className="flex flex-col items-center justify-center gap-3 rounded-lg border border-dashed py-16 px-4">
            <p className="text-muted-foreground text-sm text-center">
              No service entries yet.
            </p>
            <p className="text-muted-foreground text-xs text-center max-w-sm">
              Service entries define which XRPC endpoints are accessible through
              your DID document. Each entry maps a fragment identifier to a
              service type.
            </p>
            {canManage && (
              <Button
                size="sm"
                variant="outline"
                className="mt-1"
                onClick={() => setAddSheetOpen(true)}
              >
                <Plus className="size-3.5" />
                Add Service Entry
              </Button>
            )}
          </div>
        ) : (
          <div className="overflow-clip rounded-lg border">
            <Table>
              <TableHeader>
                <TableRow>
                  {canManage && (
                    <TableHead className="w-10">
                      <Checkbox
                        checked={allSelected || (someSelected && "indeterminate")}
                        onCheckedChange={toggleSelectAll}
                        aria-label="Select all"
                      />
                    </TableHead>
                  )}
                  <TableHead>
                    <span className="inline-flex items-center gap-1.5">
                      Fragment ID
                      <HelpTip label="Fragment ID">
                        A unique identifier within the DID document
                        (e.g. <code className="text-xs">#atproto_pds</code>).
                        Used by clients to locate this service endpoint.
                      </HelpTip>
                    </span>
                  </TableHead>
                  <TableHead>
                    <span className="inline-flex items-center gap-1.5">
                      Type
                      <HelpTip label="Service Type">
                        The AT Protocol service type this entry represents
                        (e.g. AtprotoPersonalDataServer, BskyAppView).
                      </HelpTip>
                    </span>
                  </TableHead>
                  <TableHead>
                    <span className="inline-flex items-center gap-1.5">
                      XRPC Access
                      <HelpTip label="XRPC Access">
                        Controls which XRPC methods this service can handle.
                        &quot;All&quot; allows every method; &quot;Specific&quot; restricts
                        to an allowlist.
                      </HelpTip>
                    </span>
                  </TableHead>
                  {canManage && <TableHead className="w-10" />}
                </TableRow>
              </TableHeader>
              <TableBody>
                {filteredEntries.length === 0 && filterQuery && (
                  <TableRow>
                    <TableCell
                      colSpan={canManage ? 5 : 3}
                      className="text-center py-8"
                    >
                      <p className="text-muted-foreground text-sm">
                        No entries match &ldquo;{filterQuery}&rdquo;
                      </p>
                      <Button
                        variant="ghost"
                        size="sm"
                        className="mt-2"
                        onClick={() => setFilterQuery("")}
                      >
                        Clear filter
                      </Button>
                    </TableCell>
                  </TableRow>
                )}
                {filteredEntries.map((entry) => (
                  <TableRow key={entry.id} data-state={selected.has(entry.id) ? "selected" : undefined}>
                    {canManage && (
                      <TableCell className="w-10">
                        <Checkbox
                          checked={selected.has(entry.id)}
                          onCheckedChange={() => toggleSelect(entry.id)}
                          aria-label={`Select ${entry.fragment_id}`}
                        />
                      </TableCell>
                    )}
                    <TableCell>
                      <button
                        type="button"
                        className="font-mono text-sm text-primary hover:underline cursor-pointer"
                        onClick={() => handleEntryClick(entry)}
                      >
                        {entry.fragment_id}
                      </button>
                    </TableCell>
                    <TableCell className="text-sm">{entry.service_type}</TableCell>
                    <TableCell>
                      <Badge variant="secondary">
                        {entry.access_mode === "all" ? "All XRPCs" : "Specific"}
                      </Badge>
                    </TableCell>
                    {canManage && (
                      <TableCell className="w-10">
                        <AlertDialog>
                          <AlertDialogTrigger asChild>
                            <Button
                              variant="ghost"
                              size="icon"
                              aria-label={`Delete ${entry.fragment_id}`}
                            >
                              <Trash2 className="size-4" />
                            </Button>
                          </AlertDialogTrigger>
                          <AlertDialogContent>
                            <AlertDialogHeader>
                              <AlertDialogTitle>
                                Delete {entry.fragment_id}?
                              </AlertDialogTitle>
                              <AlertDialogDescription>
                                This will remove the service entry from your
                                configuration. The change will take effect in the
                                DID document after your next PLC sync.
                              </AlertDialogDescription>
                            </AlertDialogHeader>
                            <AlertDialogFooter>
                              <AlertDialogCancel>Cancel</AlertDialogCancel>
                              <AlertDialogAction
                                variant="destructive"
                                onClick={() => handleDelete(entry)}
                              >
                                Delete
                              </AlertDialogAction>
                            </AlertDialogFooter>
                          </AlertDialogContent>
                        </AlertDialog>
                      </TableCell>
                    )}
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        )}
      </div>

      {/* Edit service entry sheet */}
      {selectedEntry && (
        <ServiceEntrySheet
          entry={selectedEntry}
          open={editSheetOpen}
          onOpenChange={setEditSheetOpen}
          onSaved={handleEntrySaved}
        />
      )}

      {/* Add service entry sheet */}
      <Sheet open={addSheetOpen} onOpenChange={(open) => {
        setAddSheetOpen(open);
        if (!open) {
          setFragmentId("");
          setServiceType("");
        } else {
          requestAnimationFrame(() => fragmentIdRef.current?.focus());
        }
      }}>
        <SheetContent className="flex flex-col gap-0">
          <SheetHeader className="border-b pb-4">
            <SheetTitle>Add Service Entry</SheetTitle>
            <SheetDescription>
              Register a new service endpoint in your DID document.
            </SheetDescription>
          </SheetHeader>

          <div className="flex flex-col gap-4 flex-1 p-4">
            <div className="flex flex-col gap-2">
              <Label htmlFor="fragment_id">
                <span className="inline-flex items-center gap-1.5">
                  Fragment ID
                  <HelpTip label="Fragment ID">
                    A short identifier prefixed with # that names this service
                    in the DID document. Common values:{" "}
                    <code className="text-xs">#atproto_pds</code>,{" "}
                    <code className="text-xs">#bsky_appview</code>.
                  </HelpTip>
                </span>
              </Label>
              <Input
                ref={fragmentIdRef}
                id="fragment_id"
                value={fragmentId}
                onChange={(e) => setFragmentId(e.target.value)}
                onKeyDown={handleAddKeyDown}
                placeholder="#atproto_pds"
              />
              {fragmentIdError ? (
                <p className="text-destructive text-xs">{fragmentIdError}</p>
              ) : (
                <p className="text-muted-foreground text-xs">
                  A <code>#</code> will be prepended automatically if omitted.
                </p>
              )}
            </div>
            <div className="flex flex-col gap-2">
              <Label htmlFor="service_type">
                <span className="inline-flex items-center gap-1.5">
                  Service Type
                  <HelpTip label="Service Type">
                    The AT Protocol service type identifier. Examples:{" "}
                    <code className="text-xs">AtprotoPersonalDataServer</code>,{" "}
                    <code className="text-xs">BskyAppView</code>,{" "}
                    <code className="text-xs">BskyLabeler</code>.
                  </HelpTip>
                </span>
              </Label>
              <Input
                id="service_type"
                value={serviceType}
                onChange={(e) => setServiceType(e.target.value)}
                onKeyDown={handleAddKeyDown}
                placeholder="AtprotoPersonalDataServer"
              />
            </div>
          </div>

          <SheetFooter className="border-t pt-4">
            <Button
              onClick={handleAdd}
              disabled={adding || !fragmentId.trim() || !serviceType.trim() || !!fragmentIdError}
            >
              {adding ? "Adding…" : "Add Entry"}
            </Button>
          </SheetFooter>
        </SheetContent>
      </Sheet>

      {/* Sync PLC sheet — attach_account mode only (did_plc uses popover) */}
      <Sheet open={syncSheetOpen} onOpenChange={(open) => {
        setSyncSheetOpen(open);
        if (!open) {
          setCodeRequested(false);
          setPlcToken("");
        }
      }}>
        <SheetContent className="flex flex-col gap-0">
          <SheetHeader className="border-b pb-4">
            <SheetTitle>Sync to PLC Directory</SheetTitle>
            <SheetDescription>
              Publish your current service entries to the{" "}
              <a
                href="https://web.plc.directory"
                target="_blank"
                rel="noopener noreferrer"
                className="underline hover:text-foreground"
              >
                PLC directory
              </a>
              . This updates your public DID document so clients can discover
              your service endpoints.
            </SheetDescription>
          </SheetHeader>

          <div className="flex flex-col gap-4 flex-1 p-4">
            {!codeRequested && (
              <div className="flex flex-col gap-3">
                <p className="text-sm text-muted-foreground">
                  Because this identity is attached to an existing account,
                  syncing requires a confirmation code sent to the
                  account&apos;s email address.
                </p>
                <Button
                  variant="outline"
                  onClick={handleSyncPlcRequest}
                  disabled={requestingCode}
                >
                  {requestingCode ? "Sending…" : "Request Code"}
                </Button>
              </div>
            )}

            {codeRequested && (
              <div className="flex flex-col gap-3">
                <p className="text-sm text-muted-foreground">
                  Check the inbox for the attached account&apos;s email.
                  The code expires after a few minutes.
                </p>
                <div className="flex flex-col gap-2">
                  <Label htmlFor="plc_token">Confirmation Code</Label>
                  <Input
                    id="plc_token"
                    value={plcToken}
                    onChange={(e) => setPlcToken(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" && plcToken.trim() && !submittingToken) {
                        handleSyncPlcSubmit();
                      }
                    }}
                    placeholder="Enter the code from your email"
                  />
                </div>
                <div className="flex items-center gap-2">
                  <Button
                    variant="outline"
                    onClick={() => {
                      setCodeRequested(false);
                      setPlcToken("");
                    }}
                  >
                    Cancel
                  </Button>
                  <Button
                    onClick={handleSyncPlcSubmit}
                    disabled={submittingToken || !plcToken.trim()}
                  >
                    {submittingToken ? "Submitting…" : "Sync"}
                  </Button>
                  <Button
                    variant="ghost"
                    className="ml-auto"
                    onClick={handleSyncPlcRequest}
                    disabled={requestingCode}
                  >
                    {requestingCode ? "Sending…" : "Resend Code"}
                  </Button>
                </div>
              </div>
            )}
          </div>
        </SheetContent>
      </Sheet>
    </>
  );
}
