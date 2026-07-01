"use client";

import { Suspense, useCallback, useEffect, useMemo, useState } from "react";
import { useRouter, useSearchParams } from "next/navigation";

import { useCurrentUser } from "@/hooks/use-current-user";
import { getLexicons, upsertScript } from "@/lib/api";
import type { LexiconSummary } from "@/types/lexicons";
import type { TriggerKind } from "@/types/scripts";
import {
  DEFAULT_JOB_SCRIPT_BODY,
  DEFAULT_SCRIPT_BODY,
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
import { Button } from "@/components/ui/button";

import {
  JOB_SOURCE,
  ScriptForm,
  type ScriptFormState,
  composeTriggerId,
  isValidJobType,
} from "../script-form";

function NewScriptInner() {
  const { hasPermission } = useCurrentUser();
  const router = useRouter();
  const searchParams = useSearchParams();
  const [state, setState] = useState<ScriptFormState>(() =>
    initialState(searchParams),
  );
  // The Lexicon picker is sourced from /admin/lexicons; we render the
  // form even if the call fails (the operator can still pick "Actor"
  // and create a labeler.apply:_actor script).
  const [lexicons, setLexicons] = useState<LexiconSummary[]>([]);
  const [lexiconsLoading, setLexiconsLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // If the URL changes (e.g. user navigates with new ?id=...), refresh state.
  useEffect(() => {
    setState(initialState(searchParams));
  }, [searchParams]);

  useEffect(() => {
    getLexicons()
      .then(setLexicons)
      .catch(() => setLexicons([]))
      .finally(() => setLexiconsLoading(false));
  }, []);

  const isDirty = useMemo(() => {
    const defaultBody =
      state.source === JOB_SOURCE ? DEFAULT_JOB_SCRIPT_BODY : DEFAULT_SCRIPT_BODY;
    return (
      state.suffix !== "" ||
      state.description !== "" ||
      state.body !== defaultBody
    );
  }, [state]);

  useEffect(() => {
    if (!isDirty) return;
    function onBeforeUnload(e: BeforeUnloadEvent) {
      e.preventDefault();
    }
    window.addEventListener("beforeunload", onBeforeUnload);
    return () => window.removeEventListener("beforeunload", onBeforeUnload);
  }, [isDirty]);

  const canSave =
    !saving &&
    !!state.suffix &&
    !(state.source === JOB_SOURCE && !isValidJobType(state.suffix));

  const handleSave = useCallback(async () => {
    if (!canSave) return;
    setSaving(true);
    setError(null);
    try {
      const id = composeTriggerId(state);
      await upsertScript({
        id,
        body: state.body,
        description: state.description.trim() || null,
      });
      router.push(`/dashboard/settings/scripts/${encodeURIComponent(id)}`);
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
      setSaving(false);
    }
  }, [canSave, state, router]);

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

  if (!hasPermission("scripts:manage")) {
    return (
      <>
        <SiteHeader title="New script" backHref="/dashboard/settings/scripts" />
        <div className="p-4 md:p-6">
          <p className="text-destructive text-sm">
            You don&apos;t have permission to create scripts.
          </p>
        </div>
      </>
    );
  }

  return (
    <>
      <SiteHeader title="New script" backHref="/dashboard/settings/scripts" />
      <div className="flex flex-col flex-1 min-h-0">
        <div className="flex flex-col flex-1 min-h-0 gap-6 p-4 md:p-6">
          {error && <p className="text-destructive text-sm">{error}</p>}
          <ScriptForm state={state} onChange={setState} lexicons={lexicons} lexiconsLoading={lexiconsLoading} />
        </div>
        <footer className="bg-sidebar-accent flex justify-between gap-2 px-4 py-2 md:px-6 md:py-4 rounded-b-md">
          {isDirty ? (
            <AlertDialog>
              <AlertDialogTrigger asChild>
                <Button variant="outline">Cancel</Button>
              </AlertDialogTrigger>
              <AlertDialogContent>
                <AlertDialogHeader>
                  <AlertDialogTitle>Discard changes?</AlertDialogTitle>
                  <AlertDialogDescription>
                    You have unsaved changes that will be lost.
                  </AlertDialogDescription>
                </AlertDialogHeader>
                <AlertDialogFooter>
                  <AlertDialogCancel>Keep editing</AlertDialogCancel>
                  <AlertDialogAction
                    variant="destructive"
                    onClick={() => router.push("/dashboard/settings/scripts")}
                  >
                    Discard
                  </AlertDialogAction>
                </AlertDialogFooter>
              </AlertDialogContent>
            </AlertDialog>
          ) : (
            <Button
              variant="outline"
              onClick={() => router.push("/dashboard/settings/scripts")}
            >
              Cancel
            </Button>
          )}
          <Button
            onClick={handleSave}
            disabled={!canSave}
          >
            {saving ? "Creating..." : "Create script"}
            <kbd className="ml-2 text-xs text-muted-foreground opacity-60 hidden sm:inline">
              ⌘↵
            </kbd>
          </Button>
        </footer>
      </div>
    </>
  );
}

export default function NewScriptPage() {
  return (
    <Suspense
      fallback={
        <SiteHeader title="New script" backHref="/dashboard/settings/scripts" />
      }
    >
      <NewScriptInner />
    </Suspense>
  );
}

function initialState(searchParams: URLSearchParams): ScriptFormState {
  // Optional `?id=record.create:<nsid>` pre-fills the form — the lexicon
  // detail page links here with a candidate trigger id.
  const presetId = searchParams.get("id");
  if (presetId) {
    const parsed = parseTriggerId(presetId);
    if (parsed) {
      const isJob = parsed.kind === "job.run";
      return {
        kind: parsed.kind,
        suffix: parsed.suffix,
        source: isJob ? JOB_SOURCE : parsed.suffix,
        description: "",
        body: isJob ? DEFAULT_JOB_SCRIPT_BODY : DEFAULT_SCRIPT_BODY,
      };
    }
  }
  // Fallbacks to a sensible default. Suffix starts empty so the form
  // surfaces the "Pick a source to compose the trigger id" hint.
  const kind = (searchParams.get("kind") as TriggerKind | null) ?? "record.index";
  const source = searchParams.get("source") ?? searchParams.get("suffix") ?? "";
  const isJob = kind === "job.run" || source === JOB_SOURCE;
  return {
    kind: isJob ? "job.run" : kind,
    suffix: isJob ? "" : (searchParams.get("suffix") ?? ""),
    source: isJob ? JOB_SOURCE : source,
    description: "",
    body: isJob ? DEFAULT_JOB_SCRIPT_BODY : DEFAULT_SCRIPT_BODY,
  };
}
