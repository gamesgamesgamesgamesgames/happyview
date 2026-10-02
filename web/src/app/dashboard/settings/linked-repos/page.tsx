"use client";

import { useCallback, useEffect, useState } from "react";
import { toast } from "sonner";
import { Loader2 } from "lucide-react";

import { useCurrentUser } from "@/hooks/use-current-user";
import { toastError } from "@/lib/format";
import { deleteLinkedRepo, getLinkedRepoInvites, getLinkedRepos } from "@/lib/api";
import type { LinkedRepo } from "@/types/linked-repos";
import {
  LinkedRepoDetail,
  relativeTime,
  statusBadge,
} from "@/components/linked-repos/linked-repo-detail";
import { LinkRepoDialog } from "@/components/linked-repos/link-repo-dialog";
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
} from "@/components/ui/alert-dialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Sheet,
  SheetContent,
  SheetDescription,
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

export default function LinkedReposPage() {
  const { hasPermission } = useCurrentUser();
  const [repos, setRepos] = useState<LinkedRepo[]>([]);
  const [loading, setLoading] = useState(true);
  const [dialogOpen, setDialogOpen] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [pendingDelete, setPendingDelete] = useState<LinkedRepo | null>(null);
  const [inviteCounts, setInviteCounts] = useState<Record<string, number>>({});

  const load = useCallback(() => {
    getLinkedRepos()
      .then((resp) => {
        setRepos(resp.linked_repos);
        setLoading(false);
      })
      .catch((e) => {
        toastError("Failed to load linked repos", e);
        setLoading(false);
      });
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const canCreate = hasPermission("linked-repos:create");
  const canDelete = hasPermission("linked-repos:delete");
  const canViewInvites = hasPermission("linked-repos:read");
  const columnCount = canViewInvites ? 6 : 5;

  // Counts only. The table says how many links are live; managing them belongs
  // to the detail sheet, so nothing in this column is interactive.
  useEffect(() => {
    if (!canViewInvites || repos.length === 0) return;
    let cancelled = false;
    Promise.all(
      repos.map((repo) =>
        getLinkedRepoInvites(repo.id)
          .then((resp) => [repo.id, resp.invites.length] as const)
          .catch(() => [repo.id, 0] as const),
      ),
    ).then((entries) => {
      if (!cancelled) setInviteCounts(Object.fromEntries(entries));
    });
    return () => {
      cancelled = true;
    };
  }, [repos, canViewInvites]);

  const selected = repos.find((r) => r.id === selectedId) ?? null;

  async function confirmDelete() {
    if (!pendingDelete) return;
    try {
      await deleteLinkedRepo(pendingDelete.id);
      toast.success("Linked repo removed");
      setPendingDelete(null);
      setSelectedId(null);
      load();
    } catch (e) {
      toastError("Failed to remove linked repo", e);
    }
  }

  function inviteSummary(repo: LinkedRepo): string {
    const n = inviteCounts[repo.id] ?? 0;
    if (n === 0) return "—";
    return n === 1 ? "1 link outstanding" : `${n} links outstanding`;
  }

  return (
    <>
      <SiteHeader title="Linked Repos" />
      <div className="flex flex-1 flex-col gap-4 p-4 md:p-6">
        <div className="flex items-center justify-between">
          <h2 className="text-lg font-semibold">Linked Repos</h2>
          {canCreate && (
            <Button onClick={() => setDialogOpen(true)}>Link a repo</Button>
          )}
        </div>

        <div className="rounded-lg border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Repo</TableHead>
                <TableHead>Reason</TableHead>
                <TableHead>Scopes</TableHead>
                <TableHead>Status</TableHead>
                {canViewInvites && <TableHead>Invite links</TableHead>}
                <TableHead>Last refreshed</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {loading && repos.length === 0 && (
                <TableRow>
                  <TableCell
                    colSpan={columnCount}
                    className="text-muted-foreground text-center py-8"
                  >
                    <Loader2 className="size-4 animate-spin mx-auto" />
                  </TableCell>
                </TableRow>
              )}
              {!loading && repos.length === 0 && (
                <TableRow>
                  <TableCell
                    colSpan={columnCount}
                    className="text-muted-foreground text-center py-8"
                  >
                    No linked repos yet. Linking a repo lets this AppView write
                    to it from scripts, long after whoever authorized it has
                    gone.
                  </TableCell>
                </TableRow>
              )}
              {repos.map((repo) => (
                <TableRow
                  key={repo.id}
                  className="cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                  tabIndex={0}
                  role="button"
                  onClick={() => setSelectedId(repo.id)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      setSelectedId(repo.id);
                    }
                  }}
                >
                  <TableCell className="font-mono text-sm">
                    {repo.handle ?? repo.did ?? "—"}
                  </TableCell>
                  <TableCell className="max-w-[240px] truncate">
                    {repo.reason ?? "—"}
                  </TableCell>
                  <TableCell>
                    <div className="flex flex-wrap gap-1">
                      {repo.scopes
                        .split(/\s+/)
                        .filter(Boolean)
                        .map((scope) => (
                          <Badge
                            key={scope}
                            variant="outline"
                            className="font-mono text-xs"
                          >
                            {scope}
                          </Badge>
                        ))}
                    </div>
                  </TableCell>
                  <TableCell>{statusBadge(repo)}</TableCell>
                  {canViewInvites && (
                    <TableCell className="text-muted-foreground text-sm">
                      {inviteSummary(repo)}
                    </TableCell>
                  )}
                  <TableCell className="text-muted-foreground text-sm">
                    {repo.last_refreshed_at
                      ? relativeTime(repo.last_refreshed_at)
                      : "—"}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      </div>

      <Sheet
        open={selected != null}
        onOpenChange={(open) => {
          if (!open) setSelectedId(null);
        }}
      >
        <SheetContent className="sm:max-w-xl flex flex-col overflow-hidden">
          {selected && (
            <>
              <SheetHeader>
                <SheetTitle className="font-mono text-base break-all">
                  {selected.handle ?? selected.did ?? "Unauthorized grant"}
                </SheetTitle>
                <SheetDescription>
                  {selected.handle
                    ? "Linked repo"
                    : selected.did
                      ? "Linked repo, pinned by DID"
                      : "Open grant — the repo is decided by whoever completes an invite link"}
                </SheetDescription>
              </SheetHeader>
              <LinkedRepoDetail
                repo={selected}
                canCreate={canCreate}
                canDelete={canDelete}
                canViewInvites={canViewInvites}
                onRequestDelete={() => setPendingDelete(selected)}
                onChanged={load}
              />
            </>
          )}
        </SheetContent>
      </Sheet>

      <LinkRepoDialog
        open={dialogOpen}
        onOpenChange={setDialogOpen}
        onCreated={load}
      />

      <AlertDialog
        open={pendingDelete !== null}
        onOpenChange={(open) => !open && setPendingDelete(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Remove this linked repo?</AlertDialogTitle>
            <AlertDialogDescription>
              This deletes the stored session as well. Any script writing to{" "}
              {pendingDelete?.handle ?? pendingDelete?.did ?? "this repo"} will
              start failing. Scopes are immutable, so restoring access means
              linking it again from scratch.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction variant="destructive" onClick={confirmDelete}>
              Remove
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  );
}
