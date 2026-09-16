"use client";

import { useState } from "react";
import { toast } from "sonner";
import { Check, Copy, Loader2 } from "lucide-react";

import {
  authorizeLinkedRepo,
  createLinkedRepo,
  inviteLinkedRepo,
} from "@/lib/api";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import {
  ScopeBuilder,
  grantsNoAccess,
  validateScopeString,
} from "@/components/linked-repos/scope-builder";

interface LinkRepoDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCreated: () => void;
}

type SubmitAction = "authorize" | "invite";

interface InviteResult {
  invite_url: string;
  expires_at: string;
}

// Replaces the Task 10 placeholder with the real create flow: handle/reason
// fields, the ScopeBuilder, and two submit paths (immediate OAuth vs. a
// single-use invite link).
export function LinkRepoDialog({
  open,
  onOpenChange,
  onCreated,
}: LinkRepoDialogProps) {
  const [formKey, setFormKey] = useState(0);
  const [handle, setHandle] = useState("");
  const [reason, setReason] = useState("");
  const [scopes, setScopes] = useState("");
  const [repoId, setRepoId] = useState<string | null>(null);
  const [inviteResult, setInviteResult] = useState<InviteResult | null>(null);
  const [submitting, setSubmitting] = useState<SubmitAction | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [lastAction, setLastAction] = useState<SubmitAction | null>(null);
  const [copied, setCopied] = useState(false);

  // Builder-level problems the scope string can't express on its own: a `repo`
  // row with no actions checked serialises to `repo:<nsid>`, which is valid and
  // grants *everything*. The builder reports it here rather than the string
  // being re-read, since only the builder knows nothing was picked.
  const [scopeSelectionError, setScopeSelectionError] = useState<string | null>(
    null,
  );

  const handleTrimmed = handle.trim();
  const scopeError = validateScopeString(scopes) ?? scopeSelectionError;
  // The builder always emits `atproto`, so a non-empty scope string no longer
  // means the operator picked anything — keep a grant that can do nothing from
  // being created by merely opening this dialog. Once the grant exists its
  // scopes are immutable, so this only gates creation, never a retry.
  const noAccess = repoId === null && grantsNoAccess(scopes);
  const canSubmit = submitting === null && scopeError === null && !noAccess;

  function handleOpenChange(nextOpen: boolean) {
    onOpenChange(nextOpen);
    if (!nextOpen) {
      // Force a fresh ScopeBuilder (and form state) next time the dialog
      // opens, rather than trying to thread a reset through its props.
      setFormKey((k) => k + 1);
      setHandle("");
      setReason("");
      setScopes("");
      setScopeSelectionError(null);
      setRepoId(null);
      setInviteResult(null);
      setSubmitting(null);
      setError(null);
      setLastAction(null);
      setCopied(false);
    }
  }

  // Creation only ever happens once per dialog session — repoId is cached so
  // a failed authorize/invite call (e.g. the auth server hasn't picked up
  // freshly-created client metadata yet) can be retried without creating a
  // second grant.
  async function submit(action: SubmitAction) {
    setSubmitting(action);
    setError(null);
    setLastAction(action);
    try {
      let id = repoId;
      if (id === null) {
        const created = await createLinkedRepo({
          handle: handleTrimmed || undefined,
          reason: reason.trim() || undefined,
          scopes,
        });
        id = created.id;
        setRepoId(id);
        onCreated();
      }
      if (action === "authorize") {
        const { authorize_url } = await authorizeLinkedRepo(id);
        window.location.assign(authorize_url);
        return;
      }
      const result = await inviteLinkedRepo(id);
      setInviteResult(result);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSubmitting(null);
    }
  }

  async function copyInvite() {
    if (!inviteResult) return;
    await navigator.clipboard.writeText(inviteResult.invite_url);
    setCopied(true);
    toast.success("Invite link copied. It works once and then expires.");
    setTimeout(() => setCopied(false), 2000);
  }

  return (
    <Sheet open={open} onOpenChange={handleOpenChange}>
      <SheetContent className="sm:max-w-xl flex flex-col overflow-hidden">
        <SheetHeader>
          <SheetTitle>
            {inviteResult ? "Invite link created" : "Link a repo"}
          </SheetTitle>
          <SheetDescription>
            {inviteResult
              ? "Share this with whoever should authorize it. It works once and then expires."
              : "Grant this AppView durable OAuth access to a repo, scoped to exactly what it needs."}
          </SheetDescription>
        </SheetHeader>

        <div className="flex flex-1 flex-col gap-4 overflow-y-auto">

        {inviteResult ? (
          <div className="flex flex-col gap-2 px-4">
            <Label>Invite link</Label>
            <div className="flex gap-2">
              <Input
                readOnly
                value={inviteResult.invite_url}
                className="font-mono text-xs"
              />
              <Button
                variant="outline"
                size="icon"
                onClick={copyInvite}
                title="Copy to clipboard"
              >
                {copied ? (
                  <Check className="size-4" />
                ) : (
                  <Copy className="size-4" />
                )}
              </Button>
            </div>
            <p className="text-muted-foreground text-xs">
              Expires {new Date(inviteResult.expires_at).toLocaleString()}.
            </p>
          </div>
        ) : (
          <div className="flex flex-col gap-4 px-4">
            {repoId !== null ? (
              <div className="rounded-lg border p-3 bg-muted/30 flex flex-col gap-1">
                <p className="text-sm">
                  This grant was already created
                  {handleTrimmed ? ` for ${handleTrimmed}` : ""}. Scopes are
                  immutable once created, so retry the action below rather
                  than editing them here.
                </p>
                <p className="font-mono text-xs text-muted-foreground break-all">
                  {scopes}
                </p>
              </div>
            ) : (
              <>
                <div className="flex flex-col gap-2">
                  <Label htmlFor="linked-repo-handle">
                    Handle or DID (optional)
                  </Label>
                  <Input
                    id="linked-repo-handle"
                    value={handle}
                    onChange={(e) => setHandle(e.target.value)}
                    placeholder="alice.bsky.social or did:plc:..."
                  />
                  <p className="text-muted-foreground text-xs">
                    Leave this blank to create an open link whose repo is
                    decided by whoever completes it. Fill it in to pin the
                    grant to this account, so only they can complete it.
                  </p>
                </div>

                <div className="flex flex-col gap-2">
                  <Label htmlFor="linked-repo-reason">Reason (optional)</Label>
                  <Textarea
                    id="linked-repo-reason"
                    value={reason}
                    onChange={(e) => setReason(e.target.value)}
                    placeholder="e.g. So the digest bot can post a weekly summary to your repo."
                    rows={3}
                  />
                  <p className="text-muted-foreground text-xs">
                    Shown to whoever opens the invite link, so write it for
                    them — it&apos;s how they decide whether to grant access.
                  </p>
                </div>

                <ScopeBuilder
                  key={formKey}
                  value={scopes}
                  onChange={setScopes}
                  onValidityChange={setScopeSelectionError}
                />
              </>
            )}

            <p className="text-muted-foreground text-xs">
              {handleTrimmed
                ? "Authorize now sends you through OAuth immediately. Create link generates a single-use invite instead."
                : "Authorize now is disabled without a handle or DID above — there's no identity to authorize against yet. Create link generates a single-use invite for whoever completes it."}
            </p>

            {error && (
              <div className="rounded-lg border border-destructive/50 bg-destructive/10 p-3 text-sm text-destructive flex flex-col gap-2">
                <span>{error}</span>
                {lastAction && (
                  <Button
                    size="sm"
                    variant="outline"
                    className="self-start"
                    onClick={() => submit(lastAction)}
                    disabled={submitting !== null}
                  >
                    Retry
                  </Button>
                )}
              </div>
            )}
          </div>
        )}

        </div>

        <SheetFooter>
          {inviteResult ? (
            <Button onClick={() => handleOpenChange(false)}>Done</Button>
          ) : (
            <>
              <Button
                variant="outline"
                onClick={() => handleOpenChange(false)}
                disabled={submitting !== null}
              >
                Cancel
              </Button>
              <Button
                variant="outline"
                onClick={() => submit("authorize")}
                disabled={!canSubmit || !handleTrimmed}
                title={
                  !handleTrimmed
                    ? "Add a handle or DID to authorize immediately, or use Create link instead"
                    : undefined
                }
              >
                {submitting === "authorize" && (
                  <Loader2 className="mr-2 size-4 animate-spin" />
                )}
                Authorize now
              </Button>
              <Button onClick={() => submit("invite")} disabled={!canSubmit}>
                {submitting === "invite" && (
                  <Loader2 className="mr-2 size-4 animate-spin" />
                )}
                Create link
              </Button>
            </>
          )}
        </SheetFooter>
      </SheetContent>
    </Sheet>
  );
}
