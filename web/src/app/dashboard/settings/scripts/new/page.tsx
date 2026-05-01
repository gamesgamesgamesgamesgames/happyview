"use client";

import { Suspense, useEffect, useState } from "react";
import { useRouter, useSearchParams } from "next/navigation";

import { useCurrentUser } from "@/hooks/use-current-user";
import { getLexicons, upsertScript } from "@/lib/api";
import type { LexiconSummary } from "@/types/lexicons";
import type { TriggerKind } from "@/types/scripts";
import { DEFAULT_SCRIPT_BODY, parseTriggerId } from "@/types/scripts";
import { SiteHeader } from "@/components/site-header";
import { Button } from "@/components/ui/button";

import {
  ScriptForm,
  type ScriptFormState,
  composeTriggerId,
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
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // If the URL changes (e.g. user navigates with new ?id=...), refresh state.
  useEffect(() => {
    setState(initialState(searchParams));
  }, [searchParams]);

  useEffect(() => {
    getLexicons()
      .then(setLexicons)
      .catch(() => setLexicons([]));
  }, []);

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

  async function handleSave() {
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
  }

  return (
    <>
      <SiteHeader title="New script" backHref="/dashboard/settings/scripts" />
      <div className="flex flex-col flex-1 min-h-0">
        <div className="flex flex-col flex-1 min-h-0 gap-6 p-4 md:p-6">
          {error && <p className="text-destructive text-sm">{error}</p>}
          <ScriptForm state={state} onChange={setState} lexicons={lexicons} />
        </div>
        <footer className="bg-sidebar-accent flex justify-end gap-2 px-4 py-2 md:px-6 md:py-4 rounded-b-md">
          <Button onClick={handleSave} disabled={saving || !state.suffix}>
            {saving ? "Creating..." : "Create script"}
          </Button>
        </footer>
      </div>
    </>
  );
}

export default function NewScriptPage() {
  return (
    <Suspense fallback={null}>
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
      return {
        kind: parsed.kind,
        suffix: parsed.suffix,
        description: "",
        body: DEFAULT_SCRIPT_BODY,
      };
    }
  }
  // Fallbacks to a sensible default. Suffix starts empty so the form
  // surfaces the "Pick a lexicon to compose the trigger id" hint.
  const kind = (searchParams.get("kind") as TriggerKind | null) ?? "record.index";
  return {
    kind,
    suffix: searchParams.get("suffix") ?? "",
    description: "",
    body: DEFAULT_SCRIPT_BODY,
  };
}
