"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import { usePathname, useRouter } from "next/navigation";
import { AlertTriangle } from "lucide-react";

import { useCurrentUser } from "@/hooks/use-current-user";
import { deleteScript, getScript, patchScript } from "@/lib/api";
import type { Script, TriggerFamily } from "@/types/scripts";
import {
  TRIGGER_KIND_LABELS,
  familyOf,
  parseTriggerId,
} from "@/types/scripts";
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
  ScriptForm,
  type ScriptFormState,
  stateFromScript,
} from "../script-form";

export default function ScriptDetail() {
  const pathname = usePathname();
  // The `[id]` route segment carries the URL-encoded trigger id.
  // Decode once so all downstream calls see the canonical id (which
  // contains `:` and `.`).
  const id = decodeURIComponent(
    pathname.split("/").filter(Boolean).pop() ?? "",
  );
  const { hasPermission } = useCurrentUser();
  const router = useRouter();
  const [script, setScript] = useState<Script | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [deleting, setDeleting] = useState(false);

  const [state, setState] = useState<ScriptFormState | null>(null);
  const [original, setOriginal] = useState<ScriptFormState | null>(null);

  const load = useCallback(() => {
    getScript(id)
      .then((s) => {
        setScript(s);
        const next = stateFromScript({
          id: s.id,
          description: s.description,
          body: s.body,
        });
        setState(next);
        setOriginal(next);
      })
      .catch((e) => setError(e instanceof Error ? e.message : String(e)));
  }, [id]);

  useEffect(() => {
    load();
  }, [load]);

  const isDirty = useMemo(() => {
    if (!state || !original) return false;
    return (
      state.body !== original.body ||
      state.description !== original.description
    );
  }, [state, original]);

  useEffect(() => {
    if (!isDirty) return;
    function onBeforeUnload(e: BeforeUnloadEvent) {
      e.preventDefault();
      e.returnValue = "";
    }
    window.addEventListener("beforeunload", onBeforeUnload);
    return () => window.removeEventListener("beforeunload", onBeforeUnload);
  }, [isDirty]);

  const handleSave = useCallback(async () => {
    if (!state || !script || !isDirty || saving) return;
    setSaving(true);
    setError(null);
    try {
      await patchScript(script.id, {
        body: state.body,
        description: state.description.trim() || null,
      });
      load();
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  }, [state, script, isDirty, saving, load]);

  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
        e.preventDefault();
        handleSave();
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [handleSave]);

  async function handleDelete() {
    if (!script) return;
    setDeleting(true);
    try {
      await deleteScript(script.id);
      router.push("/dashboard/settings/scripts");
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
      setDeleting(false);
    }
  }

  if (error && !state) {
    return (
      <>
        <SiteHeader title="Script" backHref="/dashboard/settings/scripts" />
        <div className="p-4 md:p-6">
          <p className="text-destructive text-sm">{error}</p>
        </div>
      </>
    );
  }

  if (!state) {
    return (
      <>
        <SiteHeader title="Script" backHref="/dashboard/settings/scripts" />
        <div className="p-4 md:p-6">
          <p className="text-muted-foreground text-sm">Loading...</p>
        </div>
      </>
    );
  }

  const canManage = hasPermission("scripts:manage");
  const parsed = parseTriggerId(id);
  const familyLabel: Record<TriggerFamily, string> = {
    record: "Record event",
    xrpc: "XRPC handler",
    labeler: "Label arrival",
    job: "Job runner",
  };

  return (
    <>
      <SiteHeader title={`Script: ${id}`} backHref="/dashboard/settings/scripts" />

      <div className="flex flex-col flex-1 min-h-0">
        <div className="flex flex-col flex-1 min-h-0 gap-6 p-4 md:p-6">
          {error && <p className="text-destructive text-sm">{error}</p>}

          {parsed && (
            <div className="flex gap-2 items-baseline">
              <Badge variant="outline">{TRIGGER_KIND_LABELS[parsed.kind]}</Badge>
              <span className="text-muted-foreground text-xs">
                ({familyLabel[familyOf(parsed.kind)]})
              </span>
              {parsed.kind.startsWith("record.") &&
                parsed.kind !== "record.index" && (
                  <span className="text-muted-foreground text-xs">
                    — fires only on this action; cascades to{" "}
                    <span className="font-mono">
                      record.index:{parsed.suffix}
                    </span>{" "}
                    if absent.
                  </span>
                )}
              {parsed.kind === "record.index" && (
                <span className="text-muted-foreground text-xs">
                  — wildcard fallback; runs for any action without an
                  action-specific row.
                </span>
              )}
            </div>
          )}

          <ScriptForm state={state} onChange={setState} idLocked />
        </div>

        <footer className="bg-sidebar-accent flex justify-between gap-2 ps-4 pt-2 pb-1 md:px-6 md:py-4 rounded-b-md">
          {canManage && (
            <AlertDialog>
              <AlertDialogTrigger asChild>
                <Button variant="destructive" disabled={deleting}>
                  {deleting ? "Deleting..." : "Delete script"}
                </Button>
              </AlertDialogTrigger>
              <AlertDialogContent>
                <AlertDialogHeader>
                  <AlertDialogTitle>Delete script?</AlertDialogTitle>
                  <AlertDialogDescription asChild>
                    <div className="flex flex-col gap-3">
                      <p>
                        This will permanently remove the script. This action
                        cannot be undone.
                      </p>
                      {script?.recreatable === false && (
                        <div className="flex items-start gap-3 rounded-lg border border-amber-500/50 bg-amber-500/10 p-3">
                          <AlertTriangle className="size-4 text-amber-500 shrink-0 mt-0.5" />
                          <p className="text-xs text-amber-500">
                            This script&rsquo;s trigger id no longer satisfies
                            current NSID rules. It will keep firing and can
                            still be edited in place — but once deleted, it{" "}
                            <span className="font-medium">
                              cannot be recreated
                            </span>{" "}
                            with the same id. Deletion is not required to
                            change it.
                          </p>
                        </div>
                      )}
                    </div>
                  </AlertDialogDescription>
                </AlertDialogHeader>
                <AlertDialogFooter>
                  <AlertDialogCancel disabled={deleting}>
                    Cancel
                  </AlertDialogCancel>
                  <AlertDialogAction
                    variant="destructive"
                    disabled={deleting}
                    onClick={handleDelete}
                  >
                    {script?.recreatable === false
                      ? "Delete anyway"
                      : "Delete"}
                  </AlertDialogAction>
                </AlertDialogFooter>
              </AlertDialogContent>
            </AlertDialog>
          )}
          <div className="flex gap-2">
            {canManage && (
              <Button onClick={handleSave} disabled={!isDirty || saving}>
                {saving ? "Saving..." : "Save"}
                <kbd className="ml-2 text-xs text-muted-foreground opacity-60 hidden sm:inline">
                  ⌘↵
                </kbd>
              </Button>
            )}
          </div>
        </footer>
      </div>
    </>
  );
}
