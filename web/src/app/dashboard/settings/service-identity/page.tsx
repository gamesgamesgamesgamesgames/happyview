"use client";

import { useCallback, useEffect, useState } from "react";
import { useRouter } from "next/navigation";
import { Trash2 } from "lucide-react";

import { useCurrentUser } from "@/hooks/use-current-user";
import {
  getServiceIdentity,
  getServiceEntries,
  createServiceEntry,
  deleteServiceEntry,
  updateServiceIdentity,
  syncPlc,
  syncPlcRequest,
  syncPlcSubmit,
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
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

function formatMode(mode: string): string {
  switch (mode) {
    case "did_web": return "did:web (domain-based)";
    case "did_plc": return "did:plc (PLC directory)";
    case "attach_account": return "Attached account";
    case "not_exposed": return "Not exposed";
    default: return mode;
  }
}

export default function ServiceIdentityPage() {
  const router = useRouter();
  const { hasPermission } = useCurrentUser();
  const canManage = hasPermission("settings:manage");
  const [changingMode, setChangingMode] = useState(false);

  const [identity, setIdentity] = useState<ServiceIdentityResponse | null>(
    null,
  );
  const [entries, setEntries] = useState<ServiceEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const [fragmentId, setFragmentId] = useState("");
  const [serviceType, setServiceType] = useState("");
  const [adding, setAdding] = useState(false);

  const [selectedEntry, setSelectedEntry] = useState<ServiceEntry | null>(null);
  const [sheetOpen, setSheetOpen] = useState(false);

  // PLC sync state
  const [syncing, setSyncing] = useState(false);
  const [syncSuccess, setSyncSuccess] = useState<string | null>(null);
  const [syncError, setSyncError] = useState<string | null>(null);
  const [requestingCode, setRequestingCode] = useState(false);
  const [codeRequested, setCodeRequested] = useState(false);
  const [plcToken, setPlcToken] = useState("");
  const [submittingToken, setSubmittingToken] = useState(false);

  const load = useCallback(async () => {
    try {
      const [id, ents] = await Promise.all([
        getServiceIdentity(),
        getServiceEntries(),
      ]);
      setIdentity(id);
      setEntries(ents);
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  async function handleAdd() {
    setError(null);
    setNotice(null);
    setAdding(true);
    try {
      const fid = fragmentId.startsWith("#") ? fragmentId : `#${fragmentId}`;
      await createServiceEntry({ fragment_id: fid, service_type: serviceType });
      setFragmentId("");
      setServiceType("");
      setNotice("Service entry added.");
      await load();
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setAdding(false);
    }
  }

  async function handleDelete(id: number) {
    setError(null);
    setNotice(null);
    try {
      await deleteServiceEntry(id);
      setNotice("Service entry deleted.");
      await load();
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  function handleEntryClick(entry: ServiceEntry) {
    setSelectedEntry(entry);
    setSheetOpen(true);
  }

  async function handleSyncPlc() {
    setSyncError(null);
    setSyncSuccess(null);
    setSyncing(true);
    try {
      await syncPlc();
      setSyncSuccess("DID document synced to PLC directory.");
    } catch (e: unknown) {
      setSyncError(e instanceof Error ? e.message : String(e));
    } finally {
      setSyncing(false);
    }
  }

  async function handleSyncPlcRequest() {
    setSyncError(null);
    setSyncSuccess(null);
    setRequestingCode(true);
    try {
      await syncPlcRequest();
      setCodeRequested(true);
      setSyncSuccess("Confirmation code sent to the attached account's email.");
    } catch (e: unknown) {
      setSyncError(e instanceof Error ? e.message : String(e));
    } finally {
      setRequestingCode(false);
    }
  }

  async function handleSyncPlcSubmit() {
    setSyncError(null);
    setSyncSuccess(null);
    setSubmittingToken(true);
    try {
      await syncPlcSubmit(plcToken);
      setSyncSuccess("DID document synced to PLC directory.");
      setCodeRequested(false);
      setPlcToken("");
    } catch (e: unknown) {
      setSyncError(e instanceof Error ? e.message : String(e));
    } finally {
      setSubmittingToken(false);
    }
  }

  async function handleConfirmChangeMode() {
    setError(null);
    setChangingMode(true);
    try {
      await updateServiceIdentity({ mode: "not_exposed" });
      router.push("/setup");
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
      setChangingMode(false);
    }
  }

  return (
    <>
      <SiteHeader title="Service Identity" />
      <div className="flex flex-1 flex-col gap-6 p-4 md:p-6 max-w-3xl">
        {error && <p className="text-destructive text-sm">{error}</p>}
        {notice && (
          <p className="text-sm text-green-600 dark:text-green-400">{notice}</p>
        )}

        <Card>
          <CardHeader>
            <div className="flex items-center justify-between">
              <div>
                <CardTitle>Identity Configuration</CardTitle>
                <CardDescription>
                  {identity
                    ? formatMode(identity.mode)
                    : "No identity configured"}
                </CardDescription>
              </div>
              {canManage && (
                <AlertDialog>
                  <AlertDialogTrigger asChild>
                    <Button variant="outline" size="sm">
                      Change Mode
                    </Button>
                  </AlertDialogTrigger>
                  <AlertDialogContent>
                    <AlertDialogHeader>
                      <AlertDialogTitle>Change identity mode?</AlertDialogTitle>
                      <AlertDialogDescription>
                        Changing identity mode will reset your service identity
                        configuration. Service entries will be preserved but the
                        DID and signing keys will be regenerated.
                      </AlertDialogDescription>
                    </AlertDialogHeader>
                    <AlertDialogFooter>
                      <AlertDialogCancel>Cancel</AlertDialogCancel>
                      <AlertDialogAction
                        onClick={handleConfirmChangeMode}
                        disabled={changingMode}
                      >
                        {changingMode ? "Resetting…" : "Continue"}
                      </AlertDialogAction>
                    </AlertDialogFooter>
                  </AlertDialogContent>
                </AlertDialog>
              )}
            </div>
          </CardHeader>
          {identity && (
            <CardContent className="flex flex-col gap-3">
              <div className="flex items-center gap-2">
                <span className="text-sm font-medium text-muted-foreground w-24">
                  Mode
                </span>
                <span className="text-sm">{formatMode(identity.mode)}</span>
              </div>
              <div className="flex items-center gap-2">
                <span className="text-sm font-medium text-muted-foreground w-24">
                  DID
                </span>
                <span className="text-sm font-mono">
                  {identity.did ?? (identity.mode === "did_web" ? `did:web:${window.location.host}` : <em>not set</em>)}
                </span>
              </div>
              <div className="flex items-center gap-2">
                <span className="text-sm font-medium text-muted-foreground w-24">
                  Status
                </span>
                <span className="text-sm">
                  {identity.setup_complete ? "Complete" : "Incomplete"}
                </span>
              </div>
            </CardContent>
          )}
        </Card>

        <div>
          <h2 className="text-lg font-semibold">Service Entries</h2>
          <p className="text-muted-foreground text-sm">
            Entries in this service&apos;s DID document that define access to
            XRPC endpoints.
          </p>
        </div>

        <div className="overflow-clip rounded-lg border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Fragment ID</TableHead>
                <TableHead>Type</TableHead>
                <TableHead>XRPC Access</TableHead>
                <TableHead className="w-10 sticky right-0 bg-inherit z-[1]" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {entries.length === 0 && (
                <TableRow>
                  <TableCell
                    colSpan={4}
                    className="text-muted-foreground text-center"
                  >
                    No service entries yet.
                  </TableCell>
                </TableRow>
              )}
              {entries.map((entry) => (
                <TableRow key={entry.id}>
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
                  <TableCell className="w-10 sticky right-0 bg-inherit z-[1]">
                    {canManage && (
                      <Button
                        variant="ghost"
                        size="icon"
                        onClick={() => handleDelete(entry.id)}
                        aria-label={`Delete ${entry.fragment_id}`}
                      >
                        <Trash2 className="size-4" />
                      </Button>
                    )}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>

        {canManage &&
          identity &&
          (identity.mode === "did_plc" || identity.mode === "attach_account") && (
            <Card>
              <CardHeader>
                <CardTitle>Sync to PLC Directory</CardTitle>
                <CardDescription>
                  After adding or removing service entries, sync to update your
                  DID document in the PLC directory.
                </CardDescription>
              </CardHeader>
              <CardContent className="flex flex-col gap-4">
                {syncError && (
                  <p className="text-destructive text-sm">{syncError}</p>
                )}
                {syncSuccess && (
                  <p className="text-sm text-green-600 dark:text-green-400">
                    {syncSuccess}
                  </p>
                )}

                {identity.mode === "did_plc" && (
                  <div className="flex justify-end">
                    <Button onClick={handleSyncPlc} disabled={syncing}>
                      {syncing ? "Syncing..." : "Sync Now"}
                    </Button>
                  </div>
                )}

                {identity.mode === "attach_account" && !codeRequested && (
                  <div className="flex flex-col gap-2">
                    <p className="text-sm text-muted-foreground">
                      A confirmation code will be sent to the attached
                      account&apos;s email address.
                    </p>
                    <div className="flex justify-end">
                      <Button
                        onClick={handleSyncPlcRequest}
                        disabled={requestingCode}
                      >
                        {requestingCode ? "Sending..." : "Request Code"}
                      </Button>
                    </div>
                  </div>
                )}

                {identity.mode === "attach_account" && codeRequested && (
                  <div className="flex flex-col gap-3">
                    <div className="flex flex-col gap-2">
                      <Label htmlFor="plc_token">Confirmation Code</Label>
                      <Input
                        id="plc_token"
                        value={plcToken}
                        onChange={(e) => setPlcToken(e.target.value)}
                        placeholder="Enter the code from your email"
                      />
                    </div>
                    <div className="flex justify-end gap-2">
                      <Button
                        variant="outline"
                        onClick={() => {
                          setCodeRequested(false);
                          setPlcToken("");
                          setSyncError(null);
                          setSyncSuccess(null);
                        }}
                      >
                        Cancel
                      </Button>
                      <Button
                        onClick={handleSyncPlcSubmit}
                        disabled={submittingToken || !plcToken.trim()}
                      >
                        {submittingToken ? "Submitting..." : "Submit"}
                      </Button>
                    </div>
                  </div>
                )}
              </CardContent>
            </Card>
          )}

        {canManage && (
          <div className="flex flex-col gap-4">
            <h3 className="text-base font-semibold">Add Service Entry</h3>
            <div className="flex flex-col gap-2">
              <Label htmlFor="fragment_id">Fragment ID</Label>
              <Input
                id="fragment_id"
                value={fragmentId}
                onChange={(e) => setFragmentId(e.target.value)}
                placeholder="#atproto_pds"
              />
              <p className="text-muted-foreground text-xs">
                The fragment identifier (e.g. <code>#atproto_pds</code>). A{" "}
                <code>#</code> will be prepended automatically if omitted.
              </p>
            </div>
            <div className="flex flex-col gap-2">
              <Label htmlFor="service_type">Service Type</Label>
              <Input
                id="service_type"
                value={serviceType}
                onChange={(e) => setServiceType(e.target.value)}
                placeholder="AtprotoPersonalDataServer"
              />
            </div>
            <div className="flex justify-end">
              <Button
                onClick={handleAdd}
                disabled={adding || !fragmentId.trim() || !serviceType.trim()}
              >
                {adding ? "Adding..." : "Add"}
              </Button>
            </div>
          </div>
        )}
      </div>

      {selectedEntry && (
        <ServiceEntrySheet
          entry={selectedEntry}
          open={sheetOpen}
          onOpenChange={setSheetOpen}
          onSaved={load}
        />
      )}
    </>
  );
}
