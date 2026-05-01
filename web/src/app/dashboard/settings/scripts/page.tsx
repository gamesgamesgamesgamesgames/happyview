"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import Link from "next/link";

import { useCurrentUser } from "@/hooks/use-current-user";
import { deleteScript, getScripts } from "@/lib/api";
import type { Script, TriggerFamily } from "@/types/scripts";
import {
  TRIGGER_FAMILY_LABELS,
  TRIGGER_KIND_LABELS,
  familyOf,
  parseTriggerId,
} from "@/types/scripts";
import { SiteHeader } from "@/components/site-header";
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

const FAMILY_ORDER: TriggerFamily[] = ["record", "xrpc", "labeler"];

export default function ScriptsPage() {
  const { hasPermission } = useCurrentUser();
  const [scripts, setScripts] = useState<Script[]>([]);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    getScripts()
      .then(setScripts)
      .catch((e) => setError(e instanceof Error ? e.message : String(e)));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const grouped = useMemo(() => {
    const map: Record<TriggerFamily, Script[]> = {
      record: [],
      xrpc: [],
      labeler: [],
    };
    for (const s of scripts) {
      const parsed = parseTriggerId(s.id);
      // Group unparseable rows under their best-effort family — tolerant
      // because the operator may have created them via direct DB access.
      const fam: TriggerFamily = parsed ? familyOf(parsed.kind) : "record";
      map[fam].push(s);
    }
    for (const fam of FAMILY_ORDER) {
      map[fam].sort((a, b) => a.id.localeCompare(b.id));
    }
    return map;
  }, [scripts]);

  async function handleDelete(id: string) {
    if (!confirm(`Delete script '${id}'?`)) return;
    try {
      await deleteScript(id);
      load();
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  return (
    <>
      <SiteHeader title="Scripts" />
      <div className="flex flex-1 flex-col gap-6 p-4 md:p-6">
        {error && <p className="text-destructive text-sm">{error}</p>}

        <div className="flex items-baseline justify-between">
          <p className="text-muted-foreground text-sm">
            Each row is a Lua script bound to a trigger. The script&apos;s{" "}
            <span className="font-mono">id</span> IS its trigger string —
            no separate name or binding step. The dispatcher resolves
            scripts by id at firing time.
          </p>
          {hasPermission("scripts:manage") && (
            <Button asChild>
              <Link href="/dashboard/settings/scripts/new">New script</Link>
            </Button>
          )}
        </div>

        {scripts.length === 0 && (
          <div className="bg-sidebar-accent text-muted-foreground rounded-md border p-6 text-center text-sm">
            No scripts yet.
            {hasPermission("scripts:manage") && (
              <>
                {" "}
                <Link
                  href="/dashboard/settings/scripts/new"
                  className="underline hover:no-underline"
                >
                  Create one
                </Link>
                .
              </>
            )}
          </div>
        )}

        {FAMILY_ORDER.map((fam) => {
          const rows = grouped[fam];
          if (rows.length === 0) return null;
          return (
            <section key={fam} className="flex flex-col gap-2">
              <h2 className="text-base font-semibold">
                {TRIGGER_FAMILY_LABELS[fam]}
              </h2>
              <div className="overflow-clip rounded-lg border">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Trigger</TableHead>
                      <TableHead>Kind</TableHead>
                      <TableHead>Description</TableHead>
                      <TableHead>Updated</TableHead>
                      <TableHead className="w-10 sticky right-0 bg-inherit z-[1]" />
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {rows.map((s) => {
                      const parsed = parseTriggerId(s.id);
                      return (
                        <TableRow key={s.id}>
                          <TableCell className="font-mono text-xs">
                            <Link
                              href={`/dashboard/settings/scripts/${encodeURIComponent(s.id)}`}
                              className="underline hover:no-underline"
                            >
                              {s.id}
                            </Link>
                          </TableCell>
                          <TableCell>
                            {parsed ? (
                              <Badge variant="outline">
                                {TRIGGER_KIND_LABELS[parsed.kind]}
                              </Badge>
                            ) : (
                              <Badge variant="destructive">malformed</Badge>
                            )}
                          </TableCell>
                          <TableCell className="text-muted-foreground text-sm">
                            {s.description ?? ""}
                          </TableCell>
                          <TableCell className="text-muted-foreground text-sm">
                            {new Date(s.updated_at).toLocaleString()}
                          </TableCell>
                          <TableCell className="sticky right-0 bg-inherit z-[1]">
                            {hasPermission("scripts:manage") && (
                              <Button
                                variant="ghost"
                                size="sm"
                                onClick={() => handleDelete(s.id)}
                              >
                                Delete
                              </Button>
                            )}
                          </TableCell>
                        </TableRow>
                      );
                    })}
                  </TableBody>
                </Table>
              </div>
            </section>
          );
        })}
      </div>
    </>
  );
}
