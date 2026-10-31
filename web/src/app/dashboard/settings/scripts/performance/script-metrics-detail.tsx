"use client";

import { useEffect, useMemo, useState } from "react";
import Link from "next/link";
import { Loader2 } from "lucide-react";

import { ApiError, getScriptSeries } from "@/lib/api";
import { formatBytes } from "@/lib/format";
import type { MetricsWindow, ScriptSeriesResponse } from "@/types/metrics";
import { Badge } from "@/components/ui/badge";
import {
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";

import {
  chartPoints,
  errorCount,
  failureBreakdown,
  formatMs,
  kindLabel,
  retentionNote,
  runBreakdown,
  stepFor,
  timeoutCount,
  windowText,
} from "./metrics-format";
import { SeriesCharts } from "./series-charts";
import { TimeBreakdown } from "./time-breakdown";

type Load =
  | { state: "loading" }
  | { state: "ready"; series: ScriptSeriesResponse }
  | { state: "none" }
  | { state: "failed"; message: string };

/** One script's totals, graphs and time breakdown, inside a sheet. */
export function ScriptMetricsDetail({
  trigger,
  window,
}: {
  trigger: string;
  window: MetricsWindow;
}) {
  const [load, setLoad] = useState<{ key: string; value: Load } | null>(null);
  const key = `${trigger} ${window}`;

  useEffect(() => {
    let cancelled = false;
    getScriptSeries(trigger, window, stepFor(window))
      .then((series) => {
        if (!cancelled) setLoad({ key, value: { state: "ready", series } });
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        setLoad({
          key,
          value:
            e instanceof ApiError && e.status === 404
              ? { state: "none" }
              : {
                  state: "failed",
                  message: e instanceof Error ? e.message : String(e),
                },
        });
      });
    return () => {
      cancelled = true;
    };
  }, [trigger, window, key]);

  const current: Load =
    load && load.key === key ? load.value : { state: "loading" };

  return (
    <>
      <SheetHeader>
        <SheetTitle className="pr-8 font-mono text-sm break-all">
          {trigger}
        </SheetTitle>
        <SheetDescription>
          Over {windowText(window)}.{" "}
          <Link
            href={`/dashboard/settings/scripts/${encodeURIComponent(trigger)}`}
            className="underline underline-offset-2"
          >
            Open script
          </Link>
        </SheetDescription>
      </SheetHeader>
      <div className="flex-1 min-h-0 overflow-y-auto px-4 pb-6 flex flex-col gap-6">
        {current.state === "loading" && (
          <Loader2 className="size-4 animate-spin mx-auto my-8 text-muted-foreground" />
        )}
        {current.state === "none" && (
          <p className="text-sm text-muted-foreground">
            This script has not run in {windowText(window)}.
          </p>
        )}
        {current.state === "failed" && (
          <p className="text-sm text-destructive">{current.message}</p>
        )}
        {current.state === "ready" && (
          <SeriesBody series={current.series} window={window} />
        )}
      </div>
    </>
  );
}

function SeriesBody({
  series,
  window,
}: {
  series: ScriptSeriesResponse;
  window: MetricsWindow;
}) {
  const points = useMemo(() => chartPoints(series.points), [series.points]);
  const breakdown = useMemo(() => runBreakdown(series), [series]);
  const errors = errorCount(series.outcomes);
  const timeouts = timeoutCount(series.outcomes);
  const failures = failureBreakdown(series.outcomes);
  const note = retentionNote(window, series.retention_days);

  return (
    <>
      <div className="flex flex-wrap gap-1">
        <Badge variant="outline">{kindLabel(series.kind)}</Badge>
        {series.layers.map((layer) => (
          <Badge key={layer} variant="secondary">
            {layer === "project" ? "Project file" : "Dashboard"}
          </Badge>
        ))}
      </div>

      {note && <p className="text-xs text-muted-foreground">{note}</p>}

      <dl className="grid grid-cols-3 gap-x-4 gap-y-3 text-sm sm:grid-cols-4">
        <Stat label="Runs" value={series.runs.toLocaleString()} />
        <Stat label="p50" value={formatMs(series.wall_ms.p50)} />
        <Stat label="p95" value={formatMs(series.wall_ms.p95)} />
        <Stat label="Max" value={formatMs(series.wall_ms.max)} />
        <Stat
          label="Errors"
          value={errors.toLocaleString()}
          title={failures || undefined}
          alert={errors > 0}
        />
        <Stat
          label="Timeouts"
          value={timeouts.toLocaleString()}
          alert={timeouts > 0}
        />
        <Stat
          label="Memory peak"
          value={
            series.memory_peak_bytes.max == null
              ? "—"
              : formatBytes(series.memory_peak_bytes.max)
          }
          title={
            series.memory_peak_bytes.mean == null
              ? undefined
              : `Mean ${formatBytes(series.memory_peak_bytes.mean)}`
          }
        />
        {series.fuel_mean != null && (
          <Stat
            label="Fuel per run"
            value={Math.round(series.fuel_mean).toLocaleString()}
          />
        )}
      </dl>

      <section className="flex flex-col gap-2">
        <h3 className="text-sm font-medium">Where a run&rsquo;s time goes</h3>
        <TimeBreakdown
          breakdown={breakdown}
          hostImports={series.host_imports}
        />
      </section>

      <SeriesCharts points={points} step={series.step} />
    </>
  );
}

function Stat({
  label,
  value,
  title,
  alert = false,
}: {
  label: string;
  value: string;
  title?: string;
  alert?: boolean;
}) {
  return (
    <div title={title}>
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd
        className={`tabular-nums ${alert ? "text-destructive font-medium" : ""}`}
      >
        {value}
      </dd>
    </div>
  );
}
