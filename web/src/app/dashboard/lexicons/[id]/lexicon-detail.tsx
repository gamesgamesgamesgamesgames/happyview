"use client";

import { useCallback, useEffect, useState } from "react";
import { IconPlus } from "@tabler/icons-react";
import Link from "next/link";
import { usePathname, useRouter } from "next/navigation";

import { useCurrentUser } from "@/hooks/use-current-user";
import { CodePanels } from "@/components/code-panels";
import {
  deleteLexicon,
  deleteNetworkLexicon,
  getLexicon,
  getScripts,
  uploadLexicon,
} from "@/lib/api";
import type { LexiconDetail } from "@/types/lexicons";
import type { Script, TriggerKind } from "@/types/scripts";
import { SiteHeader } from "@/components/site-header";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

export default function LexiconDetailPage() {
  const pathname = usePathname();
  const id = decodeURIComponent(
    pathname.split("/").filter(Boolean).pop() ?? "",
  );
  const { hasPermission } = useCurrentUser();
  const router = useRouter();
  const [lexicon, setLexicon] = useState<LexiconDetail | null>(null);
  // All scripts in the system — we filter to those targeting this
  // lexicon's id below. Best-effort: render an empty panel if the
  // scripts call fails so the rest of the page still works.
  const [scripts, setScripts] = useState<Script[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [saving, setSaving] = useState(false);

  // Editable text state. The lexicon page used to edit `script` and
  // `index_hook` columns inline via lua editors; those columns are
  // now managed via the Scripts subsystem (see "Scripts targeting
  // this lexicon" panel below). We pass the existing values through
  // unchanged on save so legacy data isn't accidentally NULLed.
  const [jsonText, setJsonText] = useState("");
  const [originalJson, setOriginalJson] = useState("");
  const [tokenCost, setTokenCost] = useState("");
  const [originalTokenCost, setOriginalTokenCost] = useState("");

  const load = useCallback(() => {
    // Fire both requests in parallel; the scripts list is best-effort.
    getScripts()
      .then(setScripts)
      .catch(() => setScripts([]));
    getLexicon(id)
      .then((lex) => {
        setLexicon(lex);
        const json = JSON.stringify(lex.lexicon_json, null, 2);
        setJsonText(json);
        setOriginalJson(json);
        setTokenCost(lex.token_cost != null ? String(lex.token_cost) : "");
        setOriginalTokenCost(
          lex.token_cost != null ? String(lex.token_cost) : "",
        );
      })
      .catch((e) => setError(e instanceof Error ? e.message : String(e)));
  }, [id]);

  useEffect(() => {
    load();
  }, [load]);

  const isDirty =
    jsonText !== originalJson || tokenCost !== originalTokenCost;

  async function handleSave() {
    if (!lexicon) return;
    setSaving(true);
    setError(null);
    try {
      const lexiconJson = JSON.parse(jsonText);
      await uploadLexicon({
        lexicon_json: lexiconJson,
        backfill: lexicon.backfill,
        // Preserve any legacy script / index_hook values verbatim — we
        // no longer edit them here, but leaving them out of the body
        // would NULL the columns on upsert and lose data.
        script: lexicon.script ?? undefined,
        index_hook: lexicon.index_hook ?? undefined,
        token_cost: tokenCost ? Number(tokenCost) : null,
      });
      load();
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  }

  async function handleDelete() {
    if (!lexicon) return;
    setDeleting(true);
    try {
      if (lexicon.source === "network") {
        await deleteNetworkLexicon(lexicon.id);
      } else {
        await deleteLexicon(lexicon.id);
      }
      router.push("/dashboard/lexicons");
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
      setDeleting(false);
    }
  }

  if (error && !lexicon) {
    return (
      <>
        <SiteHeader title="Lexicon" backHref="/dashboard/lexicons" />
        <div className="p-4 md:p-6">
          <p className="text-destructive text-sm">{error}</p>
        </div>
      </>
    );
  }

  if (!lexicon) {
    return (
      <>
        <SiteHeader title="Lexicon" backHref="/dashboard/lexicons" />
        <div className="p-4 md:p-6">
          <p className="text-muted-foreground text-sm">Loading...</p>
        </div>
      </>
    );
  }

  const isNetwork = lexicon.source === "network";
  const isRecord = lexicon.lexicon_type === "record";

  // Triggers that target this lexicon's id, given its type.
  //
  // Record-type lexicons get the four `record.*` slots (the cascade
  // wildcard `record.index` listed first as the most common starting
  // point, then the three action-specific slots), plus `labeler.apply`
  // for "react to labels arriving on records of this type."
  // XRPC lexicons get the matching `xrpc.{query,procedure}` slot only.
  const targetingTriggers: { kind: TriggerKind; label: string }[] = isRecord
    ? [
        { kind: "record.index", label: "Default handler (any action)" },
        { kind: "record.create", label: "On create" },
        { kind: "record.update", label: "On update" },
        { kind: "record.delete", label: "On delete" },
        { kind: "labeler.apply", label: "On label applied" },
      ]
    : lexicon.lexicon_type === "query"
      ? [{ kind: "xrpc.query", label: "Query handler" }]
      : lexicon.lexicon_type === "procedure"
        ? [{ kind: "xrpc.procedure", label: "Procedure handler" }]
        : [];

  return (
    <div className="flex flex-col h-full max-h-screen md:max-h-[calc(100vh-((var(--spacing)*2)*2))] overflow-hidden">
      <SiteHeader title={lexicon.id} backHref="/dashboard/lexicons" />
      <div className="flex flex-col flex-1 min-h-0 gap-6 items-stretch overflow-hidden">
        <div className="p-4 md:p-6">
          {error && <p className="text-destructive text-sm mb-4">{error}</p>}

          {/* Metadata */}
          <div className="grid grid-cols-2 gap-x-8 gap-y-3 sm:grid-cols-3 lg:grid-cols-4">
            <div>
              <Label className="text-muted-foreground">Type</Label>
              <div className="mt-1">
                <Badge variant="outline">{lexicon.lexicon_type}</Badge>
              </div>
            </div>
            <div>
              <Label className="text-muted-foreground">Source</Label>
              <div className="mt-1">
                <Badge variant={isNetwork ? "secondary" : "outline"}>
                  {lexicon.source}
                </Badge>
              </div>
            </div>
            <div>
              <Label className="text-muted-foreground">Revision</Label>
              <p className="mt-1 tabular-nums">{lexicon.revision}</p>
            </div>
            {lexicon.lexicon_type === "record" && (
              <div>
                <Label className="text-muted-foreground">Backfill</Label>
                <p className="mt-1">{lexicon.backfill ? "Yes" : "No"}</p>
              </div>
            )}
            {lexicon.authority_did && (
              <div className="col-span-2">
                <Label className="text-muted-foreground">Authority DID</Label>
                <p className="mt-1 font-mono text-sm break-all">
                  {lexicon.authority_did}
                </p>
              </div>
            )}
            <div>
              <Label className="text-muted-foreground">Created</Label>
              <p className="mt-1 text-sm">
                {new Date(lexicon.created_at).toLocaleString()}
              </p>
            </div>
            <div>
              <Label className="text-muted-foreground">Updated</Label>
              <p className="mt-1 text-sm">
                {new Date(lexicon.updated_at).toLocaleString()}
              </p>
            </div>
            {lexicon.last_fetched_at && (
              <div>
                <Label className="text-muted-foreground">Last Fetched</Label>
                <p className="mt-1 text-sm">
                  {new Date(lexicon.last_fetched_at).toLocaleString()}
                </p>
              </div>
            )}
            {(lexicon.lexicon_type === "query" ||
              lexicon.lexicon_type === "procedure") && (
              <div>
                <Label htmlFor="token-cost" className="text-muted-foreground">
                  Token Cost
                </Label>
                <Input
                  id="token-cost"
                  type="number"
                  min={0}
                  className="mt-1 w-24"
                  placeholder="default"
                  value={tokenCost}
                  onChange={(e) => setTokenCost(e.target.value)}
                  disabled={!hasPermission("lexicons:create")}
                />
              </div>
            )}
          </div>

          {/* Trigger-keyed scripts that target this lexicon. Each row
              either links to the existing script or to the New Script
              page pre-filled with the trigger id. */}
          {targetingTriggers.length > 0 && (
            <ScriptsTargetingPanel
              lexiconId={lexicon.id}
              scripts={scripts}
              entries={targetingTriggers}
              canManage={hasPermission("scripts:manage")}
            />
          )}
        </div>

        {/* JSON editor only — scripts (record-event handlers, XRPC
            handlers, label-arrival handlers) are managed via the
            "Scripts targeting this lexicon" panel above. The legacy
            `script` / `index_hook` columns on the lexicons table are
            preserved as-is on save but no longer edited here. */}
        <CodePanels
          className="flex-1 min-h-0 px-4 md:px-6"
          jsonValue={jsonText}
          onJsonChange={isNetwork ? undefined : setJsonText}
          jsonReadOnly={isNetwork}
        />

        {/* Actions */}
        <footer className="bg-sidebar-accent flex justify-between gap-2 ps-4 py-2 md:px-6 md:py-4 rounded-b-md">
          {hasPermission("lexicons:delete") && (
            <Button
              variant="destructive"
              onClick={handleDelete}
              disabled={deleting}
            >
              {deleting ? "Deleting..." : "Delete Lexicon"}
            </Button>
          )}

          <div className="flex gap-2">
            {hasPermission("lexicons:create") && (
              <Button onClick={handleSave} disabled={!isDirty || saving}>
                {saving ? "Saving..." : "Save"}
              </Button>
            )}
          </div>
        </footer>
      </div>
    </div>
  );
}

/**
 * Lists scripts targeting this lexicon and offers a "+ New" dropdown
 * for the slots that don't yet have a script.
 *
 * Existing scripts (those whose trigger id matches one of `entries`)
 * appear as rows linking to the Scripts detail page. Missing slots
 * are surfaced via a single "+ New script" dropdown so the operator
 * picks the kind of handler they want without seeing four "Create"
 * buttons stacked. The dropdown hides when every slot is taken.
 */
function ScriptsTargetingPanel({
  lexiconId,
  scripts,
  entries,
  canManage,
}: {
  lexiconId: string;
  scripts: Script[];
  entries: { kind: TriggerKind; label: string }[];
  canManage: boolean;
}) {
  const byId = new Map(scripts.map((s) => [s.id, s]));
  const existing = entries
    .map((e) => ({ ...e, triggerId: `${e.kind}:${lexiconId}` }))
    .filter((e) => byId.has(e.triggerId));
  const available = entries
    .map((e) => ({ ...e, triggerId: `${e.kind}:${lexiconId}` }))
    .filter((e) => !byId.has(e.triggerId));

  return (
    <div className="mt-6 rounded-md border p-4">
      <div className="mb-3 flex items-center justify-between gap-2">
        <div>
          <h3 className="text-sm font-semibold">
            Scripts targeting this lexicon
          </h3>
          <p className="text-muted-foreground text-xs">
            Each row is a{" "}
            <Link
              href="/dashboard/settings/scripts"
              className="underline hover:no-underline"
            >
              trigger
            </Link>{" "}
            the dispatcher resolves at firing time.
          </p>
        </div>
        {canManage && available.length > 0 && (
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="outline" size="sm">
                <IconPlus className="size-4" />
                New script
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              {available.map(({ kind, label, triggerId }) => (
                <DropdownMenuItem key={kind} asChild>
                  <Link
                    href={`/dashboard/settings/scripts/new?id=${encodeURIComponent(triggerId)}`}
                    className="flex flex-col items-start gap-0.5"
                  >
                    <span className="text-sm">{label}</span>
                    <span className="text-muted-foreground font-mono text-[11px]">
                      {triggerId}
                    </span>
                  </Link>
                </DropdownMenuItem>
              ))}
            </DropdownMenuContent>
          </DropdownMenu>
        )}
      </div>
      {existing.length === 0 ? (
        <p className="text-muted-foreground text-sm py-2">
          No scripts yet.
          {canManage && available.length > 0 && (
            <> Use the &ldquo;New script&rdquo; menu to add one.</>
          )}
        </p>
      ) : (
        <ul className="flex flex-col divide-y">
          {existing.map(({ kind, label, triggerId }) => (
            <li
              key={kind}
              className="flex items-center justify-between gap-2 py-2"
            >
              <Link
                href={`/dashboard/settings/scripts/${encodeURIComponent(triggerId)}`}
                className="flex flex-col gap-0.5 hover:underline"
              >
                <span className="text-sm">{label}</span>
                <span className="text-muted-foreground font-mono text-xs">
                  {triggerId}
                </span>
              </Link>
              <Button asChild variant="ghost" size="sm">
                <Link
                  href={`/dashboard/settings/scripts/${encodeURIComponent(triggerId)}`}
                >
                  Edit
                </Link>
              </Button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
