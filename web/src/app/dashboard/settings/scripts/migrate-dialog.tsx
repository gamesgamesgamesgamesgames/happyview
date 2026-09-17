"use client";

import { useState } from "react";
import { Loader2, Wand2 } from "lucide-react";
import { toast } from "sonner";

import { codemodScript } from "@/lib/api";
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

/** Preview and apply the v3 codemod for one Lua script. */
export function MigrateScriptDialog({
  scriptId,
  currentBody,
  canApply,
  onApplied,
}: {
  scriptId: string;
  currentBody: string;
  canApply: boolean;
  onApplied: () => void;
}) {
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
    codemodScript(scriptId)
      .then(setResult)
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setLoading(false));
  }

  async function handleApply() {
    if (!result?.changed || applying) return;
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
        <Button variant="outline">
          <Wand2 className="size-4" />
          Migrate
        </Button>
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
            . Nothing is saved until you apply it.
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
                  {result.changed && (
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
          {canApply && (
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
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
