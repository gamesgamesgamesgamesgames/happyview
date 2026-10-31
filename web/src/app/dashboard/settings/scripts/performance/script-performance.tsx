"use client";

import { useCallback, useEffect, useState } from "react";
import { usePathname, useRouter, useSearchParams } from "next/navigation";
import { AlertTriangle } from "lucide-react";

import { useCurrentUser } from "@/hooks/use-current-user";
import { getIngestMetrics, getScriptMetrics } from "@/lib/api";
import { toastError } from "@/lib/format";
import type {
  IngestMetricsResponse,
  MetricsWindow,
  ScriptMetricsListResponse,
} from "@/types/metrics";
import { SiteHeader } from "@/components/site-header";
import { Sheet, SheetContent } from "@/components/ui/sheet";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";

import { IngestTable } from "./ingest-table";
import { WINDOWS, retentionNote, windowText } from "./metrics-format";
import { ScriptMetricsDetail } from "./script-metrics-detail";
import { ScriptMetricsTable } from "./script-metrics-table";

const WINDOW_LABELS: Record<MetricsWindow, string> = {
  "1h": "1 hour",
  "24h": "24 hours",
  "7d": "7 days",
  "30d": "30 days",
};

function windowOf(raw: string | null): MetricsWindow {
  return (WINDOWS as readonly string[]).includes(raw ?? "")
    ? (raw as MetricsWindow)
    : "1h";
}

export default function ScriptPerformance() {
  const { hasPermission, loading: userLoading } = useCurrentUser();
  const canRead = hasPermission("metrics:read");
  const router = useRouter();
  const pathname = usePathname();
  const searchParams = useSearchParams();
  const window = windowOf(searchParams.get("window"));
  const selected = searchParams.get("trigger");

  const [scripts, setScripts] = useState<ScriptMetricsListResponse | null>(
    null,
  );
  const [ingest, setIngest] = useState<IngestMetricsResponse | null>(null);
  const [loadedFor, setLoadedFor] = useState<MetricsWindow | null>(null);

  // The window and the open script live in the URL, so a link from the
  // script editor opens straight onto its detail.
  const setParams = useCallback(
    (next: { window?: MetricsWindow; trigger?: string | null }) => {
      const qs = new URLSearchParams(searchParams.toString());
      if (next.window) {
        if (next.window === "1h") qs.delete("window");
        else qs.set("window", next.window);
      }
      if (next.trigger !== undefined) {
        if (next.trigger) qs.set("trigger", next.trigger);
        else qs.delete("trigger");
      }
      const query = qs.toString();
      router.replace(query ? `${pathname}?${query}` : pathname, {
        scroll: false,
      });
    },
    [pathname, router, searchParams],
  );

  useEffect(() => {
    if (!canRead) return;
    let cancelled = false;
    Promise.all([getScriptMetrics(window), getIngestMetrics(window)])
      .then(([s, i]) => {
        if (cancelled) return;
        setScripts(s);
        setIngest(i);
        setLoadedFor(window);
      })
      .catch((e) => {
        if (cancelled) return;
        toastError("Failed to load script performance", e);
        setLoadedFor(window);
      });
    return () => {
      cancelled = true;
    };
  }, [canRead, window]);

  const loading = loadedFor !== window;
  const fresh = !loading;
  const scriptRows = fresh && scripts?.window === window ? scripts.scripts : [];
  const ingestRows =
    fresh && ingest?.window === window ? ingest.collections : [];
  const disabled =
    (scripts && scripts.enabled === false) ||
    (ingest && ingest.enabled === false);
  const note = retentionNote(
    window,
    scripts?.window === window ? scripts.retention_days : undefined,
  );

  if (!userLoading && !canRead) {
    return (
      <>
        <SiteHeader title="Script performance" />
        <div className="p-4 md:p-6">
          <p className="text-sm text-muted-foreground">
            Seeing script performance needs the{" "}
            <span className="font-mono">metrics:read</span> permission.
          </p>
        </div>
      </>
    );
  }

  return (
    <>
      <SiteHeader title="Script performance" />
      {/* min-w-0: a wide table scrolls inside its own box rather than
          widening the page. */}
      <div className="flex min-w-0 flex-1 flex-col gap-6 p-4 md:p-6">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <p className="max-w-prose text-sm text-muted-foreground">
            How often each script runs, how long it takes, and how much of that
            time is the script&rsquo;s own.
          </p>
          <ToggleGroup
            type="single"
            variant="outline"
            size="sm"
            value={window}
            onValueChange={(value) => {
              if (value) setParams({ window: value as MetricsWindow });
            }}
            aria-label="Window"
          >
            {WINDOWS.map((w) => (
              <ToggleGroupItem
                key={w}
                value={w}
                aria-label={`Last ${WINDOW_LABELS[w]}`}
                className="px-3"
              >
                {w}
              </ToggleGroupItem>
            ))}
          </ToggleGroup>
        </div>

        {disabled && (
          <div className="flex items-start gap-3 rounded-lg border border-amber-500/50 bg-amber-500/10 p-3">
            <AlertTriangle className="size-4 text-amber-500 shrink-0 mt-0.5" />
            <p className="text-xs text-amber-700 dark:text-amber-400">
              Script metrics are switched off, so new runs are not recorded.
              Turn them on with the{" "}
              <span className="font-mono">script_metrics_enabled</span> setting.
              What was recorded before stays until it leaves its window.
            </p>
          </div>
        )}

        {note && <p className="text-xs text-muted-foreground">{note}</p>}

        <section className="flex min-w-0 flex-col gap-3">
          <h2 className="text-lg font-semibold">Scripts</h2>
          <ScriptMetricsTable
            rows={scriptRows}
            loading={loading}
            emptyText={`No script runs in ${windowText(window)}.`}
            onSelect={(trigger) => setParams({ trigger })}
          />
          {fresh && scripts && scripts.overflow > 0 && (
            <p className="text-xs text-muted-foreground">
              {scripts.overflow.toLocaleString()} more{" "}
              {scripts.overflow === 1 ? "script was" : "scripts were"} not
              recorded, because 1,024 were already held.
            </p>
          )}
        </section>

        <section className="flex min-w-0 flex-col gap-3">
          <div>
            <h2 className="text-lg font-semibold">Record pipeline</h2>
            <p className="text-sm text-muted-foreground">
              What each record costs on its way in, and how much of that is its
              hook&rsquo;s script.
            </p>
          </div>
          <IngestTable
            rows={ingestRows}
            loading={loading}
            emptyText={`No records came in during ${windowText(window)}.`}
          />
        </section>

        <Sheet
          open={selected != null}
          onOpenChange={(open) => {
            if (!open) setParams({ trigger: null });
          }}
        >
          <SheetContent className="sm:max-w-2xl overflow-hidden flex flex-col">
            {selected && (
              <ScriptMetricsDetail trigger={selected} window={window} />
            )}
          </SheetContent>
        </Sheet>
      </div>
    </>
  );
}
