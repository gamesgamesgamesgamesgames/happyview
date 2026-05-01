"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import { usePathname, useRouter } from "next/navigation";

import { useCurrentUser } from "@/hooks/use-current-user";
import { deleteScript, getScript, patchScript } from "@/lib/api";
import type { Script, TriggerFamily } from "@/types/scripts";
import {
  TRIGGER_KIND_LABELS,
  familyOf,
  parseTriggerId,
} from "@/types/scripts";
import { SiteHeader } from "@/components/site-header";
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

  async function handleSave() {
    if (!state || !script) return;
    setSaving(true);
    setError(null);
    try {
      // PATCH only the editable fields. Trigger id is the PK — to
      // rename, delete and recreate.
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
  }

  async function handleDelete() {
    if (!script) return;
    if (!confirm(`Delete script '${script.id}'?`)) return;
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
            <Button
              variant="destructive"
              onClick={handleDelete}
              disabled={deleting}
            >
              {deleting ? "Deleting..." : "Delete script"}
            </Button>
          )}
          <div className="flex gap-2">
            {canManage && (
              <Button onClick={handleSave} disabled={!isDirty || saving}>
                {saving ? "Saving..." : "Save"}
              </Button>
            )}
          </div>
        </footer>
      </div>
    </>
  );
}
