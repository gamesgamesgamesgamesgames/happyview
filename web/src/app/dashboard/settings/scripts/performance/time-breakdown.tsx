import type { ReactNode } from "react";

import type { HostImportStats } from "@/types/metrics";
import { cn } from "@/lib/utils";

import {
  type Breakdown,
  type Segment,
  formatMs,
  shareText,
} from "./metrics-format";

/**
 * Each segment's fill. The script's own time is the one colour; the
 * platform's stages are greys, told apart by depth, so the bar reads as
 * "how much of this is mine" before anything else.
 */
const SEGMENT_CLASS: Record<string, string> = {
  guest: "bg-blue-500",
  host: "bg-blue-500/50",
  prepare: "bg-muted-foreground/70",
  link: "bg-muted-foreground/50",
  allocate: "bg-muted-foreground/35",
  settle: "bg-muted-foreground/60",
  teardown: "bg-muted-foreground/25",
};

function segmentClass(segment: Segment): string {
  return (
    SEGMENT_CLASS[segment.key] ??
    (segment.side === "script" ? "bg-blue-500" : "bg-muted-foreground/40")
  );
}

/** A thin bar of the script's share against the platform's. */
export function ShareBar({
  scriptShare,
  className,
}: {
  scriptShare: number;
  className?: string;
}) {
  const label = `Script ${shareText(scriptShare)}, platform ${shareText(1 - scriptShare)}`;
  return (
    <div
      role="img"
      aria-label={label}
      title={label}
      className={cn(
        "flex h-1.5 w-full overflow-hidden rounded-full bg-muted-foreground/30",
        className,
      )}
    >
      <div
        className="h-full bg-blue-500"
        style={{ width: `${Math.max(0, Math.min(1, scriptShare)) * 100}%` }}
      />
    </div>
  );
}

/**
 * Where a mean run's time goes: a stacked bar of its stages, then each
 * stage with its time and share, grouped into the script's and the
 * platform's.
 */
export function TimeBreakdown({
  breakdown,
  hostImports,
}: {
  breakdown: Breakdown;
  hostImports: HostImportStats[];
}) {
  if (breakdown.scriptShare == null) {
    return (
      <p className="text-sm text-muted-foreground">
        No stage timings were recorded in this window.
      </p>
    );
  }
  const script = breakdown.segments.filter((s) => s.side === "script");
  const platform = breakdown.segments.filter((s) => s.side === "platform");
  const topImports = hostImports.slice(0, 5);

  return (
    <div className="flex flex-col gap-3">
      <p className="text-sm">
        Script {formatMs(breakdown.scriptMs)} of{" "}
        {formatMs(breakdown.scriptMs + breakdown.platformMs)} a run{" "}
        <span className="text-muted-foreground">
          · script {shareText(breakdown.scriptShare)} · platform{" "}
          {shareText(1 - breakdown.scriptShare)}
        </span>
      </p>
      <div
        role="img"
        aria-label={breakdown.segments
          .map((s) => `${s.label} ${shareText(s.share)}`)
          .join(", ")}
        className="flex h-3 w-full gap-px overflow-hidden rounded-full bg-background"
      >
        {breakdown.segments.map((s) => (
          <div
            key={s.key}
            className={cn("h-full min-w-px", segmentClass(s))}
            style={{ width: `${s.share * 100}%` }}
            title={`${s.label}: ${formatMs(s.ms)} (${shareText(s.share)})`}
          />
        ))}
      </div>
      <div className="grid gap-4 sm:grid-cols-2">
        <StageList
          title="Script"
          hint="Your code and the host calls it makes"
          segments={script}
        >
          {topImports.length > 0 && (
            <ul className="mt-1 flex flex-col gap-1 border-l pl-3">
              {topImports.map((imp) => (
                <li
                  key={imp.name}
                  className="flex items-baseline justify-between gap-2 text-xs text-muted-foreground"
                >
                  <span className="truncate font-mono" title={imp.name}>
                    {imp.name}
                  </span>
                  <span className="shrink-0 tabular-nums">
                    {imp.calls.toLocaleString()}{" "}
                    {imp.calls === 1 ? "call" : "calls"} ·{" "}
                    {formatMs(imp.ms_mean)} each
                  </span>
                </li>
              ))}
            </ul>
          )}
        </StageList>
        <StageList
          title="Platform"
          hint="HappyView setting up the run and cleaning up after it"
          segments={platform}
        />
      </div>
    </div>
  );
}

function StageList({
  title,
  hint,
  segments,
  children,
}: {
  title: string;
  segments: Segment[];
  hint: string;
  children?: ReactNode;
}) {
  return (
    <div className="flex flex-col gap-1.5">
      <div>
        <h4 className="text-sm font-medium">{title}</h4>
        <p className="text-xs text-muted-foreground">{hint}</p>
      </div>
      {segments.length === 0 ? (
        <p className="text-xs text-muted-foreground">Nothing measured.</p>
      ) : (
        <ul className="flex flex-col gap-1">
          {segments.map((s) => (
            <li key={s.key} className="flex flex-col gap-1">
              <div className="flex items-center justify-between gap-2 text-sm">
                <span className="flex items-center gap-2">
                  <span
                    aria-hidden
                    className={cn("size-2.5 rounded-[2px]", segmentClass(s))}
                  />
                  {s.label}
                </span>
                <span className="text-xs text-muted-foreground tabular-nums">
                  {formatMs(s.ms)} · {shareText(s.share)}
                </span>
              </div>
              {s.key === "host" && children}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
