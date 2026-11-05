"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { IconChartBar } from "@tabler/icons-react";

import { getScriptSeries } from "@/lib/api";
import type { ScriptStats } from "@/types/metrics";

import { summaryLine } from "./metrics-format";

/**
 * The script editor's one line on the last hour, linking to the script's
 * graphs. Says nothing when the script has not run or the read fails: it is
 * a glance, not something to raise an error over.
 */
export function ScriptMetricsSummary({ trigger }: { trigger: string }) {
  const [stats, setStats] = useState<{
    trigger: string;
    value: ScriptStats;
  } | null>(null);

  useEffect(() => {
    let cancelled = false;
    getScriptSeries(trigger, "1h")
      .then((series) => {
        if (!cancelled) setStats({ trigger, value: series });
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [trigger]);

  if (!stats || stats.trigger !== trigger || stats.value.runs === 0) {
    return null;
  }

  return (
    <Link
      href={`/dashboard/settings/scripts/performance?trigger=${encodeURIComponent(trigger)}`}
      className="inline-flex w-fit items-center gap-1.5 text-xs text-muted-foreground tabular-nums hover:text-foreground hover:underline underline-offset-2"
    >
      <IconChartBar className="size-3.5" aria-hidden />
      {summaryLine(stats.value)}
    </Link>
  );
}
