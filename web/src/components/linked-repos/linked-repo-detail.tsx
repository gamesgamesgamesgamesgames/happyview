"use client";

import { useCallback, useEffect, useState } from "react";
import { toast } from "sonner";
import { Loader2, Trash2 } from "lucide-react";

import { toastError } from "@/lib/format";
import {
  authorizeLinkedRepo,
  getLinkedRepoInvites,
  inviteLinkedRepo,
  revokeLinkedRepoInvite,
  type LinkedRepoInvite,
} from "@/lib/api";
import type { LinkedRepo } from "@/types/linked-repos";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

/** Handles both past timestamps (last refreshed) and future ones (invite expiry). */
export function relativeTime(dateStr: string): string {
  const diffMs = Date.now() - new Date(dateStr).getTime();
  const past = diffMs >= 0;
  const seconds = Math.floor(Math.abs(diffMs) / 1000);
  if (seconds < 60) return past ? "just now" : "in under a minute";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return past ? `${minutes}m ago` : `in ${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return past ? `${hours}h ago` : `in ${hours}h`;
  const days = Math.floor(hours / 24);
  return past ? `${days}d ago` : `in ${days}d`;
}

export function statusBadge(repo: LinkedRepo) {
  switch (repo.status) {
    case "active":
      return (
        <Badge className="bg-emerald-500/15 text-emerald-700 dark:text-emerald-400 hover:bg-emerald-500/25 border-emerald-500/20">
          active
        </Badge>
      );
    case "needs_reauth":
      return (
        <Badge variant="destructive" title={repo.last_error ?? undefined}>
          needs reauth
        </Badge>
      );
    default:
      return <Badge variant="secondary">pending</Badge>;
  }
}

interface LinkedRepoDetailProps {
  repo: LinkedRepo;
  canCreate: boolean;
  canDelete: boolean;
  canViewInvites: boolean;
  onRequestDelete: () => void;
  /** Called after anything that changes server state, so the list can refresh. */
  onChanged: () => void;
}

export function LinkedRepoDetail({
  repo,
  canCreate,
  canDelete,
  canViewInvites,
  onRequestDelete,
  onChanged,
}: LinkedRepoDetailProps) {
  const [invites, setInvites] = useState<LinkedRepoInvite[] | null>(null);
  const [busy, setBusy] = useState(false);

  const loadInvites = useCallback(() => {
    if (!canViewInvites) return;
    getLinkedRepoInvites(repo.id)
      .then((resp) => setInvites(resp.invites))
      .catch((e) => {
        toastError("Failed to load invite links", e);
        setInvites([]);
      });
  }, [repo.id, canViewInvites]);

  useEffect(() => {
    loadInvites();
  }, [loadInvites]);

  async function createInvite() {
    setBusy(true);
    let inviteUrl: string;
    try {
      ({ invite_url: inviteUrl } = await inviteLinkedRepo(repo.id));
    } catch (e) {
      toastError("Failed to create invite link", e);
      setBusy(false);
      return;
    }

    // The link was minted server-side and can never be read back, so a
    // clipboard failure must not read as "the invite wasn't created" — that
    // would send the admin back to mint another and lose this one.
    try {
      await navigator.clipboard.writeText(inviteUrl);
      toast.success("Invite link copied. Send it to the repo's owner.");
    } catch {
      toast.warning("Invite link created, but copying to the clipboard failed", {
        description: inviteUrl,
        duration: Infinity,
        closeButton: true,
      });
    }
    setBusy(false);
    loadInvites();
    onChanged();
  }

  async function revoke(invite: LinkedRepoInvite) {
    try {
      await revokeLinkedRepoInvite(repo.id, invite.invite_id);
      toast.success("Invite link revoked");
      loadInvites();
      onChanged();
    } catch (e) {
      toastError("Failed to revoke invite link", e);
    }
  }

  async function authorize() {
    try {
      const { authorize_url } = await authorizeLinkedRepo(repo.id);
      window.location.assign(authorize_url);
    } catch (e) {
      toastError("Failed to start authorization", e);
    }
  }

  // The backend resolves the identity to authorize against as `handle ?? did`,
  // so a grant pinned by a bare DID is authorizable too. Only a truly open
  // grant — neither handle nor DID — has nothing to authorize against.
  const canAuthorizeInline = Boolean(repo.handle || repo.did);

  return (
    <div className="flex flex-1 flex-col gap-6 overflow-y-auto px-4 pb-4">
      <section className="flex flex-col gap-3">
        <div className="flex flex-col gap-1">
          <span className="text-muted-foreground text-xs">Status</span>
          <div className="flex items-center gap-2">
            {statusBadge(repo)}
            {repo.status === "active" && repo.last_refreshed_at && (
              <span className="text-muted-foreground text-xs">
                refreshed {relativeTime(repo.last_refreshed_at)}
              </span>
            )}
          </div>
          {repo.last_error && (
            <p className="text-destructive text-xs">{repo.last_error}</p>
          )}
        </div>

        {repo.did && (
          <div className="flex flex-col gap-1">
            <span className="text-muted-foreground text-xs">DID</span>
            <p className="font-mono text-sm break-all">{repo.did}</p>
          </div>
        )}

        {repo.reason && (
          <div className="flex flex-col gap-1">
            <span className="text-muted-foreground text-xs">Reason</span>
            <p className="text-sm whitespace-pre-line">{repo.reason}</p>
          </div>
        )}

        <div className="flex flex-col gap-1">
          <span className="text-muted-foreground text-xs">Scopes</span>
          <div className="flex flex-wrap gap-1">
            {repo.scopes
              .split(/\s+/)
              .filter(Boolean)
              .map((scope) => (
                <Badge key={scope} variant="outline" className="font-mono text-xs">
                  {scope}
                </Badge>
              ))}
          </div>
          <p className="text-muted-foreground text-xs">
            Scopes are fixed for the life of the grant. Changing them means
            removing this repo and linking it again.
          </p>
        </div>
      </section>

      {canViewInvites && (
        <section className="flex flex-col gap-3">
          <div className="flex items-center justify-between gap-4">
            <h3 className="text-sm font-medium">Invite links</h3>
            {canCreate && (
              <Button size="sm" onClick={createInvite} disabled={busy}>
                {busy && <Loader2 className="size-4 animate-spin" />}
                Create a new invite link
              </Button>
            )}
          </div>

          <p className="text-muted-foreground text-xs">
            Each link works once and is copied to your clipboard when created —
            it can&apos;t be shown again afterwards. Creating another leaves any
            existing links working, so revoke one you no longer want used.
          </p>

          {invites === null ? (
            <div className="text-muted-foreground flex items-center gap-2 text-sm">
              <Loader2 className="size-4 animate-spin" /> Loading…
            </div>
          ) : invites.length === 0 ? (
            <p className="text-muted-foreground rounded-md border px-3 py-4 text-center text-sm">
              No outstanding invite links.
            </p>
          ) : (
            <div className="rounded-lg border">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Link</TableHead>
                    <TableHead className="text-right">Status</TableHead>
                    {canCreate && <TableHead className="w-[52px]" />}
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {invites.map((invite) => (
                    <TableRow key={invite.invite_id}>
                      <TableCell
                        className="font-mono text-xs"
                        title={invite.invite_id}
                      >
                        {invite.invite_id.slice(0, 12)}…
                      </TableCell>
                      <TableCell className="text-muted-foreground text-right text-sm">
                        expires {relativeTime(invite.expires_at)}
                      </TableCell>
                      {canCreate && (
                        <TableCell>
                          <Button
                            variant="ghost"
                            size="icon"
                            aria-label="Revoke this invite link"
                            className="text-muted-foreground hover:text-destructive size-8"
                            onClick={() => revoke(invite)}
                          >
                            <Trash2 className="size-4" />
                          </Button>
                        </TableCell>
                      )}
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          )}
        </section>
      )}

      <section className="mt-auto flex flex-col gap-2 border-t pt-4">
        {canCreate && canAuthorizeInline && repo.status !== "active" && (
          <Button variant="outline" onClick={authorize}>
            {repo.status === "needs_reauth" ? "Re-authorize" : "Authorize now"}
          </Button>
        )}
        {canCreate && !canAuthorizeInline && repo.status !== "active" && (
          <p className="text-muted-foreground text-xs">
            This grant is open, so there&apos;s no account to authorize against
            here — send an invite link and let the owner choose.
          </p>
        )}
        {canDelete && (
          <Button variant="ghost" className="text-destructive" onClick={onRequestDelete}>
            Remove this linked repo
          </Button>
        )}
      </section>
    </div>
  );
}
