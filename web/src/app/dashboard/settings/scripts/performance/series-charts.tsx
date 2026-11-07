"use client";

import { useMemo } from "react";
import {
  Bar,
  BarChart,
  CartesianGrid,
  Line,
  LineChart,
  XAxis,
  YAxis,
} from "recharts";

import {
  type ChartConfig,
  ChartContainer,
  ChartLegend,
  ChartLegendContent,
  ChartTooltip,
} from "@/components/ui/chart";
import type { MetricsStep } from "@/types/metrics";

import {
  type ChartPoint,
  formatMs,
  latencyAxisTop,
  stepNoun,
  tickFormat,
  withMaxOverflow,
} from "./metrics-format";

/** The legend's mark for max, drawn as the line is: a short grey dash. */
function DashIcon() {
  return (
    <svg viewBox="0 0 12 12" aria-hidden>
      <line
        x1="0"
        y1="6"
        x2="12"
        y2="6"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeDasharray="3 2"
      />
    </svg>
  );
}

/**
 * The mark for an off-scale step whose neighbours are not: a short solid
 * dash on the top edge, the same stroke as the edge line it stands in for.
 */
function OverflowTick({
  cx,
  cy,
  payload,
}: {
  cx?: number;
  cy?: number;
  payload?: { maxOverAlone?: boolean };
}) {
  if (cx == null || cy == null || !payload?.maxOverAlone) return <g />;
  return (
    <line
      x1={cx - 4}
      y1={cy}
      x2={cx + 4}
      y2={cy}
      stroke="var(--color-max)"
      strokeWidth={2}
      strokeLinecap="round"
    />
  );
}

/**
 * Series colours, from the theme's chart tokens. Each pair is chosen per
 * theme so the two latency lines stay apart and errors stay red in both.
 */
const runsConfig = {
  runs: { label: "Runs", color: "var(--chart-2)" },
} satisfies ChartConfig;

const latencyConfig = {
  p50: {
    label: "p50",
    theme: { light: "var(--chart-3)", dark: "var(--chart-1)" },
  },
  p95: {
    label: "p95",
    theme: { light: "var(--chart-1)", dark: "var(--chart-4)" },
  },
  // Max is context, not one of the pair: a thin grey dash in both themes.
  max: { label: "Max", color: "var(--muted-foreground)", icon: DashIcon },
  maxShown: { label: "Max", color: "var(--muted-foreground)", icon: DashIcon },
} satisfies ChartConfig;

const failuresConfig = {
  errors: { label: "Errors", color: "var(--destructive)" },
  timeouts: {
    label: "Timeouts",
    theme: { light: "var(--chart-5)", dark: "var(--chart-3)" },
  },
} satisfies ChartConfig;

type Format = (value: number) => string;

const countFormat: Format = (v) => v.toLocaleString();

interface TooltipEntry {
  dataKey?: unknown;
  name?: unknown;
  value?: unknown;
  color?: string;
  payload?: unknown;
}

/**
 * A tooltip that prints zero as zero and a gap as "no runs", where the
 * stock one leaves both out.
 */
function SeriesTooltip({
  active,
  payload,
  label,
  config,
  format,
  timeFormat,
  rowKeys,
}: {
  active?: boolean;
  payload?: readonly TooltipEntry[];
  label?: unknown;
  config: ChartConfig;
  format: Format;
  timeFormat: Intl.DateTimeFormat;
  /**
   * Read these fields from the hovered row rather than from the drawn
   * series, for a chart that draws one value as two (max's line and caret).
   */
  rowKeys?: string[];
}) {
  if (!active || !payload?.length || typeof label !== "number") return null;
  const row = payload[0]?.payload as Record<string, unknown> | undefined;
  const entries: TooltipEntry[] = rowKeys
    ? rowKeys.map((key) => ({
        dataKey: key,
        value: row?.[key],
        color: `var(--color-${key})`,
      }))
    : [...payload];
  return (
    <div className="border-border/50 bg-background grid min-w-[8rem] gap-1.5 rounded-lg border px-2.5 py-1.5 text-xs shadow-xl">
      <div className="font-medium">{timeFormat.format(label)}</div>
      {entries.map((item) => {
        const key = String(item.dataKey);
        const value = typeof item.value === "number" ? item.value : null;
        return (
          <div key={key} className="flex items-center gap-2">
            <span
              aria-hidden
              className="size-2.5 shrink-0 rounded-[2px]"
              style={{ background: item.color }}
            />
            <span className="text-muted-foreground flex-1">
              {config[key]?.label ?? key}
            </span>
            <span className="text-foreground font-mono font-medium tabular-nums">
              {value == null ? "no runs" : format(value)}
            </span>
          </div>
        );
      })}
    </div>
  );
}

function useTimeFormats(step: MetricsStep) {
  return useMemo(() => {
    const tick = new Intl.DateTimeFormat(undefined, tickFormat(step));
    const full = new Intl.DateTimeFormat(
      undefined,
      step === "1d"
        ? { dateStyle: "medium", timeZone: "UTC" }
        : { dateStyle: "medium", timeStyle: "short" },
    );
    return { tick, full };
  }, [step]);
}

function ChartSection({
  title,
  note,
  children,
}: {
  title: string;
  note?: string | null;
  children: React.ReactNode;
}) {
  return (
    <section className="flex flex-col gap-2">
      <div>
        <h4 className="text-sm font-medium">{title}</h4>
        {note && <p className="text-xs text-muted-foreground">{note}</p>}
      </div>
      {children}
    </section>
  );
}

function xAxis(tick: Intl.DateTimeFormat) {
  return (
    <XAxis
      dataKey="t"
      type="number"
      scale="time"
      domain={["dataMin", "dataMax"]}
      tickFormatter={(t: number) => tick.format(t)}
      tickLine={false}
      axisLine={false}
      minTickGap={32}
    />
  );
}

/** Throughput, latency and failures for one script over its window. */
export function SeriesCharts({
  points,
  step,
}: {
  points: ChartPoint[];
  step: MetricsStep;
}) {
  const { tick, full } = useTimeFormats(step);
  const per = stepNoun(step);
  const latencyTop = useMemo(() => latencyAxisTop(points), [points]);
  const latencyPoints = useMemo(
    () => withMaxOverflow(points, latencyTop),
    [points, latencyTop],
  );
  const maxClipped = latencyPoints.some((p) => p.maxOver != null);

  return (
    <div className="flex flex-col gap-6">
      <ChartSection title={`Runs per ${per}`}>
        <ChartContainer config={runsConfig} className="aspect-auto h-40 w-full">
          <BarChart data={points} margin={{ left: 4, right: 4, top: 8 }}>
            <CartesianGrid vertical={false} />
            {xAxis(tick)}
            <YAxis
              allowDecimals={false}
              tickLine={false}
              axisLine={false}
              width={48}
              tickFormatter={countFormat}
              label={{
                value: "runs",
                angle: -90,
                position: "insideLeft",
                className: "fill-muted-foreground",
              }}
            />
            <ChartTooltip
              cursor={false}
              content={
                <SeriesTooltip
                  config={runsConfig}
                  format={countFormat}
                  timeFormat={full}
                />
              }
            />
            <Bar dataKey="runs" fill="var(--color-runs)" radius={2} />
          </BarChart>
        </ChartContainer>
      </ChartSection>

      <ChartSection
        title="Run time"
        note={
          maxClipped
            ? "Where max is above this scale, it runs along the top edge; hover a step for its value."
            : null
        }
      >
        <ChartContainer
          config={latencyConfig}
          className="aspect-auto h-48 w-full"
        >
          <LineChart
            data={latencyPoints}
            margin={{ left: 4, right: 4, top: 6 }}
          >
            <CartesianGrid vertical={false} />
            {xAxis(tick)}
            <YAxis
              tickLine={false}
              axisLine={false}
              width={56}
              domain={[0, latencyTop ?? "auto"]}
              allowDataOverflow
              tickFormatter={(v: number) => formatMs(v)}
            />
            <ChartTooltip
              content={
                <SeriesTooltip
                  config={latencyConfig}
                  format={(v) => formatMs(v)}
                  timeFormat={full}
                  rowKeys={["p50", "p95", "max"]}
                />
              }
            />
            <ChartLegend content={<ChartLegendContent />} />
            <Line
              dataKey="p50"
              type="monotone"
              stroke="var(--color-p50)"
              strokeWidth={2}
              dot={points.length < 3}
              isAnimationActive={false}
            />
            <Line
              dataKey="p95"
              type="monotone"
              stroke="var(--color-p95)"
              strokeWidth={2}
              dot={points.length < 3}
              isAnimationActive={false}
            />
            <Line
              dataKey="maxShown"
              type="monotone"
              stroke="var(--color-maxShown)"
              strokeDasharray="4 3"
              strokeWidth={1}
              dot={false}
              activeDot={false}
              isAnimationActive={false}
            />
            <Line
              dataKey="maxOver"
              stroke="var(--color-max)"
              strokeWidth={2}
              legendType="none"
              dot={<OverflowTick />}
              activeDot={false}
              isAnimationActive={false}
            />
          </LineChart>
        </ChartContainer>
      </ChartSection>

      <ChartSection title={`Errors and timeouts per ${per}`}>
        <ChartContainer
          config={failuresConfig}
          className="aspect-auto h-40 w-full"
        >
          <BarChart data={points} margin={{ left: 4, right: 4, top: 8 }}>
            <CartesianGrid vertical={false} />
            {xAxis(tick)}
            <YAxis
              allowDecimals={false}
              domain={[0, (dataMax: number) => Math.max(dataMax, 4)]}
              interval={0}
              tickLine={false}
              axisLine={false}
              width={48}
              tickFormatter={countFormat}
              label={{
                value: "runs",
                angle: -90,
                position: "insideLeft",
                className: "fill-muted-foreground",
              }}
            />
            <ChartTooltip
              cursor={false}
              content={
                <SeriesTooltip
                  config={failuresConfig}
                  format={countFormat}
                  timeFormat={full}
                />
              }
            />
            <ChartLegend content={<ChartLegendContent />} />
            <Bar
              dataKey="errors"
              stackId="failures"
              fill="var(--color-errors)"
            />
            <Bar
              dataKey="timeouts"
              stackId="failures"
              fill="var(--color-timeouts)"
              radius={[2, 2, 0, 0]}
            />
          </BarChart>
        </ChartContainer>
      </ChartSection>
    </div>
  );
}
