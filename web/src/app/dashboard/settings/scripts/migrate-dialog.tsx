"use client";

import { type ReactNode, useState } from "react";
import { Loader2, Wand2 } from "lucide-react";
import { toast } from "sonner";

import { codemodScript, previewCodemodDraft } from "@/lib/api";
import { toastError } from "@/lib/format";
import type { CodemodResult } from "@/types/scripts";
import { MonacoDiffEditor } from "@/components/monaco-editor";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";

/** Rewrites the stored script, and Apply stores the result. */
interface StoredTarget {
  canApply: boolean;
  onApplied: () => void;
  onRewritten?: undefined;
}

/**
 * Rewrites `currentBody` as the editor holds it, and hands the result back
 * for the editor to show. Nothing is stored, so it needs no stored script.
 */
interface DraftTarget {
  onRewritten: (source: string) => void;
  canApply?: undefined;
  onApplied?: undefined;
}

/** Preview and apply the v3 codemod for one Lua script. */
export function MigrateScriptDialog({
  scriptId,
  currentBody,
  trigger,
  canApply,
  onApplied,
  onRewritten,
}: {
  scriptId: string;
  currentBody: string;
  /** Replaces the default Migrate button as what opens the dialog. */
  trigger?: ReactNode;
} & (StoredTarget | DraftTarget)) {
  const isDraft = onRewritten !== undefined;
  const [open, setOpen] = useState(false);
  const [loading, setLoading] = useState(false);
  const [applying, setApplying] = useState(false);
  const [result, setResult] = useState<CodemodResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [allowMarkers, setAllowMarkers] = useState(false);

  function handleOpenChange(next: boolean) {
    setOpen(next);
    if (!next) {
      // Discard the preview so reopening always reflects the current body.
      setResult(null);
      setError(null);
      setAllowMarkers(false);
      return;
    }
    setLoading(true);
    setError(null);
    setAllowMarkers(false);
    (isDraft ? previewCodemodDraft(scriptId, currentBody) : codemodScript(scriptId))
      .then(setResult)
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setLoading(false));
  }

  async function handleApply() {
    if (!result?.changed || applying) return;
    if (isDraft) {
      onRewritten(result.source);
      handleOpenChange(false);
      return;
    }
    if (result.notes.length > 0 && !allowMarkers) return;
    setApplying(true);
    try {
      await codemodScript(scriptId, true, allowMarkers);
      toast.success("Script migrated");
      setOpen(false);
      setResult(null);
      onApplied();
    } catch (e: unknown) {
      toastError("Failed to apply migration", e);
    } finally {
      setApplying(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogTrigger asChild>
        {trigger ?? (
          <Button variant="outline">
            <Wand2 className="size-4" />
            Migrate
          </Button>
        )}
      </DialogTrigger>
      <DialogContent className="flex h-[85vh] flex-col sm:max-w-5xl">
        <DialogHeader>
          <DialogTitle>Migrate to the v3 contract</DialogTitle>
          <DialogDescription>
            Rewrites removed globals onto{" "}
            <code className="bg-muted rounded px-1 font-mono text-xs">
              require(&quot;internal.*&quot;)
            </code>{" "}
            and{" "}
            <code className="bg-muted rounded px-1 font-mono text-xs">
              require(&quot;happyview.*&quot;)
            </code>
            .{" "}
            {isDraft
              ? "Accepting it replaces the editor's contents; nothing is saved until you save the script."
              : "Nothing is saved until you apply it."}
          </DialogDescription>
        </DialogHeader>

        <div className="flex flex-1 flex-col gap-3 overflow-hidden">
          {loading && (
            <div className="text-muted-foreground flex flex-1 items-center justify-center gap-2 text-sm">
              <Loader2 className="size-4 animate-spin" />
              Rewriting...
            </div>
          )}

          {error && !loading && (
            <p className="text-destructive text-sm">{error}</p>
          )}

          {result && !loading && !error && (
            <>
              {result.changed ? (
                <div className="min-h-0 flex-1 overflow-hidden rounded-md border">
                  <MonacoDiffEditor
                    original={currentBody}
                    modified={result.source}
                    language="lua"
                    className="h-full"
                  />
                </div>
              ) : result.notes.length === 0 ? (
                <p className="text-muted-foreground flex flex-1 items-center justify-center text-sm">
                  Already on the v3 contract — nothing to rewrite.
                </p>
              ) : null}

              {result.notes.length > 0 && (
                <div className="max-h-40 shrink-0 overflow-y-auto rounded-md border p-3">
                  <p className="text-xs font-medium">
                    {result.notes.length} construct
                    {result.notes.length === 1 ? "" : "s"} left for you to
                    finish, marked{" "}
                    <code className="bg-muted rounded px-1 font-mono text-xs">
                      -- codemod:
                    </code>{" "}
                    in the rewritten source. Line numbers below refer to the
                    original script, not the diff:
                  </p>
                  <ul className="mt-2 flex flex-col gap-1">
                    {result.notes.map((note, i) => (
                      <li key={i} className="text-muted-foreground text-xs">
                        <span className="font-mono">line {note.line}</span> —{" "}
                        {note.message}
                      </li>
                    ))}
                  </ul>
                  {isDraft && (
                    <p className="mt-3 text-xs">
                      A marked line still references a removed global, so the
                      script is refused on save until each one is rewritten by
                      hand.
                    </p>
                  )}
                  {result.changed && !isDraft && (
                    <label className="mt-3 flex items-center gap-2 text-xs">
                      <Checkbox
                        checked={allowMarkers}
                        onCheckedChange={(checked) =>
                          setAllowMarkers(checked === true)
                        }
                      />
                      Apply with {result.notes.length} marker
                      {result.notes.length === 1 ? "" : "s"} remaining
                    </label>
                  )}
                </div>
              )}
            </>
          )}
        </div>

        <DialogFooter>
          <DialogClose asChild>
            <Button variant="outline">Cancel</Button>
          </DialogClose>
          {isDraft ? (
            <Button onClick={handleApply} disabled={!result?.changed}>
              Use rewritten script
            </Button>
          ) : (
            canApply && (
              <Button
                onClick={handleApply}
                disabled={
                  !result?.changed ||
                  applying ||
                  (result.notes.length > 0 && !allowMarkers)
                }
              >
                {applying ? "Applying..." : "Apply"}
              </Button>
            )
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
