"use client";

import { useCallback, useEffect, useState } from "react";
import { RefreshCw, ShieldAlert } from "lucide-react";
import { toast } from "sonner";

import { useCurrentUser } from "@/hooks/use-current-user";
import { toastError } from "@/lib/format";
import {
  ApiError,
  listInstanceOauthKeys,
  revokeInstanceOauthKey,
  rotateInstanceOauthKey,
} from "@/lib/api";
import type { InstanceOauthKey, KeyRotationResult } from "@/lib/api";
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
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";

export default function OauthKeysPage() {
  const { hasPermission } = useCurrentUser();
  const canManage = hasPermission("settings:manage");

  const [rotating, setRotating] = useState(false);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [lastRotation, setLastRotation] = useState<KeyRotationResult | null>(
    null,
  );

  const [keys, setKeys] = useState<InstanceOauthKey[]>([]);
  const [keysLoading, setKeysLoading] = useState(true);
  const [keysError, setKeysError] = useState<string | null>(null);
  const [revokeTarget, setRevokeTarget] = useState<InstanceOauthKey | null>(
    null,
  );
  const [revoking, setRevoking] = useState(false);

  const loadKeys = useCallback(() => {
    setKeysLoading(true);
    setKeysError(null);
    listInstanceOauthKeys()
      .then((res) => {
        setKeys(res.keys);
        setKeysLoading(false);
      })
      .catch((e: unknown) => {
        setKeysError(
          e instanceof ApiError && e.status === 403
            ? "You do not have permission to view instance signing keys."
            : "Could not load instance signing keys.",
        );
        setKeysLoading(false);
        toastError("Failed to load instance OAuth keys", e);
      });
  }, []);

  useEffect(() => {
    loadKeys();
  }, [loadKeys]);

  async function handleRotate(immediate: boolean) {
    setRotating(true);
    try {
      const result = await rotateInstanceOauthKey(immediate);
      setLastRotation(result);
      setConfirmOpen(false);
      const title =
        result.status === "pending"
          ? `Published a new instance signing key; it takes over ${formatActivation(result.activates_at)}`
          : "Switched to a new instance signing key";
      if (result.orphaned_sessions > 0) {
        toast.success(title, {
          description: `${result.orphaned_sessions} session${result.orphaned_sessions === 1 ? "" : "s"} predate key pinning and cannot be protected by this or any future rotation.`,
        });
      } else {
        toast.success(title);
      }
      loadKeys();
    } catch (e: unknown) {
      toastError("Failed to rotate the instance signing key", e);
    } finally {
      setRotating(false);
    }
  }

  async function handleRevoke() {
    if (!revokeTarget) return;
    setRevoking(true);
    try {
      const result = await revokeInstanceOauthKey(revokeTarget.kid);
      setRevokeTarget(null);
      toast.success("Revoked instance signing key", {
        description:
          result.sessions_destroyed > 0
            ? `${result.sessions_destroyed} session${result.sessions_destroyed === 1 ? "" : "s"} pinned to this key ${result.sessions_destroyed === 1 ? "was" : "were"} destroyed.`
            : "No sessions were pinned to this key.",
      });
      loadKeys();
    } catch (e: unknown) {
      toastError("Failed to revoke the instance signing key", e);
    } finally {
      setRevoking(false);
    }
  }

  function statusBadge(status: InstanceOauthKey["status"]) {
    switch (status) {
      case "pending":
        return <Badge variant="secondary">Pending</Badge>;
      case "current":
        return <Badge>Current</Badge>;
      case "retiring":
        return <Badge variant="secondary">Retiring</Badge>;
      case "revoked":
        return <Badge variant="outline">Revoked</Badge>;
    }
  }

  return (
    <>
      <SiteHeader title="OAuth Keys" />
      <div className="flex flex-1 flex-col gap-4 p-4 md:p-6 max-w-3xl">
        <div>
          <h2 className="text-lg font-semibold">Instance OAuth Signing Key</h2>
          <p className="text-muted-foreground text-sm">
            The key HappyView signs dashboard logins and token refreshes with,
            as a confidential OAuth client. Every session is pinned to the exact
            key that established it — a refresh is only ever attempted with that
            same key, never with whatever key happens to be current later.
          </p>
        </div>

        <Card>
          <CardHeader>
            <CardTitle>Generate a new key</CardTitle>
            <CardDescription>
              This costs nothing. The new key is published straight away and
              starts signing new sessions about 75 minutes later, once every
              authorization server&rsquo;s cached copy of this instance&rsquo;s
              keys includes it. The current key keeps signing until then, and
              keeps signing refreshes for every session already established with
              it — nothing already logged in is affected.
            </CardDescription>
          </CardHeader>
          <CardContent className="flex flex-col gap-4">
            <div>
              <AlertDialog open={confirmOpen} onOpenChange={setConfirmOpen}>
                <AlertDialogTrigger asChild>
                  <Button disabled={!canManage || rotating} className="w-fit">
                    <RefreshCw
                      className={`size-4 ${rotating ? "animate-spin" : ""}`}
                    />
                    Generate new key
                  </Button>
                </AlertDialogTrigger>
                <AlertDialogContent>
                  <AlertDialogHeader>
                    <AlertDialogTitle>
                      Generate a new instance signing key?
                    </AlertDialogTitle>
                    <AlertDialogDescription>
                      The new key is published now and takes over in about 75
                      minutes. Then the current key becomes retiring: it keeps
                      signing refreshes for every session already established
                      with it, but no new session will use it. This does not log
                      anyone out.
                    </AlertDialogDescription>
                    <AlertDialogDescription>
                      If the current key has leaked, switch now instead.
                      Sign-ins through a PDS that cached the old keys can fail
                      for up to an hour.
                    </AlertDialogDescription>
                  </AlertDialogHeader>
                  <AlertDialogFooter>
                    <AlertDialogCancel disabled={rotating}>
                      Cancel
                    </AlertDialogCancel>
                    <Button
                      variant="outline"
                      disabled={rotating}
                      onClick={() => handleRotate(true)}
                    >
                      Switch now
                    </Button>
                    <AlertDialogAction
                      disabled={rotating}
                      onClick={() => handleRotate(false)}
                    >
                      {rotating ? "Generating..." : "Generate new key"}
                    </AlertDialogAction>
                  </AlertDialogFooter>
                </AlertDialogContent>
              </AlertDialog>
            </div>

            {lastRotation && (
              <div className="flex flex-col gap-1.5 rounded-lg border p-3">
                <div className="flex items-center gap-2">
                  {statusBadge(lastRotation.status)}
                  <span className="font-mono text-sm">{lastRotation.kid}</span>
                </div>
                {lastRotation.status === "pending" && (
                  <p className="text-xs text-muted-foreground">
                    Takes over {formatActivation(lastRotation.activates_at)}.
                  </p>
                )}
                {lastRotation.orphaned_sessions > 0 && (
                  <p className="text-xs text-muted-foreground">
                    {lastRotation.orphaned_sessions} session
                    {lastRotation.orphaned_sessions === 1 ? "" : "s"} predate
                    key pinning (no recorded signing key) and cannot be
                    protected by this rotation.
                  </p>
                )}
              </div>
            )}

            {!canManage && (
              <p className="text-muted-foreground text-xs">
                Requires the{" "}
                <code className="bg-muted px-1 rounded">settings:manage</code>{" "}
                permission.
              </p>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Retiring and revoked keys</CardTitle>
            <CardDescription>
              To contain a leaked key: generate a new key and choose{" "}
              <em>Switch now</em> (the leaked one becomes retiring, so the
              instance keeps working), then revoke the retiring key below.
              Revoking is immediate and destroys every session pinned to that
              key — it is the correct response to a leak, not routine cleanup.
            </CardDescription>
          </CardHeader>
          <CardContent className="flex flex-col gap-3">
            {keysLoading && (
              <p className="text-sm text-muted-foreground">Loading keys...</p>
            )}

            {!keysLoading && keysError && (
              <div className="flex items-center justify-between gap-3">
                <p className="text-destructive text-sm">{keysError}</p>
                <Button variant="outline" size="sm" onClick={loadKeys}>
                  Retry
                </Button>
              </div>
            )}

            {!keysLoading && !keysError && keys.length === 0 && (
              <p className="text-sm text-muted-foreground">
                No instance signing keys yet.
              </p>
            )}

            {!keysLoading &&
              !keysError &&
              keys.map((key) => (
                <div
                  key={key.kid}
                  className="flex flex-col gap-1.5 rounded-lg border p-3 sm:flex-row sm:items-center sm:justify-between"
                >
                  <div className="flex flex-col gap-1.5">
                    <div className="flex items-center gap-2">
                      {statusBadge(key.status)}
                      <span className="font-mono text-sm">{key.kid}</span>
                    </div>
                    <p className="text-xs text-muted-foreground">
                      Created {new Date(key.created_at).toLocaleString()} ·{" "}
                      {key.status === "pending" ? (
                        <>Takes over {formatActivation(key.activates_at)}</>
                      ) : key.status === "revoked" ? (
                        <>
                          {key.session_count} session
                          {key.session_count === 1 ? "" : "s"} destroyed when
                          this key was revoked
                        </>
                      ) : (
                        <>
                          {key.session_count} live session
                          {key.session_count === 1 ? "" : "s"}
                        </>
                      )}
                    </p>
                  </div>

                  {key.status === "retiring" && (
                    <AlertDialog
                      open={revokeTarget?.kid === key.kid}
                      onOpenChange={(o) => !o && setRevokeTarget(null)}
                    >
                      <AlertDialogTrigger asChild>
                        <Button
                          variant="destructive"
                          size="sm"
                          disabled={!canManage}
                          onClick={() => setRevokeTarget(key)}
                          className="w-fit"
                        >
                          <ShieldAlert className="size-4" />
                          Revoke now
                        </Button>
                      </AlertDialogTrigger>
                      <AlertDialogContent>
                        <AlertDialogHeader>
                          <AlertDialogTitle>
                            Revoke this key immediately?
                          </AlertDialogTitle>
                          <AlertDialogDescription asChild>
                            <div className="flex flex-col gap-2 text-sm text-muted-foreground">
                              <p>
                                This is the response to a leaked or compromised
                                key, not routine cleanup. Revoking removes{" "}
                                <span className="font-mono">{key.kid}</span>{" "}
                                from the published JWKS immediately.
                              </p>
                              <p className="font-medium text-foreground">
                                {key.session_count > 0
                                  ? `${key.session_count} live session${key.session_count === 1 ? "" : "s"} pinned to this key will be destroyed and their users signed out.`
                                  : "No live sessions are pinned to this key, so revoking it is free."}
                              </p>
                              <p>This cannot be undone.</p>
                            </div>
                          </AlertDialogDescription>
                        </AlertDialogHeader>
                        <AlertDialogFooter>
                          <AlertDialogCancel disabled={revoking}>
                            Cancel
                          </AlertDialogCancel>
                          <AlertDialogAction
                            variant="destructive"
                            disabled={revoking}
                            onClick={handleRevoke}
                          >
                            {revoking ? "Revoking..." : "Revoke now"}
                          </AlertDialogAction>
                        </AlertDialogFooter>
                      </AlertDialogContent>
                    </AlertDialog>
                  )}
                </div>
              ))}

            {!canManage && (
              <p className="text-muted-foreground text-xs">
                Requires the{" "}
                <code className="bg-muted px-1 rounded">settings:manage</code>{" "}
                permission.
              </p>
            )}
          </CardContent>
        </Card>
      </div>
    </>
  );
}

/** "at 14:05:00", or "shortly" when the server gave no time. */
function formatActivation(at: string | null): string {
  return at ? `at ${new Date(at).toLocaleString()}` : "shortly";
}
